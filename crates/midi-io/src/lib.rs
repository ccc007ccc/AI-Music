//! Standard MIDI file import/export for the internal tick-based project.

use midly::{
    Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind,
    num::{u4, u7, u15, u24, u28},
};
use music_core::{
    Clip, ControlEvent, DEFAULT_BPM, MixerSettings, NoteEvent, Project, TempoMap, TempoPoint,
    TimeSignature, Track, TrackSource, new_id,
};
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MidiError {
    #[error("MIDI I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("MIDI parse failed: {0}")]
    Parse(#[from] midly::Error),
    #[error("MIDI timing is not metrical")]
    UnsupportedTiming,
    #[error("MIDI PPQ must fit in 15 bits")]
    InvalidPpq,
}

pub fn export_project(project: &Project, path: impl AsRef<Path>) -> Result<(), MidiError> {
    let bytes = export_project_bytes(project)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

pub fn export_project_bytes(project: &Project) -> Result<Vec<u8>, MidiError> {
    if project.ppq == 0 || project.ppq > 0x7fff {
        return Err(MidiError::InvalidPpq);
    }
    let mut tracks = Vec::new();
    tracks.push(tempo_track(project));

    for (channel_index, track) in project.tracks.iter().enumerate() {
        let TrackSource::Midi { clips, .. } = &track.source else {
            continue;
        };
        let channel = u4::new((channel_index % 16) as u8);
        let mut events = Vec::new();
        events.push(AbsoluteEvent {
            tick: 0,
            priority: 0,
            kind: TrackEventKind::Meta(MetaMessage::TrackName(track.name.as_bytes())),
        });
        for clip in clips {
            for note in &clip.notes {
                let start = clip.start_tick + note.start_tick;
                let end = start + note.duration_tick;
                events.push(AbsoluteEvent {
                    tick: start,
                    priority: 2,
                    kind: TrackEventKind::Midi {
                        channel,
                        message: MidiMessage::NoteOn {
                            key: u7::new(note.pitch),
                            vel: u7::new(note.velocity),
                        },
                    },
                });
                events.push(AbsoluteEvent {
                    tick: end,
                    priority: 1,
                    kind: TrackEventKind::Midi {
                        channel,
                        message: MidiMessage::NoteOff {
                            key: u7::new(note.pitch),
                            vel: u7::new(0),
                        },
                    },
                });
            }
            for control in &clip.controls {
                events.push(AbsoluteEvent {
                    tick: clip.start_tick + control.tick,
                    priority: 0,
                    kind: TrackEventKind::Midi {
                        channel,
                        message: MidiMessage::Controller {
                            controller: u7::new(control.controller),
                            value: u7::new(control.value),
                        },
                    },
                });
            }
        }
        events.sort_by_key(|event| (event.tick, event.priority));
        tracks.push(with_deltas(events));
    }

    let smf = Smf {
        header: Header {
            format: Format::Parallel,
            timing: Timing::Metrical(u15::new(project.ppq)),
        },
        tracks,
    };
    let mut bytes = Vec::new();
    smf.write_std(&mut bytes)?;
    Ok(bytes)
}

fn tempo_track(project: &Project) -> Vec<TrackEvent<'_>> {
    let mut events = Vec::new();
    for point in &project.tempo_map.points {
        let micros = (60_000_000.0 / point.bpm).round().clamp(1.0, 16_777_215.0) as u32;
        events.push(AbsoluteEvent {
            tick: point.tick,
            priority: 0,
            kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(micros))),
        });
    }
    events.push(AbsoluteEvent {
        tick: 0,
        priority: 0,
        kind: TrackEventKind::Meta(MetaMessage::TimeSignature(
            project.time_signature.numerator,
            denominator_power(project.time_signature.denominator),
            24,
            8,
        )),
    });
    events.push(AbsoluteEvent {
        tick: project.duration_tick(),
        priority: 9,
        kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
    });
    events.sort_by_key(|event| (event.tick, event.priority));
    with_deltas(events)
}

fn denominator_power(denominator: u8) -> u8 {
    match denominator {
        1 => 0,
        2 => 1,
        4 => 2,
        8 => 3,
        16 => 4,
        32 => 5,
        _ => 2,
    }
}

