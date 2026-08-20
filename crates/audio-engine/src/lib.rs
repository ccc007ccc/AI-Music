//! MIDI-track rendering, instrument sessions, mixing, and audio export.
//!
//! Every MIDI track gets a stateful [`InstrumentSession`].  The scheduler
//! sends timestamped note events to that session and asks it to render blocks.
//! This preserves polyphony, pedal state, resonance, and effects across notes
//! while keeping the project model independent from any concrete synthesizer.

mod asset_pack;
mod piano;
mod player;
#[cfg(feature = "rustysynth-backend")]
mod rustysynth_piano;
#[cfg(feature = "sfz-backend")]
mod sfz_piano;
#[cfg(feature = "sfz-backend")]
mod sfz_preprocess;

use music_core::{Project, TrackSource};
use std::collections::HashMap;
use std::io::{Cursor, Seek, Write};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;

pub use asset_pack::{
    ASSET_PACK_SCHEMA_VERSION, AssetLicense, AssetPack, AssetPackEngine, AssetPackError,
    AssetPackManifest,
};
pub use piano::PianoSynth;
pub use player::{PlaybackHandle, PlayerError, play_buffer};
#[cfg(feature = "rustysynth-backend")]
pub use rustysynth_piano::{RustySynthError, RustySynthPiano};
#[cfg(feature = "sfz-backend")]
pub use sfz_piano::{SfzPiano, SfzPianoError};

pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;
pub const DEFAULT_CHANNELS: usize = 2;
const RENDER_BLOCK_FRAMES: usize = 256;

#[derive(Clone, Debug)]
pub struct AudioBuffer {
    pub sample_rate: u32,
    pub channels: usize,
    pub samples: Vec<f32>,
}

impl AudioBuffer {
    pub fn new(sample_rate: u32, channels: usize, frames: usize) -> Self {
        Self {
            sample_rate,
            channels,
            samples: vec![0.0; frames.saturating_mul(channels)],
        }
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }

    pub fn add_stereo(&mut self, frame: usize, left: f32, right: f32) {
        if self.channels < 2 {
            return;
        }
        let index = frame.saturating_mul(self.channels);
        if index + 1 < self.samples.len() {
            self.samples[index] += left;
            self.samples[index + 1] += right;
        }
    }

    pub fn peak(&self) -> f32 {
        self.samples
            .iter()
            .map(|value| value.abs())
            .fold(0.0_f32, f32::max)
    }