struct AbsoluteEvent<'a> {
    tick: i64,
    priority: u8,
    kind: TrackEventKind<'a>,
}

fn with_deltas<'a>(mut events: Vec<AbsoluteEvent<'a>>) -> Vec<TrackEvent<'a>> {
    events.sort_by_key(|event| (event.tick, event.priority));
    let mut previous = 0_i64;
    let mut result = Vec::with_capacity(events.len() + 1);
    for event in events {
        let delta = (event.tick - previous).max(0) as u32;
        previous = event.tick.max(previous);
        result.push(TrackEvent {
            delta: u28::new(delta),
            kind: event.kind,
        });
    }
    if !matches!(
        result.last().map(|event| &event.kind),
        Some(TrackEventKind::Meta(MetaMessage::EndOfTrack))
    ) {
        result.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
    }
    result
}

pub fn import_midi(path: impl AsRef<Path>) -> Result<Project, MidiError> {
    let bytes = std::fs::read(path)?;
    import_midi_bytes(&bytes)
}

pub fn import_midi_bytes(bytes: &[u8]) -> Result<Project, MidiError> {
    let smf = Smf::parse(bytes)?;
    let ppq = match smf.header.timing {
        Timing::Metrical(value) => value.as_int(),
        Timing::Timecode(_, _) => return Err(MidiError::UnsupportedTiming),
    };
    let mut tempo_points = Vec::new();
    let mut time_signature = TimeSignature::default();
    let mut tracks = Vec::new();

    for (track_index, source_track) in smf.tracks.iter().enumerate() {
        let mut absolute_tick = 0_i64;
        let mut name = format!("Track {}", track_index + 1);
        let mut active: HashMap<(u8, u8), Vec<(i64, u8)>> = HashMap::new();
        let mut notes = Vec::new();
        let mut controls = Vec::new();
        for event in source_track {
            absolute_tick += event.delta.as_int() as i64;
            match event.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(value)) => {
                    let micros = value.as_int().max(1) as f64;
                    tempo_points.push(TempoPoint {
                        tick: absolute_tick,
                        bpm: 60_000_000.0 / micros,
                    });
                }
                TrackEventKind::Meta(MetaMessage::TimeSignature(numerator, denominator, _, _)) => {
                    time_signature = TimeSignature {
                        numerator,
                        denominator: 2_u8.saturating_pow(denominator as u32),
                    };
                }
                TrackEventKind::Meta(MetaMessage::TrackName(value)) => {
                    name = String::from_utf8_lossy(value).into_owned();
                }
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOn { key, vel },
                } if vel.as_int() > 0 => {
                    active
                        .entry((channel.as_int(), key.as_int()))
                        .or_default()
                        .push((absolute_tick, vel.as_int()));
                }
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOff { key, .. },
                } => {
                    if let Some(starts) = active.get_mut(&(channel.as_int(), key.as_int()))
                        && let Some((start, velocity)) = starts.pop()
                    {
                        notes.push(NoteEvent {
                            id: new_id("note"),
                            start_tick: start,
                            duration_tick: (absolute_tick - start).max(1),
                            pitch: key.as_int(),
                            velocity,
                        });
                    }
                }
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOn { key, vel },
                } if vel.as_int() == 0 => {
                    if let Some(starts) = active.get_mut(&(channel.as_int(), key.as_int()))
                        && let Some((start, velocity)) = starts.pop()
                    {
                        notes.push(NoteEvent {
                            id: new_id("note"),
                            start_tick: start,
                            duration_tick: (absolute_tick - start).max(1),
                            pitch: key.as_int(),
                            velocity,
                        });
                    }
                }
                TrackEventKind::Midi {
                    channel: _,
                    message: MidiMessage::Controller { controller, value },
                } => {
                    controls.push(ControlEvent {
                        id: new_id("control"),
                        tick: absolute_tick,
                        controller: controller.as_int(),
                        value: value.as_int(),
                    });
                }
                _ => {}
            }
        }
        if notes.is_empty() && controls.is_empty() {
            continue;
        }
        notes.sort_by_key(|note| note.start_tick);
        let note_length = notes
            .iter()
            .map(|note| note.start_tick + note.duration_tick)
            .max()
            .unwrap_or(0);
        let control_length = controls
            .iter()
            .map(|control| control.tick)
            .max()
            .unwrap_or(0);
        let length = note_length.max(control_length).max(ppq as i64 * 4);
        tracks.push(Track {
            id: format!("track-{}", tracks.len()),
            name,
            source: TrackSource::Midi {
                instrument: "piano".to_owned(),
                clips: vec![Clip {
                    id: format!("clip-{}", tracks.len()),
                    start_tick: 0,
                    length_tick: length,
                    notes,
                    controls,
                }],
            },
            mixer: MixerSettings::default(),
        });
    }

    if tempo_points.is_empty() {
        tempo_points.push(TempoPoint {
            tick: 0,
            bpm: DEFAULT_BPM,
        });
    }
    tempo_points.sort_by_key(|point| point.tick);
    // MIDI permits multiple tempo meta events at one absolute tick.  The
    // Project IR has one tempo point per tick, so retain the last event in
    // file order as the deterministic effective value.
    let mut normalized_tempo_points: Vec<TempoPoint> = Vec::with_capacity(tempo_points.len());
    for point in tempo_points {
        if let Some(previous) = normalized_tempo_points.last_mut()
            && previous.tick == point.tick
        {
            *previous = point;
        } else {
            normalized_tempo_points.push(point);
        }
    }
    let mut tempo_points = normalized_tempo_points;
    if tempo_points[0].tick != 0 {
        tempo_points.insert(
            0,
            TempoPoint {
                tick: 0,
                bpm: tempo_points[0].bpm,
            },
        );
    }
    let mut project = Project {
        schema_version: 1,
        revision: 0,
        ppq,
        tempo_map: TempoMap {
            points: tempo_points,
        },
        time_signature,
        tracks,
    };
    if project.tracks.is_empty() {
        // Keep timing metadata from an otherwise empty MIDI file while using
        // the standard piano track as an editable destination.
        let default_project = Project::default();
        project.tracks = default_project.tracks;
    }
    Ok(project)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_round_trips_through_standard_midi() {
        let mut source = Project::demo();
        let TrackSource::Midi { clips, .. } = &mut source.tracks[0].source else {
            panic!("demo track is not MIDI")
        };
        clips[0].controls.push(ControlEvent {
            id: "pedal".to_owned(),
            tick: 0,
            controller: 64,
            value: 127,
        });
        let bytes = export_project_bytes(&source).unwrap();
        assert!(bytes.starts_with(b"MThd"));

        let imported = import_midi_bytes(&bytes).unwrap();
        assert_eq!(imported.ppq, source.ppq);
        assert_eq!(imported.time_signature, source.time_signature);
        assert_eq!(
            imported.scheduled_notes().len(),
            source.scheduled_notes().len()
        );
        let TrackSource::Midi { clips, .. } = &imported.tracks[0].source else {
            panic!("imported track is not MIDI")
        };
        assert_eq!(clips[0].controls.len(), 1);
        assert_eq!(imported.duration_tick(), source.duration_tick());
        assert!((imported.tempo_map.points[0].bpm - DEFAULT_BPM).abs() < 0.001);
    }

    #[test]
    fn empty_midi_import_gets_a_default_piano_track() {
        let source = Project::default();
        let bytes = export_project_bytes(&source).unwrap();
        let imported = import_midi_bytes(&bytes).unwrap();
        assert_eq!(imported.tracks.len(), 1);
        assert_eq!(imported.scheduled_notes().len(), 0);
    }

    #[test]
    fn duplicate_tempo_events_at_one_tick_are_normalized_on_import() {
        let track = vec![
            TrackEvent {
                delta: u28::new(0),
                kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(500_000))),
            },
            TrackEvent {
                delta: u28::new(0),
                kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(400_000))),
            },
            TrackEvent {
                delta: u28::new(0),
                kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
            },
        ];
        let smf = Smf {
            header: Header {
                format: Format::SingleTrack,
                timing: Timing::Metrical(u15::new(960)),
            },
            tracks: vec![track],
        };
        let mut bytes = Vec::new();
        smf.write_std(&mut bytes).unwrap();

        let imported = import_midi_bytes(&bytes).unwrap();
        assert_eq!(imported.tempo_map.points.len(), 1);
        assert_eq!(imported.tempo_map.points[0].bpm, 150.0);
        imported.validate().unwrap();
    }
}