    pub fn normalize(&mut self, target_peak: f32) {
        let peak = self.peak();
        if peak > target_peak && peak > 0.0 {
            let scale = target_peak / peak;
            for sample in &mut self.samples {
                *sample *= scale;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstrumentEvent {
    NoteOn { pitch: u8, velocity: u8 },
    NoteOff { pitch: u8 },
    ControlChange { controller: u8, value: u8 },
}

#[derive(Debug, Error)]
pub enum InstrumentError {
    #[error("{0}")]
    Backend(String),
}

#[derive(Debug, Error)]
pub enum AssetInstrumentError {
    #[error("asset engine {engine:?} is disabled; rebuild with feature '{feature}'")]
    BackendDisabled {
        engine: AssetPackEngine,
        feature: &'static str,
    },
    #[error("could not load {engine:?} instrument from {path:?}: {message}")]
    Load {
        engine: AssetPackEngine,
        path: std::path::PathBuf,
        message: String,
    },
}

/// Factory interface at the instrument seam.
///
/// Implementations hold immutable assets/configuration.  Each track receives
/// its own mutable session so separate tracks never share MIDI or effect state.
pub trait Instrument: Send + Sync {
    fn create_session(
        &self,
        sample_rate: u32,
    ) -> Result<Box<dyn InstrumentSession>, InstrumentError>;

    fn tail_seconds(&self) -> f32 {
        3.0
    }
}

/// Stateful render interface used by both offline and future realtime engines.
/// `render` must overwrite both output slices and keep internal state between
/// calls.  Both slices always have the same length.
pub trait InstrumentSession {
    fn send_event(&mut self, event: InstrumentEvent);
    fn render(&mut self, left: &mut [f32], right: &mut [f32]);
}

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("sample rate must be between 16000 and 192000 Hz")]
    InvalidSampleRate,
    #[error("no instrument registered for '{0}'")]
    MissingInstrument(String),
    #[error("instrument '{instrument}' could not start: {message}")]
    Instrument { instrument: String, message: String },
    #[error("WAV export failed: {0}")]
    Wav(#[from] hound::Error),
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct InstrumentDescriptor {
    pub id: String,
    pub name: String,
}

#[derive(Clone)]
struct RegisteredInstrument {
    descriptor: InstrumentDescriptor,
    instrument: Arc<dyn Instrument>,
}

#[derive(Clone)]
pub struct InstrumentRack {
    instruments: HashMap<String, RegisteredInstrument>,
}

impl InstrumentRack {
    pub fn new() -> Self {
        Self {
            instruments: HashMap::new(),
        }
    }

    pub fn with_piano() -> Self {
        let mut rack = Self::new();
        rack.register_named("piano", "Piano", Arc::new(PianoSynth::default()));
        rack
    }

    pub fn register(&mut self, id: impl Into<String>, instrument: Arc<dyn Instrument>) {
        let id = id.into();
        self.register_named(id.clone(), id, instrument);
    }

    pub fn register_named(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        instrument: Arc<dyn Instrument>,
    ) {
        let id = id.into();
        self.instruments.insert(
            id.clone(),
            RegisteredInstrument {
                descriptor: InstrumentDescriptor {
                    id,
                    name: name.into(),
                },
                instrument,
            },
        );
    }

    pub fn get(&self, id: &str) -> Option<&Arc<dyn Instrument>> {
        self.instruments
            .get(id)
            .map(|registration| &registration.instrument)
    }

    pub fn catalog(&self) -> Vec<InstrumentDescriptor> {
        let mut catalog: Vec<_> = self
            .instruments
            .values()
            .map(|registration| registration.descriptor.clone())
            .collect();
        catalog.sort_by(|left, right| left.id.cmp(&right.id));
        catalog
    }

    /// Builds a one-instrument rack from a validated, licensed asset pack.
    /// Backend selection stays inside the audio engine so callers do not need
    /// to know whether the pack contains SF2 or SFZ data.
    #[cfg(any(feature = "rustysynth-backend", feature = "sfz-backend"))]
    pub fn from_asset_pack(pack: &AssetPack) -> Result<Self, AssetInstrumentError> {
        let engine = pack.manifest().engine;
        let instrument: Arc<dyn Instrument> =
            match engine {
                AssetPackEngine::SoundFont2 => {
                    #[cfg(feature = "rustysynth-backend")]
                    {
                        Arc::new(RustySynthPiano::from_path(pack.entry_path()).map_err(
                            |error| AssetInstrumentError::Load {
                                engine,
                                path: pack.entry_path().to_owned(),
                                message: error.to_string(),
                            },
                        )?)
                    }
                    #[cfg(not(feature = "rustysynth-backend"))]
                    {
                        return Err(AssetInstrumentError::BackendDisabled {
                            engine,
                            feature: "rustysynth-backend",
                        });
                    }
                }
                AssetPackEngine::Sfz => {
                    #[cfg(feature = "sfz-backend")]
                    {
                        Arc::new(SfzPiano::from_asset_pack(pack).map_err(|error| {
                            AssetInstrumentError::Load {
                                engine,
                                path: pack.entry_path().to_owned(),
                                message: error.to_string(),
                            }
                        })?)
                    }
                    #[cfg(not(feature = "sfz-backend"))]
                    {
                        return Err(AssetInstrumentError::BackendDisabled {
                            engine,
                            feature: "sfz-backend",
                        });
                    }
                }
            };
        let mut rack = Self::new();
        rack.register_named(
            pack.manifest().instrument_id.clone(),
            pack.manifest().name.clone(),
            instrument,
        );
        Ok(rack)
    }

    /// Builds an asset-backed rack optimized for one renderable project.
    /// SFZ packs decode only the pitch/velocity layers the project can reach;
    /// other backends retain their normal loading behavior.
    #[cfg(any(feature = "rustysynth-backend", feature = "sfz-backend"))]
    pub fn from_asset_pack_for_project(
        pack: &AssetPack,
        project: &Project,
    ) -> Result<Self, AssetInstrumentError> {
        if pack.manifest().engine != AssetPackEngine::Sfz {
            return Self::from_asset_pack(pack);
        }
        #[cfg(feature = "sfz-backend")]
        {
            let notes: Vec<_> = project
                .scheduled_notes()
                .into_iter()
                .filter(|note| note.instrument == pack.manifest().instrument_id)
                .map(|note| (note.pitch, note.velocity))
                .collect();
            let instrument: Arc<dyn Instrument> = Arc::new(
                SfzPiano::from_asset_pack_for_performance(pack, &notes).map_err(|error| {
                    AssetInstrumentError::Load {
                        engine: pack.manifest().engine,
                        path: pack.entry_path().to_owned(),
                        message: error.to_string(),
                    }
                })?,
            );
            let mut rack = Self::new();
            rack.register_named(
                pack.manifest().instrument_id.clone(),
                pack.manifest().name.clone(),
                instrument,
            );
            Ok(rack)
        }
        #[cfg(not(feature = "sfz-backend"))]
        {
            Err(AssetInstrumentError::BackendDisabled {
                engine: pack.manifest().engine,
                feature: "sfz-backend",
            })
        }
    }

    /// Reports the feature required by a pack when no asset backend is built.
    #[cfg(not(any(feature = "rustysynth-backend", feature = "sfz-backend")))]
    pub fn from_asset_pack(pack: &AssetPack) -> Result<Self, AssetInstrumentError> {
        let engine = pack.manifest().engine;
        let feature = match engine {
            AssetPackEngine::SoundFont2 => "rustysynth-backend",
            AssetPackEngine::Sfz => "sfz-backend",
        };
        Err(AssetInstrumentError::BackendDisabled { engine, feature })
    }

    #[cfg(not(any(feature = "rustysynth-backend", feature = "sfz-backend")))]
    pub fn from_asset_pack_for_project(
        pack: &AssetPack,
        _project: &Project,
    ) -> Result<Self, AssetInstrumentError> {
        Self::from_asset_pack(pack)
    }
}

impl Default for InstrumentRack {
    fn default() -> Self {
        Self::with_piano()
    }
}

#[derive(Clone, Copy, Debug)]
struct ScheduledEvent {
    frame: usize,
    priority: u8,
    event: InstrumentEvent,
}

pub fn render_project(project: &Project, sample_rate: u32) -> Result<AudioBuffer, RenderError> {
    render_project_with_rack(project, sample_rate, &InstrumentRack::default())
}

pub fn render_project_with_rack(
    project: &Project,
    sample_rate: u32,
    rack: &InstrumentRack,
) -> Result<AudioBuffer, RenderError> {
    if !(16_000..=192_000).contains(&sample_rate) {
        return Err(RenderError::InvalidSampleRate);
    }

    let has_solo = project.tracks.iter().any(|track| track.mixer.solo);
    let mut tail_seconds = 0.5_f32;
    for track in &project.tracks {
        if track.mixer.mute || (has_solo && !track.mixer.solo) {
            continue;
        }
        if let TrackSource::Midi { instrument, .. } = &track.source {
            let backend = rack
                .get(instrument)
                .ok_or_else(|| RenderError::MissingInstrument(instrument.clone()))?;
            tail_seconds = tail_seconds.max(backend.tail_seconds());
        }
    }

    let project_seconds = project
        .tempo_map
        .seconds_at(project.duration_tick(), project.ppq);
    let frames = ((project_seconds + tail_seconds as f64) * sample_rate as f64).ceil() as usize;
    let mut output = AudioBuffer::new(sample_rate, DEFAULT_CHANNELS, frames.max(1));

    for track in &project.tracks {
        if track.mixer.mute || (has_solo && !track.mixer.solo) {
            continue;
        }
        let TrackSource::Midi { instrument, clips } = &track.source else {
            // Raw audio clips have a stable project representation but their
            // decoder/mixer adapter is intentionally deferred beyond the
            // single-piano MVP.
            continue;
        };
        let backend = rack
            .get(instrument)
            .ok_or_else(|| RenderError::MissingInstrument(instrument.clone()))?;
        let mut session =
            backend
                .create_session(sample_rate)
                .map_err(|error| RenderError::Instrument {
                    instrument: instrument.clone(),
                    message: error.to_string(),
                })?;

        let mut events = Vec::new();
        for clip in clips {
            for note in &clip.notes {
                let start_tick = clip.start_tick + note.start_tick;
                let end_tick = start_tick + note.duration_tick;
                events.push(ScheduledEvent {
                    frame: project
                        .tempo_map
                        .ticks_to_samples(start_tick, project.ppq, sample_rate)
                        as usize,
                    priority: 2,
                    event: InstrumentEvent::NoteOn {
                        pitch: note.pitch,
                        velocity: note.velocity,
                    },
                });
                events.push(ScheduledEvent {
                    frame: project
                        .tempo_map
                        .ticks_to_samples(end_tick, project.ppq, sample_rate)
                        as usize,
                    // Release before retriggering the same pitch at one frame.
                    priority: 1,
                    event: InstrumentEvent::NoteOff { pitch: note.pitch },
                });
            }
            for control in &clip.controls {
                events.push(ScheduledEvent {
                    frame: project.tempo_map.ticks_to_samples(
                        clip.start_tick + control.tick,
                        project.ppq,
                        sample_rate,
                    ) as usize,
                    priority: 0,
                    event: InstrumentEvent::ControlChange {
                        controller: control.controller,
                        value: control.value,
                    },
                });
            }
        }
        events.sort_by_key(|event| (event.frame, event.priority));

        let gain = 10.0_f32.powf(track.mixer.gain_db / 20.0);
        let pan = track.mixer.pan.clamp(-1.0, 1.0);
        let left_gain = gain * if pan > 0.0 { (1.0 - pan).sqrt() } else { 1.0 };
        let right_gain = gain * if pan < 0.0 { (1.0 + pan).sqrt() } else { 1.0 };
        render_track(
            session.as_mut(),
            &events,
            &mut output,
            left_gain,
            right_gain,
        );
    }

    output.normalize(0.96);
    Ok(output)
}

fn render_track(
    session: &mut dyn InstrumentSession,
    events: &[ScheduledEvent],
    output: &mut AudioBuffer,
    left_gain: f32,
    right_gain: f32,
) {
    let mut cursor = 0_usize;
    let mut event_index = 0_usize;
    let mut left = vec![0.0_f32; RENDER_BLOCK_FRAMES];
    let mut right = vec![0.0_f32; RENDER_BLOCK_FRAMES];

    while cursor < output.frames() {
        while event_index < events.len() && events[event_index].frame <= cursor {
            session.send_event(events[event_index].event);
            event_index += 1;
        }

        let next_event = events
            .get(event_index)
            .map(|event| event.frame)
            .unwrap_or(output.frames());
        let frames = RENDER_BLOCK_FRAMES
            .min(output.frames() - cursor)
            .min(next_event.saturating_sub(cursor));

        if frames == 0 {
            continue;
        }
        left[..frames].fill(0.0);
        right[..frames].fill(0.0);
        session.render(&mut left[..frames], &mut right[..frames]);
        for offset in 0..frames {
            output.add_stereo(
                cursor + offset,
                left[offset] * left_gain,
                right[offset] * right_gain,
            );
        }
        cursor += frames;
    }
}

pub fn render_wav(
    project: &Project,
    sample_rate: u32,
    path: impl AsRef<Path>,
) -> Result<(), RenderError> {
    let buffer = render_project(project, sample_rate)?;
    write_wav(&buffer, path)
}

pub fn write_wav(buffer: &AudioBuffer, path: impl AsRef<Path>) -> Result<(), RenderError> {
    let mut writer = hound::WavWriter::create(path, wav_spec(buffer))?;
    write_wav_samples(buffer, &mut writer)?;
    writer.finalize()?;
    Ok(())
}

/// Encode a rendered buffer for transactional package persistence.
pub fn wav_bytes(buffer: &AudioBuffer) -> Result<Vec<u8>, RenderError> {
    let mut bytes = Vec::new();
    {
        let cursor = Cursor::new(&mut bytes);
        let mut writer = hound::WavWriter::new(cursor, wav_spec(buffer))?;
        write_wav_samples(buffer, &mut writer)?;
        writer.finalize()?;
    }
    Ok(bytes)
}

fn wav_spec(buffer: &AudioBuffer) -> hound::WavSpec {
    hound::WavSpec {
        channels: buffer.channels as u16,
        sample_rate: buffer.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    }
}

fn write_wav_samples<W: Write + Seek>(
    buffer: &AudioBuffer,
    writer: &mut hound::WavWriter<W>,
) -> Result<(), RenderError> {
    for sample in &buffer.samples {
        writer.write_sample(sample.clamp(-1.0, 1.0))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_core::{Command, NoteEvent, Project, ProjectEngine};

    #[test]
    fn demo_project_renders_non_silent_audio() {
        let buffer = render_project(&Project::demo(), 16_000).unwrap();
        assert!(buffer.frames() > 0);
        assert!(buffer.peak() > 0.01);
    }

    #[test]
    fn wav_can_be_encoded_to_memory_for_atomic_package_writes() {
        let buffer = render_project(&Project::demo(), 16_000).unwrap();
        let bytes = wav_bytes(&buffer).unwrap();
        assert!(bytes.starts_with(b"RIFF"));
        assert!(bytes.len() > 44);
    }

    #[test]
    fn instrument_catalog_describes_registered_renderers_in_stable_order() {
        let mut rack = InstrumentRack::with_piano();
        rack.register_named(
            "piano-bright",
            "Bright Piano",
            Arc::new(PianoSynth {
                brightness: 1.1,
                ..PianoSynth::default()
            }),
        );

        assert_eq!(
            rack.catalog(),
            vec![
                InstrumentDescriptor {
                    id: "piano".to_owned(),
                    name: "Piano".to_owned(),
                },
                InstrumentDescriptor {
                    id: "piano-bright".to_owned(),
                    name: "Bright Piano".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn unknown_instrument_is_reported() {
        let mut project = Project::demo();
        if let TrackSource::Midi { instrument, .. } = &mut project.tracks[0].source {
            *instrument = "missing".to_owned();
        }
        assert!(matches!(
            render_project(&project, 48_000),
            Err(RenderError::MissingInstrument(id)) if id == "missing"
        ));
    }

    #[test]
    fn multi_track_solo_and_pan_are_applied_by_the_mixer() {
        let mut engine = ProjectEngine::new(Project::default());
        engine
            .apply(Command::AddNote {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                note: NoteEvent {
                    id: "left-track-note".to_owned(),
                    start_tick: 0,
                    duration_tick: 960,
                    pitch: 60,
                    velocity: 90,
                },
            })
            .unwrap();
        engine
            .apply(Command::CreateTrack {
                track_id: "piano-2".to_owned(),
                name: "Piano 2".to_owned(),
                instrument: "piano".to_owned(),
            })
            .unwrap();
        engine
            .apply(Command::AddNote {
                track_id: "piano-2".to_owned(),
                clip_id: "piano-2-main".to_owned(),
                note: NoteEvent {
                    id: "solo-note".to_owned(),
                    start_tick: 0,
                    duration_tick: 960,
                    pitch: 67,
                    velocity: 100,
                },
            })
            .unwrap();
        engine
            .apply(Command::SetTrackMixer {
                track_id: "piano-2".to_owned(),
                gain_db: 0.0,
                pan: 1.0,
                mute: false,
                solo: true,
            })
            .unwrap();

        let buffer = render_project(engine.project(), 16_000).unwrap();
        let left_energy: f32 = buffer
            .samples
            .iter()
            .step_by(2)
            .map(|value| value * value)
            .sum();
        let right_energy: f32 = buffer
            .samples
            .iter()
            .skip(1)
            .step_by(2)
            .map(|value| value * value)
            .sum();
        assert!(left_energy < 1.0e-10);
        assert!(right_energy > 0.01);
    }
}
