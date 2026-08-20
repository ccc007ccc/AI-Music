//! Musical project model and transactional editing commands.
//!
//! The rest of the application talks to this crate instead of mutating the
//! project directly.  That keeps the CLI, GUI, and AI composition adapter on the
//! same seam and gives us one place for validation and undo/redo.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
use thiserror::Error;
use uuid::Uuid;

pub type Tick = i64;
pub const DEFAULT_PPQ: u16 = 960;
pub const DEFAULT_BPM: f64 = 120.0;

pub type TrackId = String;
pub type ClipId = String;
pub type NoteId = String;
pub type InstrumentId = String;

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4().simple())
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Project {
    pub schema_version: u32,
    /// Monotonic edit revision persisted with the project.  It is used by AI
    /// patches to detect stale edits after a project has been reopened.
    #[serde(default)]
    pub revision: u64,
    pub ppq: u16,
    pub tempo_map: TempoMap,
    pub time_signature: TimeSignature,
    pub tracks: Vec<Track>,
}

impl Default for Project {
    fn default() -> Self {
        let track_id = "piano".to_owned();
        let clip_id = "piano-main".to_owned();
        Self {
            schema_version: 1,
            revision: 0,
            ppq: DEFAULT_PPQ,
            tempo_map: TempoMap::default(),
            time_signature: TimeSignature::default(),
            tracks: vec![Track {
                id: track_id,
                name: "Piano".to_owned(),
                source: TrackSource::Midi {
                    instrument: "piano".to_owned(),
                    clips: vec![Clip {
                        id: clip_id,
                        start_tick: 0,
                        length_tick: DEFAULT_PPQ as Tick * 16,
                        notes: Vec::new(),
                        controls: Vec::new(),
                    }],
                },
                mixer: MixerSettings::default(),
            }],
        }
    }
}

impl Project {
    pub fn demo() -> Self {
        let mut project = Self::default();
        let chord_progression = [
            [60_u8, 64, 67], // C
            [57, 60, 64],    // A minor
            [53, 57, 60],    // F
            [55, 59, 62],    // G
        ];
        let beat = project.ppq as Tick;
        let bar = beat * 4;
        if let Some(clip) = project.midi_clip_mut("piano", "piano-main") {
            for (bar_index, chord) in chord_progression.iter().enumerate() {
                let start = bar_index as Tick * bar;
                for (index, pitch) in chord.iter().enumerate() {
                    clip.notes.push(NoteEvent {
                        id: format!("demo-{bar_index}-{index}"),
                        start_tick: start,
                        duration_tick: bar,
                        pitch: *pitch,
                        velocity: 70 + (index as u8 * 6),
                    });
                }
            }
        }
        project
    }

    pub fn midi_clip(&self, track_id: &str, clip_id: &str) -> Option<&Clip> {
        self.track(track_id).and_then(|track| {
            if let TrackSource::Midi { clips, .. } = &track.source {
                clips.iter().find(|clip| clip.id == clip_id)
            } else {
                None
            }
        })
    }

    pub fn midi_clip_mut(&mut self, track_id: &str, clip_id: &str) -> Option<&mut Clip> {
        self.track_mut(track_id).and_then(|track| {
            if let TrackSource::Midi { clips, .. } = &mut track.source {
                clips.iter_mut().find(|clip| clip.id == clip_id)
            } else {
                None
            }
        })
    }

    pub fn track(&self, track_id: &str) -> Option<&Track> {
        self.tracks.iter().find(|track| track.id == track_id)
    }

    pub fn track_mut(&mut self, track_id: &str) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|track| track.id == track_id)
    }

    pub fn midi_track(&self, track_id: &str) -> Option<&Track> {
        self.track(track_id)
            .filter(|track| matches!(track.source, TrackSource::Midi { .. }))
    }

    pub fn midi_track_mut(&mut self, track_id: &str) -> Option<&mut Track> {
        self.track_mut(track_id)
            .filter(|track| matches!(track.source, TrackSource::Midi { .. }))
    }

    pub fn scheduled_notes(&self) -> Vec<ScheduledNote> {
        let mut result = Vec::new();
        let has_solo = self.tracks.iter().any(|track| track.mixer.solo);
        for track in &self.tracks {
            if track.mixer.mute || (has_solo && !track.mixer.solo) {
                continue;
            }
            let TrackSource::Midi { instrument, clips } = &track.source else {
                continue;
            };
            for clip in clips {
                for note in &clip.notes {
                    result.push(ScheduledNote {
                        instrument: instrument.clone(),
                        start_tick: clip.start_tick + note.start_tick,
                        duration_tick: note.duration_tick,
                        pitch: note.pitch,
                        velocity: note.velocity,
                        gain_db: track.mixer.gain_db,
                        pan: track.mixer.pan,
                    });
                }
            }
        }
        result.sort_by_key(|note| note.start_tick);
        result
    }

    pub fn duration_tick(&self) -> Tick {
        self.tracks
            .iter()
            .map(|track| match &track.source {
                TrackSource::Midi { clips, .. } => clips
                    .iter()
                    .map(|clip| {
                        let note_end = clip
                            .notes
                            .iter()
                            .map(|note| note.start_tick + note.duration_tick)
                            .max()
                            .unwrap_or(0);
                        let control_end = clip
                            .controls
                            .iter()
                            .map(|control| control.tick)
                            .max()
                            .unwrap_or(0);
                        clip.start_tick + clip.length_tick.max(note_end).max(control_end)
                    })
                    .max()
                    .unwrap_or(0),
                TrackSource::Audio { clips } => clips
                    .iter()
                    .map(|clip| clip.start_tick + clip.length_tick)
                    .max()
                    .unwrap_or(0),
            })
            .max()
            .unwrap_or(0)
    }

    pub fn to_pretty_json(&self) -> Result<String, ProjectError> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(ProjectError::Serialization)
    }

    pub fn from_json(value: &str) -> Result<Self, ProjectError> {
        let project: Self = serde_json::from_str(value).map_err(ProjectError::Serialization)?;
        project.validate()?;
        Ok(project)
    }

    /// Validate a complete project loaded from an untrusted file or before a
    /// package save. Command application performs the same checks incrementally;
    /// this whole-project pass closes the direct-JSON escape hatch.
    pub fn validate(&self) -> Result<(), ProjectError> {
        if self.ppq == 0 {
            return Err(ProjectError::InvalidPpq);
        }
        self.time_signature.beat_length_tick(self.ppq)?;
        if self.tempo_map.points.is_empty()
            || self.tempo_map.points[0].tick != 0
            || self
                .tempo_map
                .points
                .windows(2)
                .any(|points| points[0].tick >= points[1].tick)
        {
            return Err(ProjectError::InvalidTempoMap);
        }
        for point in &self.tempo_map.points {
            if point.tick < 0 {
                return Err(ProjectError::InvalidTempoMap);
            }
            validate_bpm(point.bpm)?;
        }

        let mut track_ids = BTreeSet::new();
        for track in &self.tracks {
            validate_track_id(&track.id)?;
            if !track_ids.insert(track.id.as_str()) {
                return Err(ProjectError::DuplicateTrack(track.id.clone()));
            }
            if track.name.trim().is_empty() {
                return Err(ProjectError::InvalidTrackName);
            }
            if !track.mixer.gain_db.is_finite() || !(-96.0..=24.0).contains(&track.mixer.gain_db) {
                return Err(ProjectError::InvalidGain);
            }
            if !(-1.0..=1.0).contains(&track.mixer.pan) {
                return Err(ProjectError::InvalidPan);
            }
            match &track.source {
                TrackSource::Midi { instrument, clips } => {
                    normalized_instrument(instrument.clone())?;
                    let mut clip_ids = BTreeSet::new();
                    for clip in clips {
                        if !clip_ids.insert(clip.id.as_str()) {
                            return Err(ProjectError::DuplicateClip(clip.id.clone()));
                        }
                        if clip.start_tick < 0 {
                            return Err(ProjectError::InvalidTick(clip.start_tick));
                        }
                        validate_duration(clip.length_tick)?;
                        let mut note_ids = BTreeSet::new();
                        for note in &clip.notes {
                            if !note_ids.insert(note.id.as_str()) {
                                return Err(ProjectError::DuplicateNote(note.id.clone()));
                            }
                            validate_note(note)?;
                        }
                        let mut control_ids = BTreeSet::new();
                        for control in &clip.controls {
                            if !control_ids.insert(control.id.as_str()) {
                                return Err(ProjectError::DuplicateControl(control.id.clone()));
                            }
                            validate_control(control.tick, control.controller, control.value)?;
                        }
                    }
                }
                TrackSource::Audio { clips } => {
                    let mut clip_ids = BTreeSet::new();
                    for clip in clips {
                        if !clip_ids.insert(clip.id.as_str()) {
                            return Err(ProjectError::DuplicateClip(clip.id.clone()));
                        }
                        if clip.start_tick < 0 {
                            return Err(ProjectError::InvalidTick(clip.start_tick));
                        }
                        validate_duration(clip.length_tick)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<(), ProjectError> {
        let json = self.to_pretty_json()?;
        std::fs::write(path, json).map_err(ProjectError::Io)
    }

    pub fn load(path: &Path) -> Result<Self, ProjectError> {
        let json = std::fs::read_to_string(path).map_err(ProjectError::Io)?;
        Self::from_json(&json)
    }

    /// Returns a compact, stable description intended for AI planning and
    /// editor status views. It omits individual events while retaining the
    /// identifiers, timing ranges, counts, and pitch bounds needed to form a
    /// safe patch.
    pub fn summary(&self) -> ProjectSummary {
        ProjectSummary {
            schema_version: self.schema_version,
            revision: self.revision,
            ppq: self.ppq,
            tempo_map: self.tempo_map.clone(),
            time_signature: self.time_signature.clone(),
            duration_tick: self.duration_tick(),
            tracks: self.tracks.iter().map(TrackSummary::from_track).collect(),
        }
    }

    /// Returns MIDI events intersecting an absolute project-tick window.
    /// Notes include both local and absolute positions because edit commands
    /// use clip-local ticks while AI planning usually reasons on the timeline.
    pub fn clip_window(
        &self,
        track_id: &str,
        clip_id: &str,
        start_tick: Tick,
        end_tick: Tick,
    ) -> Result<ClipWindow, ProjectError> {
        if start_tick < 0 || end_tick <= start_tick {
            return Err(ProjectError::InvalidTickWindow {
                start: start_tick,
                end: end_tick,
            });
        }
        let track = self
            .track(track_id)
            .ok_or_else(|| ProjectError::TrackNotFound(track_id.to_owned()))?;
        let TrackSource::Midi { clips, .. } = &track.source else {
            return Err(ProjectError::NotMidiTrack);
        };
        let clip = clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .ok_or_else(|| ProjectError::ClipNotFound(clip_id.to_owned()))?;
        let beat_length_tick = self.time_signature.beat_length_tick(self.ppq)?;
        let bar_length_tick = self.time_signature.bar_length_tick(self.ppq)?;
        let mut notes = Vec::new();
        for note in &clip.notes {
            let absolute_start_tick = clip.start_tick + note.start_tick;
            let absolute_end_tick = absolute_start_tick + note.duration_tick;
            if absolute_start_tick < end_tick && absolute_end_tick > start_tick {
                let duration_quarters = TickRatio::new(note.duration_tick, self.ppq as Tick);
                notes.push(WindowNote {
                    id: note.id.clone(),
                    local_start_tick: note.start_tick,
                    absolute_start_tick,
                    start_position: self
                        .time_signature
                        .musical_position(self.ppq, absolute_start_tick)?,
                    end_position: self
                        .time_signature
                        .musical_position(self.ppq, absolute_end_tick)?,
                    duration_tick: note.duration_tick,
                    duration_quarters,
                    common_duration: CommonDuration::from_ratio(duration_quarters),
                    pitch: note.pitch,
                    pitch_name: midi_note_name(note.pitch),
                    velocity: note.velocity,
                });
            }
        }
        notes.sort_by_key(|note| (note.absolute_start_tick, note.pitch, note.id.clone()));
        let mut controls = Vec::new();
        for control in &clip.controls {
            let absolute_tick = clip.start_tick + control.tick;
            if absolute_tick >= start_tick && absolute_tick < end_tick {
                controls.push(WindowControl {
                    id: control.id.clone(),
                    local_tick: control.tick,
                    absolute_tick,
                    position: self
                        .time_signature
                        .musical_position(self.ppq, absolute_tick)?,
                    controller: control.controller,
                    meaning: MidiControlMeaning::from_controller(control.controller),
                    value: control.value,
                });
            }
        }
        controls.sort_by_key(|control| {
            (
                control.absolute_tick,
                control.controller,
                control.id.clone(),
            )
        });
        Ok(ClipWindow {
            track_id: track.id.clone(),
            clip_id: clip.id.clone(),
            clip_start_tick: clip.start_tick,
            start_tick,
            end_tick,
            start_position: self.time_signature.musical_position(self.ppq, start_tick)?,
            end_position: self.time_signature.musical_position(self.ppq, end_tick)?,
            ppq: self.ppq,
            time_signature: self.time_signature.clone(),
            beat_length_tick,
            bar_length_tick,
            notes,
            controls,
        })
    }
}

/// One-based musical coordinates derived from an absolute Project tick.
/// `tick_in_beat` remains exact, including expressive off-grid timing.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct MusicalPosition {
    pub bar: Tick,
    pub beat: u8,
    pub tick_in_beat: Tick,
}

/// An exact reduced ratio. Event windows use this to express duration in
/// quarter-note units without introducing floating-point rounding.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct TickRatio {
    pub numerator: Tick,
    pub denominator: Tick,
}

impl TickRatio {
    fn new(numerator: Tick, denominator: Tick) -> Self {
        let divisor = greatest_common_divisor(numerator, denominator);
        Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommonDuration {
    Whole,
    DottedHalf,
    Half,
    DottedQuarter,
    Quarter,
    DottedEighth,
    QuarterTriplet,
    Eighth,
    DottedSixteenth,
    EighthTriplet,
    Sixteenth,
    SixteenthTriplet,
    ThirtySecond,
}

impl CommonDuration {
    fn from_ratio(ratio: TickRatio) -> Option<Self> {
        match (ratio.numerator, ratio.denominator) {
            (4, 1) => Some(Self::Whole),
            (3, 1) => Some(Self::DottedHalf),
            (2, 1) => Some(Self::Half),
            (3, 2) => Some(Self::DottedQuarter),
            (1, 1) => Some(Self::Quarter),
            (3, 4) => Some(Self::DottedEighth),
            (2, 3) => Some(Self::QuarterTriplet),
            (1, 2) => Some(Self::Eighth),
            (3, 8) => Some(Self::DottedSixteenth),
            (1, 3) => Some(Self::EighthTriplet),
            (1, 4) => Some(Self::Sixteenth),
            (1, 6) => Some(Self::SixteenthTriplet),
            (1, 8) => Some(Self::ThirtySecond),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MidiControlMeaning {
    SustainPedal,
    SostenutoPedal,
    SoftPedal,
}

impl MidiControlMeaning {
    fn from_controller(controller: u8) -> Option<Self> {
        match controller {
            64 => Some(Self::SustainPedal),
            66 => Some(Self::SostenutoPedal),
            67 => Some(Self::SoftPedal),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct WindowNote {
    pub id: NoteId,
    pub local_start_tick: Tick,
    pub absolute_start_tick: Tick,
    pub start_position: MusicalPosition,
    pub end_position: MusicalPosition,
    pub duration_tick: Tick,
    pub duration_quarters: TickRatio,
    pub common_duration: Option<CommonDuration>,
    pub pitch: u8,
    pub pitch_name: String,
    pub velocity: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct WindowControl {
    pub id: String,
    pub local_tick: Tick,
    pub absolute_tick: Tick,
    pub position: MusicalPosition,
    pub controller: u8,
    pub meaning: Option<MidiControlMeaning>,
    pub value: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ClipWindow {
    pub track_id: TrackId,
    pub clip_id: ClipId,
    pub clip_start_tick: Tick,
    pub start_tick: Tick,
    pub end_tick: Tick,
    pub start_position: MusicalPosition,
    pub end_position: MusicalPosition,
    pub ppq: u16,
    pub time_signature: TimeSignature,
    pub beat_length_tick: Tick,
    pub bar_length_tick: Tick,
    pub notes: Vec<WindowNote>,
    pub controls: Vec<WindowControl>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrackSummaryKind {
    Midi,
    Audio,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct PitchRange {
    pub min: u8,
    pub max: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ClipSummary {
    pub id: ClipId,
    pub start_tick: Tick,
    pub length_tick: Tick,
    pub content_end_tick: Tick,
    pub note_count: usize,
    pub control_count: usize,
    pub pitch_range: Option<PitchRange>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TrackSummary {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackSummaryKind,
    pub instrument: Option<InstrumentId>,
    pub mixer: MixerSettings,
    pub note_count: usize,
    pub control_count: usize,
    pub pitch_range: Option<PitchRange>,
    pub clips: Vec<ClipSummary>,
}

impl TrackSummary {
    fn from_track(track: &Track) -> Self {
        let (kind, instrument, clips): (TrackSummaryKind, Option<InstrumentId>, Vec<ClipSummary>) =
            match &track.source {
                TrackSource::Midi { instrument, clips } => (
                    TrackSummaryKind::Midi,
                    Some(instrument.clone()),
                    clips.iter().map(ClipSummary::from_midi_clip).collect(),
                ),
                TrackSource::Audio { clips } => (
                    TrackSummaryKind::Audio,
                    None,
                    clips.iter().map(ClipSummary::from_audio_clip).collect(),
                ),
            };
        let note_count = clips.iter().map(|clip| clip.note_count).sum();
        let control_count = clips.iter().map(|clip| clip.control_count).sum();
        let pitch_range = clips
            .iter()
            .filter_map(|clip| clip.pitch_range.as_ref())
            .fold(None, |range: Option<PitchRange>, clip_range| {
                Some(match range {
                    Some(range) => PitchRange {
                        min: range.min.min(clip_range.min),
                        max: range.max.max(clip_range.max),
                    },
                    None => clip_range.clone(),
                })
            });
        Self {
            id: track.id.clone(),
            name: track.name.clone(),
            kind,
            instrument,
            mixer: track.mixer.clone(),
            note_count,
            control_count,
            pitch_range,
            clips,
        }
    }
}

impl ClipSummary {
    fn from_midi_clip(clip: &Clip) -> Self {
        let note_end = clip
            .notes
            .iter()
            .map(|note| note.start_tick + note.duration_tick)
            .max()
            .unwrap_or(0);
        let control_end = clip
            .controls
            .iter()
            .map(|control| control.tick)
            .max()
            .unwrap_or(0);
        let pitch_range = clip.notes.iter().map(|note| note.pitch).fold(
            None,
            |range: Option<PitchRange>, pitch| {
                Some(match range {
                    Some(range) => PitchRange {
                        min: range.min.min(pitch),
                        max: range.max.max(pitch),
                    },
                    None => PitchRange {
                        min: pitch,
                        max: pitch,
                    },
                })
            },
        );
        Self {
            id: clip.id.clone(),
            start_tick: clip.start_tick,
            length_tick: clip.length_tick,
            content_end_tick: clip.length_tick.max(note_end).max(control_end),
            note_count: clip.notes.len(),
            control_count: clip.controls.len(),
            pitch_range,
        }
    }

    fn from_audio_clip(clip: &AudioClip) -> Self {
        Self {
            id: clip.id.clone(),
            start_tick: clip.start_tick,
            length_tick: clip.length_tick,
            content_end_tick: clip.length_tick,
            note_count: 0,
            control_count: 0,
            pitch_range: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ProjectSummary {
    pub schema_version: u32,
    pub revision: u64,
    pub ppq: u16,
    pub tempo_map: TempoMap,
    pub time_signature: TimeSignature,
    pub duration_tick: Tick,
    pub tracks: Vec<TrackSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TempoMap {
    pub points: Vec<TempoPoint>,
}

impl Default for TempoMap {
    fn default() -> Self {
        Self {
            points: vec![TempoPoint {
                tick: 0,
                bpm: DEFAULT_BPM,
            }],
        }
    }
}

impl TempoMap {
    pub fn set_point(&mut self, tick: Tick, bpm: f64) -> Result<(), ProjectError> {
        validate_bpm(bpm)?;
        if tick < 0 {
            return Err(ProjectError::InvalidTick(tick));
        }
        if let Some(point) = self.points.iter_mut().find(|point| point.tick == tick) {
            point.bpm = bpm;
        } else {
            self.points.push(TempoPoint { tick, bpm });
            self.points.sort_by_key(|point| point.tick);
        }
        if self.points.first().map(|point| point.tick) != Some(0) {
            self.points.insert(
                0,
                TempoPoint {
                    tick: 0,
                    bpm: DEFAULT_BPM,
                },
            );
        }
        Ok(())
    }

    pub fn seconds_at(&self, tick: Tick, ppq: u16) -> f64 {
        if tick <= 0 || ppq == 0 {
            return 0.0;
        }
        let mut seconds = 0.0;
        let mut segment_start = 0_i64;
        let mut bpm = DEFAULT_BPM;
        for point in &self.points {
            if point.tick <= 0 {
                bpm = point.bpm;
                continue;
            }
            if point.tick >= tick {
                break;
            }
            seconds += ticks_to_seconds(point.tick - segment_start, bpm, ppq);
            segment_start = point.tick;
            bpm = point.bpm;
        }
        seconds + ticks_to_seconds(tick - segment_start, bpm, ppq)
    }

    pub fn ticks_to_samples(&self, tick: Tick, ppq: u16, sample_rate: u32) -> u64 {
        (self.seconds_at(tick, ppq) * sample_rate as f64).round() as u64
    }
}

fn ticks_to_seconds(ticks: Tick, bpm: f64, ppq: u16) -> f64 {
    ticks as f64 * 60.0 / (bpm * ppq as f64)
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TempoPoint {
    pub tick: Tick,
    pub bpm: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct TimeSignature {
    pub numerator: u8,
    pub denominator: u8,
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self {
            numerator: 4,
            denominator: 4,
        }
    }
}

impl TimeSignature {
    /// Returns whether this is a representable MIDI-style time signature.
    /// Denominators are powers of two so a bar can be mapped to PPQ ticks
    /// without silently rounding the beat grid.
    pub fn is_valid(&self) -> bool {
        validate_time_signature(self.numerator, self.denominator).is_ok()
    }

    pub fn beat_length_tick(&self, ppq: u16) -> Result<Tick, ProjectError> {
        validate_time_signature(self.numerator, self.denominator)?;
        let numerator = ppq as Tick * 4;
        if numerator % self.denominator as Tick != 0 {
            return Err(ProjectError::IncompatibleMeter {
                ppq,
                denominator: self.denominator,
            });
        }
        Ok(numerator / self.denominator as Tick)
    }

    pub fn bar_length_tick(&self, ppq: u16) -> Result<Tick, ProjectError> {
        Ok(self.beat_length_tick(ppq)? * self.numerator as Tick)
    }

    pub fn musical_position(
        &self,
        ppq: u16,
        absolute_tick: Tick,
    ) -> Result<MusicalPosition, ProjectError> {
        if absolute_tick < 0 {
            return Err(ProjectError::InvalidTick(absolute_tick));
        }
        let beat_length_tick = self.beat_length_tick(ppq)?;
        let bar_length_tick = beat_length_tick * self.numerator as Tick;
        let tick_in_bar = absolute_tick % bar_length_tick;
        Ok(MusicalPosition {
            bar: absolute_tick / bar_length_tick + 1,
            beat: (tick_in_bar / beat_length_tick + 1) as u8,
            tick_in_beat: tick_in_bar % beat_length_tick,
        })
    }
}

pub fn midi_note_name(pitch: u8) -> String {
    const PITCH_CLASSES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let pitch_class = PITCH_CLASSES[usize::from(pitch % 12)];
    let octave = i16::from(pitch) / 12 - 1;
    format!("{pitch_class}{octave}")
}

fn greatest_common_divisor(mut left: Tick, mut right: Tick) -> Tick {
    left = left.abs();
    right = right.abs();
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left.max(1)
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub source: TrackSource,
    pub mixer: MixerSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrackSource {
    Midi {
        instrument: InstrumentId,
        clips: Vec<Clip>,
    },
    Audio {
        clips: Vec<AudioClip>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Clip {
    pub id: ClipId,
    pub start_tick: Tick,
    pub length_tick: Tick,
    /// Notes are local to the clip.  Their absolute position is
    /// `clip.start_tick + note.start_tick`.
    pub notes: Vec<NoteEvent>,
    #[serde(default)]
    pub controls: Vec<ControlEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AudioClip {
    pub id: String,
    pub asset: String,
    pub start_tick: Tick,
    pub length_tick: Tick,
    pub offset_samples: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct NoteEvent {
    pub id: NoteId,
    pub start_tick: Tick,
    pub duration_tick: Tick,
    pub pitch: u8,
    pub velocity: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ControlEvent {
    pub id: String,
    pub tick: Tick,
    pub controller: u8,
    pub value: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ScheduledNote {
    pub instrument: InstrumentId,
    pub start_tick: Tick,
    pub duration_tick: Tick,
    pub pitch: u8,
    pub velocity: u8,
    pub gain_db: f32,
    pub pan: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct MixerSettings {
    pub gain_db: f32,
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
}

impl Default for MixerSettings {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    /// Creates a MIDI track with one empty `{track_id}-main` clip covering
    /// sixteen beats, so the result is immediately editable by GUI and AI
    /// callers without a second setup command.
    CreateTrack {
        track_id: TrackId,
        name: String,
        instrument: InstrumentId,
    },
    RemoveTrack {
        track_id: TrackId,
    },
    RenameTrack {
        track_id: TrackId,
        name: String,
    },
    SetTrackInstrument {
        track_id: TrackId,
        instrument: InstrumentId,
    },
    AddClip {
        track_id: TrackId,
        clip_id: ClipId,
        start_tick: Tick,
        length_tick: Tick,
    },
    AddNote {
        track_id: TrackId,
        clip_id: ClipId,
        note: NoteEvent,
    },
    AddControl {
        track_id: TrackId,
        clip_id: ClipId,
        control: ControlEvent,
    },
    SetControl {
        track_id: TrackId,
        clip_id: ClipId,
        control_id: String,
        tick: Tick,
        controller: u8,
        value: u8,
    },
    RemoveControl {
        track_id: TrackId,
        clip_id: ClipId,
        control_id: String,
    },
    RemoveNote {
        track_id: TrackId,
        clip_id: ClipId,
        note_id: NoteId,
    },
    SetNoteVelocity {
        track_id: TrackId,
        clip_id: ClipId,
        note_id: NoteId,
        velocity: u8,
    },
    MoveNote {
        track_id: TrackId,
        clip_id: ClipId,
        note_id: NoteId,
        start_tick: Tick,
        pitch: u8,
    },
    ResizeNote {
        track_id: TrackId,
        clip_id: ClipId,
        note_id: NoteId,
        duration_tick: Tick,
    },
    SetTempo {
        tick: Tick,
        bpm: f64,
    },
    SetTimeSignature {
        numerator: u8,
        denominator: u8,
    },
    /// Quantizes note onsets in a clip-local half-open range.  `strength`
    /// blends toward the nearest grid point, so 0 preserves the performance
    /// and 100 applies exact quantization.  Note durations and controls are
    /// intentionally untouched.
    QuantizeNotes {
        track_id: TrackId,
        clip_id: ClipId,
        start_tick: Tick,
        end_tick: Tick,
        grid_tick: Tick,
        strength: u8,
    },
    SetTrackMixer {
        track_id: TrackId,
        gain_db: f32,
        pan: f32,
        mute: bool,
        solo: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ChangeSet {
    pub revision: u64,
    pub description: String,
}

/// A group of commands applied as one transaction.  This is the wire format
/// used by the CLI, the Tauri command layer, and the AI composition adapter.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Patch {
    #[serde(default)]
    pub base_revision: Option<u64>,
    #[serde(default)]
    pub description: Option<String>,
    pub operations: Vec<Command>,
}

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("track not found: {0}")]
    TrackNotFound(String),
    #[error("clip not found: {0}")]
    ClipNotFound(String),
    #[error("note not found: {0}")]
    NoteNotFound(String),
    #[error("track is not a MIDI track")]
    NotMidiTrack,
    #[error("invalid tick: {0}")]
    InvalidTick(Tick),
    #[error("invalid tick window: start {start}, end {end}")]
    InvalidTickWindow { start: Tick, end: Tick },
    #[error("duration must be positive")]
    InvalidDuration,
    #[error("pitch must be between 0 and 127")]
    InvalidPitch,
    #[error("velocity must be between 1 and 127")]
    InvalidVelocity,
    #[error("MIDI controller and value must be between 0 and 127")]
    InvalidControl,
    #[error("BPM must be between 1 and 1000")]
    InvalidBpm,
    #[error("PPQ must be positive")]
    InvalidPpq,
    #[error("tempo map must start at tick 0 and be strictly ordered")]
    InvalidTempoMap,
    #[error("invalid time signature {numerator}/{denominator}")]
    InvalidTimeSignature { numerator: u8, denominator: u8 },
    #[error("PPQ {ppq} cannot represent a denominator of {denominator} exactly")]
    IncompatibleMeter { ppq: u16, denominator: u8 },
    #[error("quantization grid must be positive")]
    InvalidQuantizeGrid,
    #[error("quantization strength must be between 0 and 100")]
    InvalidQuantizeStrength,
    #[error("track id must not be empty")]
    InvalidTrackId,
    #[error("track name must not be empty")]
    InvalidTrackName,
    #[error("instrument id must not be empty")]
    InvalidInstrument,
    #[error("gain must be finite and between -96 and 24 dB")]
    InvalidGain,
    #[error("pan must be between -1 and 1")]
    InvalidPan,
    #[error("a track with this id already exists: {0}")]
    DuplicateTrack(String),
    #[error("a clip with this id already exists: {0}")]
    DuplicateClip(String),
    #[error("a note with this id already exists: {0}")]
    DuplicateNote(String),
    #[error("a control event with this id already exists: {0}")]
    DuplicateControl(String),
    #[error("control event not found: {0}")]
    ControlNotFound(String),
    #[error("revision conflict: patch expects {expected}, project is at {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("serialization failed: {0}")]
    Serialization(serde_json::Error),
    #[error("I/O failed: {0}")]
    Io(std::io::Error),
}

#[derive(Clone)]
pub struct ProjectEngine {
    project: Project,
    revision: u64,
    undo_stack: Vec<Project>,
    redo_stack: Vec<Project>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct PatchPreview {
    pub base_revision: u64,
    pub resulting_revision: u64,
    pub operation_count: usize,
    pub affected_tracks: Vec<TrackId>,
    pub description: String,
}

impl ProjectEngine {
    pub fn new(project: Project) -> Self {
        let revision = project.revision;
        Self {
            project,
            revision,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn apply(&mut self, command: Command) -> Result<ChangeSet, ProjectError> {
        self.apply_patch(Patch {
            base_revision: None,
            description: None,
            operations: vec![command],
        })
    }

    /// Validates a patch against the current project without changing the
    /// project, undo stack, redo stack, or revision. This is the safe first
    /// half of an AI edit: callers can preview a patch, then submit the exact
    /// same value with the same `base_revision`.
    pub fn preview_patch(&self, patch: &Patch) -> Result<PatchPreview, ProjectError> {
        if let Some(expected) = patch.base_revision
            && expected != self.revision
        {
            return Err(ProjectError::RevisionConflict {
                expected,
                actual: self.revision,
            });
        }
        if patch.operations.is_empty() {
            return Ok(PatchPreview {
                base_revision: self.revision,
                resulting_revision: self.revision,
                operation_count: 0,
                affected_tracks: Vec::new(),
                description: patch
                    .description
                    .clone()
                    .unwrap_or_else(|| "empty patch".to_owned()),
            });
        }
        let mut shadow = ProjectEngine::new(self.project.clone());
        shadow.apply_patch_unchecked(patch.clone())?;
        let mut affected_tracks = Vec::new();
        for operation in &patch.operations {
            let Some(track_id) = command_track_id(operation) else {
                continue;
            };
            if !affected_tracks.iter().any(|existing| existing == track_id) {
                affected_tracks.push(track_id.to_owned());
            }
        }
        Ok(PatchPreview {
            base_revision: self.revision,
            resulting_revision: self.revision.saturating_add(1),
            operation_count: patch.operations.len(),
            affected_tracks,
            description: patch
                .description
                .clone()
                .unwrap_or_else(|| "project updated".to_owned()),
        })
    }

    /// Apply several commands atomically.  If any command fails, the project,
    /// undo stack, redo stack, and revision remain unchanged.
    pub fn apply_patch(&mut self, patch: Patch) -> Result<ChangeSet, ProjectError> {
        if let Some(expected) = patch.base_revision
            && expected != self.revision
        {
            return Err(ProjectError::RevisionConflict {
                expected,
                actual: self.revision,
            });
        }
        if patch.operations.is_empty() {
            return Ok(ChangeSet {
                revision: self.revision,
                description: patch
                    .description
                    .unwrap_or_else(|| "empty patch".to_owned()),
            });
        }

        self.apply_patch_unchecked(patch)
    }

    fn apply_patch_unchecked(&mut self, patch: Patch) -> Result<ChangeSet, ProjectError> {
        let previous = self.project.clone();
        for operation in patch.operations {
            if let Err(error) = self.apply_inner(operation) {
                self.project = previous;
                return Err(error);
            }
        }
        self.undo_stack.push(previous);
        self.redo_stack.clear();
        self.revision += 1;
        self.project.revision = self.revision;
        Ok(ChangeSet {
            revision: self.revision,
            description: patch
                .description
                .unwrap_or_else(|| "project updated".to_owned()),
        })
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo_stack.pop() else {
            return false;
        };
        self.redo_stack.push(self.project.clone());
        self.project = previous;
        self.revision += 1;
        self.project.revision = self.revision;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo_stack.pop() else {
            return false;
        };
        self.undo_stack.push(self.project.clone());
        self.project = next;
        self.revision += 1;
        self.project.revision = self.revision;
        true
    }

    fn apply_inner(&mut self, command: Command) -> Result<(), ProjectError> {
        match command {
            Command::CreateTrack {
                track_id,
                name,
                instrument,
            } => {
                validate_track_id(&track_id)?;
                let name = normalized_track_name(name)?;
                let instrument = normalized_instrument(instrument)?;
                if self.project.tracks.iter().any(|track| track.id == track_id) {
                    return Err(ProjectError::DuplicateTrack(track_id));
                }
                let clip_id = format!("{track_id}-main");
                self.project.tracks.push(Track {
                    id: track_id,
                    name,
                    source: TrackSource::Midi {
                        instrument,
                        clips: vec![Clip {
                            id: clip_id,
                            start_tick: 0,
                            length_tick: self.project.ppq as Tick * 16,
                            notes: Vec::new(),
                            controls: Vec::new(),
                        }],
                    },
                    mixer: MixerSettings::default(),
                });
            }
            Command::RemoveTrack { track_id } => {
                let index = self
                    .project
                    .tracks
                    .iter()
                    .position(|track| track.id == track_id)
                    .ok_or_else(|| ProjectError::TrackNotFound(track_id.clone()))?;
                self.project.tracks.remove(index);
            }
            Command::RenameTrack { track_id, name } => {
                let name = normalized_track_name(name)?;
                let track = self
                    .project
                    .track_mut(&track_id)
                    .ok_or_else(|| ProjectError::TrackNotFound(track_id.clone()))?;
                track.name = name;
            }
            Command::SetTrackInstrument {
                track_id,
                instrument,
            } => {
                let instrument = normalized_instrument(instrument)?;
                let track = self
                    .project
                    .track_mut(&track_id)
                    .ok_or_else(|| ProjectError::TrackNotFound(track_id.clone()))?;
                let TrackSource::Midi {
                    instrument: current,
                    ..
                } = &mut track.source
                else {
                    return Err(ProjectError::NotMidiTrack);
                };
                *current = instrument;
            }
            Command::AddClip {
                track_id,
                clip_id,
                start_tick,
                length_tick,
            } => {
                if start_tick < 0 {
                    return Err(ProjectError::InvalidTick(start_tick));
                }
                validate_duration(length_tick)?;
                let track = self
                    .project
                    .track_mut(&track_id)
                    .ok_or_else(|| ProjectError::TrackNotFound(track_id.clone()))?;
                let TrackSource::Midi { clips, .. } = &mut track.source else {
                    return Err(ProjectError::NotMidiTrack);
                };
                if clips.iter().any(|clip| clip.id == clip_id) {
                    return Err(ProjectError::DuplicateClip(clip_id));
                }
                clips.push(Clip {
                    id: clip_id,
                    start_tick,
                    length_tick,
                    notes: Vec::new(),
                    controls: Vec::new(),
                });
            }
            Command::AddNote {
                track_id,
                clip_id,
                note,
            } => {
                validate_note(&note)?;
                let clip = self
                    .project
                    .midi_clip_mut(&track_id, &clip_id)
                    .ok_or_else(|| ProjectError::ClipNotFound(clip_id.clone()))?;
                if clip.notes.iter().any(|existing| existing.id == note.id) {
                    return Err(ProjectError::DuplicateNote(note.id));
                }
                clip.notes.push(note);
                clip.notes.sort_by_key(|item| item.start_tick);
            }
            Command::AddControl {
                track_id,
                clip_id,
                control,
            } => {
                validate_control(control.tick, control.controller, control.value)?;
                let clip = self
                    .project
                    .midi_clip_mut(&track_id, &clip_id)
                    .ok_or_else(|| ProjectError::ClipNotFound(clip_id.clone()))?;
                if clip
                    .controls
                    .iter()
                    .any(|existing| existing.id == control.id)
                {
                    return Err(ProjectError::DuplicateControl(control.id));
                }
                clip.controls.push(control);
                clip.controls.sort_by_key(|item| item.tick);
            }
            Command::SetControl {
                track_id,
                clip_id,
                control_id,
                tick,
                controller,
                value,
            } => {
                validate_control(tick, controller, value)?;
                let clip = self
                    .project
                    .midi_clip_mut(&track_id, &clip_id)
                    .ok_or_else(|| ProjectError::ClipNotFound(clip_id.clone()))?;
                let control = clip
                    .controls
                    .iter_mut()
                    .find(|control| control.id == control_id)
                    .ok_or_else(|| ProjectError::ControlNotFound(control_id.clone()))?;
                control.tick = tick;
                control.controller = controller;
                control.value = value;
                clip.controls.sort_by_key(|item| item.tick);
            }
            Command::RemoveControl {
                track_id,
                clip_id,
                control_id,
            } => {
                let clip = self
                    .project
                    .midi_clip_mut(&track_id, &clip_id)
                    .ok_or_else(|| ProjectError::ClipNotFound(clip_id.clone()))?;
                let index = clip
                    .controls
                    .iter()
                    .position(|control| control.id == control_id)
                    .ok_or_else(|| ProjectError::ControlNotFound(control_id.clone()))?;
                clip.controls.remove(index);
            }
            Command::RemoveNote {
                track_id,
                clip_id,
                note_id,
            } => {
                let clip = self
                    .project
                    .midi_clip_mut(&track_id, &clip_id)
                    .ok_or_else(|| ProjectError::ClipNotFound(clip_id.clone()))?;
                let index = clip
                    .notes
                    .iter()
                    .position(|note| note.id == note_id)
                    .ok_or_else(|| ProjectError::NoteNotFound(note_id.clone()))?;
                clip.notes.remove(index);
            }
            Command::SetNoteVelocity {
                track_id,
                clip_id,
                note_id,
                velocity,
            } => {
                validate_velocity(velocity)?;
                let clip = self
                    .project
                    .midi_clip_mut(&track_id, &clip_id)
                    .ok_or_else(|| ProjectError::ClipNotFound(clip_id.clone()))?;
                let note = clip
                    .notes
                    .iter_mut()
                    .find(|note| note.id == note_id)
                    .ok_or_else(|| ProjectError::NoteNotFound(note_id.clone()))?;
                note.velocity = velocity;
            }
            Command::MoveNote {
                track_id,
                clip_id,
                note_id,
                start_tick,
                pitch,
            } => {
                if start_tick < 0 {
                    return Err(ProjectError::InvalidTick(start_tick));
                }
                validate_pitch(pitch)?;
                let clip = self
                    .project
                    .midi_clip_mut(&track_id, &clip_id)
                    .ok_or_else(|| ProjectError::ClipNotFound(clip_id.clone()))?;
                let note = clip
                    .notes
                    .iter_mut()
                    .find(|note| note.id == note_id)
                    .ok_or_else(|| ProjectError::NoteNotFound(note_id.clone()))?;
                note.start_tick = start_tick;
                note.pitch = pitch;
                clip.notes.sort_by_key(|item| item.start_tick);
            }
            Command::ResizeNote {
                track_id,
                clip_id,
                note_id,
                duration_tick,
            } => {
                validate_duration(duration_tick)?;
                let clip = self
                    .project
                    .midi_clip_mut(&track_id, &clip_id)
                    .ok_or_else(|| ProjectError::ClipNotFound(clip_id.clone()))?;
                let note = clip
                    .notes
                    .iter_mut()
                    .find(|note| note.id == note_id)
                    .ok_or_else(|| ProjectError::NoteNotFound(note_id.clone()))?;
                note.duration_tick = duration_tick;
            }
            Command::SetTempo { tick, bpm } => {
                self.project.tempo_map.set_point(tick, bpm)?;
            }
            Command::SetTimeSignature {
                numerator,
                denominator,
            } => {
                let signature = TimeSignature {
                    numerator,
                    denominator,
                };
                signature.beat_length_tick(self.project.ppq)?;
                self.project.time_signature = signature;
            }
            Command::QuantizeNotes {
                track_id,
                clip_id,
                start_tick,
                end_tick,
                grid_tick,
                strength,
            } => {
                validate_tick_window(start_tick, end_tick)?;
                validate_quantize_grid(grid_tick, strength)?;
                let clip = self
                    .project
                    .midi_clip_mut(&track_id, &clip_id)
                    .ok_or_else(|| ProjectError::ClipNotFound(clip_id.clone()))?;
                for note in &mut clip.notes {
                    if note.start_tick < start_tick || note.start_tick >= end_tick {
                        continue;
                    }
                    let target = quantize_tick(note.start_tick, grid_tick, strength)?;
                    if target >= start_tick && target < end_tick {
                        note.start_tick = target;
                    }
                }
                clip.notes.sort_by_key(|item| item.start_tick);
            }
            Command::SetTrackMixer {
                track_id,
                gain_db,
                pan,
                mute,
                solo,
            } => {
                if !(-1.0..=1.0).contains(&pan) {
                    return Err(ProjectError::InvalidPan);
                }
                if !gain_db.is_finite() || !(-96.0..=24.0).contains(&gain_db) {
                    return Err(ProjectError::InvalidGain);
                }
                let track = self
                    .project
                    .track_mut(&track_id)
                    .ok_or_else(|| ProjectError::TrackNotFound(track_id.clone()))?;
                track.mixer = MixerSettings {
                    gain_db,
                    pan,
                    mute,
                    solo,
                };
            }
        }
        Ok(())
    }
}

fn validate_note(note: &NoteEvent) -> Result<(), ProjectError> {
    if note.start_tick < 0 {
        return Err(ProjectError::InvalidTick(note.start_tick));
    }
    validate_duration(note.duration_tick)?;
    validate_pitch(note.pitch)?;
    validate_velocity(note.velocity)
}

fn validate_duration(duration: Tick) -> Result<(), ProjectError> {
    if duration <= 0 {
        Err(ProjectError::InvalidDuration)
    } else {
        Ok(())
    }
}

fn validate_pitch(pitch: u8) -> Result<(), ProjectError> {
    if pitch > 127 {
        Err(ProjectError::InvalidPitch)
    } else {
        Ok(())
    }
}

fn validate_velocity(velocity: u8) -> Result<(), ProjectError> {
    if velocity == 0 || velocity > 127 {
        Err(ProjectError::InvalidVelocity)
    } else {
        Ok(())
    }
}

fn validate_control(tick: Tick, controller: u8, value: u8) -> Result<(), ProjectError> {
    if tick < 0 {
        return Err(ProjectError::InvalidTick(tick));
    }
    if controller > 127 || value > 127 {
        return Err(ProjectError::InvalidControl);
    }
    Ok(())
}

fn validate_tick_window(start_tick: Tick, end_tick: Tick) -> Result<(), ProjectError> {
    if start_tick < 0 || end_tick <= start_tick {
        return Err(ProjectError::InvalidTickWindow {
            start: start_tick,
            end: end_tick,
        });
    }
    Ok(())
}

fn validate_time_signature(numerator: u8, denominator: u8) -> Result<(), ProjectError> {
    let denominator_is_power_of_two = denominator.is_power_of_two();
    if numerator == 0 || numerator > 32 || !denominator_is_power_of_two || denominator > 128 {
        return Err(ProjectError::InvalidTimeSignature {
            numerator,
            denominator,
        });
    }
    Ok(())
}

fn validate_quantize_grid(grid_tick: Tick, strength: u8) -> Result<(), ProjectError> {
    if grid_tick <= 0 {
        return Err(ProjectError::InvalidQuantizeGrid);
    }
    if strength > 100 {
        return Err(ProjectError::InvalidQuantizeStrength);
    }
    Ok(())
}

/// Returns a deterministic, bounded blend from an onset to its nearest grid
/// point.  This is public so the proposal reviewer can calculate the exact
/// affected timeline without reimplementing quantization policy.
pub fn quantize_tick(tick: Tick, grid_tick: Tick, strength: u8) -> Result<Tick, ProjectError> {
    if tick < 0 {
        return Err(ProjectError::InvalidTick(tick));
    }
    validate_quantize_grid(grid_tick, strength)?;
    if strength == 0 {
        return Ok(tick);
    }
    let lower = (tick / grid_tick).saturating_mul(grid_tick);
    let upper = lower.saturating_add(grid_tick);
    let target = if tick.saturating_sub(lower) < upper.saturating_sub(tick) {
        lower
    } else {
        upper
    };
    let delta = target as i128 - tick as i128;
    let blended = tick as i128 + (delta * strength as i128) / 100;
    Ok(blended.max(0).min(Tick::MAX as i128) as Tick)
}

fn command_track_id(command: &Command) -> Option<&str> {
    match command {
        Command::CreateTrack { track_id, .. }
        | Command::RemoveTrack { track_id }
        | Command::RenameTrack { track_id, .. }
        | Command::SetTrackInstrument { track_id, .. }
        | Command::AddClip { track_id, .. }
        | Command::AddNote { track_id, .. }
        | Command::AddControl { track_id, .. }
        | Command::SetControl { track_id, .. }
        | Command::RemoveControl { track_id, .. }
        | Command::RemoveNote { track_id, .. }
        | Command::SetNoteVelocity { track_id, .. }
        | Command::MoveNote { track_id, .. }
        | Command::ResizeNote { track_id, .. }
        | Command::QuantizeNotes { track_id, .. }
        | Command::SetTrackMixer { track_id, .. } => Some(track_id),
        Command::SetTempo { .. } | Command::SetTimeSignature { .. } => None,
    }
}

fn validate_bpm(bpm: f64) -> Result<(), ProjectError> {
    if (1.0..=1000.0).contains(&bpm) {
        Ok(())
    } else {
        Err(ProjectError::InvalidBpm)
    }
}

fn validate_track_id(track_id: &str) -> Result<(), ProjectError> {
    if track_id.trim().is_empty() {
        Err(ProjectError::InvalidTrackId)
    } else {
        Ok(())
    }
}

fn normalized_track_name(name: String) -> Result<String, ProjectError> {
    let name = name.trim();
    if name.is_empty() {
        Err(ProjectError::InvalidTrackName)
    } else {
        Ok(name.to_owned())
    }
}

fn normalized_instrument(instrument: String) -> Result<String, ProjectError> {
    let instrument = instrument.trim();
    if instrument.is_empty() {
        Err(ProjectError::InvalidInstrument)
    } else {
        Ok(instrument.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_project_has_a_piano_track() {
        let project = Project::default();
        assert_eq!(project.tracks.len(), 1);
        assert_eq!(project.scheduled_notes().len(), 0);
    }

    #[test]
    fn commands_are_undoable() {
        let mut engine = ProjectEngine::new(Project::default());
        engine
            .apply(Command::AddNote {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                note: NoteEvent {
                    id: "n1".to_owned(),
                    start_tick: 0,
                    duration_tick: 960,
                    pitch: 60,
                    velocity: 90,
                },
            })
            .unwrap();
        assert_eq!(engine.project().scheduled_notes().len(), 1);
        assert!(engine.undo());
        assert_eq!(engine.project().scheduled_notes().len(), 0);
        assert!(engine.redo());
        assert_eq!(engine.project().scheduled_notes().len(), 1);
    }

    #[test]
    fn piano_roll_edit_commands_change_authoritative_project_state() {
        let mut engine = ProjectEngine::new(Project::default());
        engine
            .apply(Command::AddNote {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                note: NoteEvent {
                    id: "editable".to_owned(),
                    start_tick: 0,
                    duration_tick: 960,
                    pitch: 60,
                    velocity: 90,
                },
            })
            .unwrap();
        engine
            .apply(Command::MoveNote {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                note_id: "editable".to_owned(),
                start_tick: 1_920,
                pitch: 64,
            })
            .unwrap();
        engine
            .apply(Command::ResizeNote {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                note_id: "editable".to_owned(),
                duration_tick: 480,
            })
            .unwrap();

        let note = &engine
            .project()
            .midi_clip("piano", "piano-main")
            .unwrap()
            .notes[0];
        assert_eq!(
            (note.start_tick, note.duration_tick, note.pitch),
            (1_920, 480, 64)
        );

        engine
            .apply(Command::RemoveNote {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                note_id: "editable".to_owned(),
            })
            .unwrap();
        assert!(
            engine
                .project()
                .midi_clip("piano", "piano-main")
                .unwrap()
                .notes
                .is_empty()
        );
        assert!(engine.undo());
        assert_eq!(
            engine
                .project()
                .midi_clip("piano", "piano-main")
                .unwrap()
                .notes[0]
                .duration_tick,
            480
        );
    }

    #[test]
    fn track_lifecycle_commands_keep_a_ready_to_edit_track() {
        let mut engine = ProjectEngine::new(Project::default());
        engine
            .apply(Command::CreateTrack {
                track_id: "piano-2".to_owned(),
                name: "  Countermelody  ".to_owned(),
                instrument: " piano ".to_owned(),
            })
            .unwrap();

        let track = engine.project().track("piano-2").unwrap();
        assert_eq!(track.name, "Countermelody");
        let TrackSource::Midi { instrument, clips } = &track.source else {
            panic!("created track is not MIDI");
        };
        assert_eq!(instrument, "piano");
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].id, "piano-2-main");
        assert_eq!(clips[0].length_tick, engine.project().ppq as Tick * 16);

        engine
            .apply_patch(Patch {
                base_revision: Some(1),
                description: Some("configure second track".to_owned()),
                operations: vec![
                    Command::RenameTrack {
                        track_id: "piano-2".to_owned(),
                        name: "Harmony".to_owned(),
                    },
                    Command::SetTrackInstrument {
                        track_id: "piano-2".to_owned(),
                        instrument: "felt-piano".to_owned(),
                    },
                    Command::SetTrackMixer {
                        track_id: "piano-2".to_owned(),
                        gain_db: -6.0,
                        pan: 0.25,
                        mute: true,
                        solo: false,
                    },
                ],
            })
            .unwrap();
        let track = engine.project().track("piano-2").unwrap();
        assert_eq!(track.name, "Harmony");
        assert_eq!(track.mixer.gain_db, -6.0);
        assert!(track.mixer.mute);
        assert!(matches!(
            &track.source,
            TrackSource::Midi { instrument, .. } if instrument == "felt-piano"
        ));

        engine
            .apply(Command::RemoveTrack {
                track_id: "piano-2".to_owned(),
            })
            .unwrap();
        assert!(engine.project().track("piano-2").is_none());
        assert!(engine.undo());
        assert_eq!(engine.project().track("piano-2").unwrap().name, "Harmony");
    }

    #[test]
    fn invalid_track_patch_rolls_back_creation() {
        let mut engine = ProjectEngine::new(Project::default());
        let result = engine.apply_patch(Patch {
            base_revision: Some(0),
            description: None,
            operations: vec![
                Command::CreateTrack {
                    track_id: "temporary".to_owned(),
                    name: "Temporary".to_owned(),
                    instrument: "piano".to_owned(),
                },
                Command::RenameTrack {
                    track_id: "temporary".to_owned(),
                    name: "   ".to_owned(),
                },
            ],
        });
        assert!(matches!(result, Err(ProjectError::InvalidTrackName)));
        assert!(engine.project().track("temporary").is_none());
        assert_eq!(engine.revision(), 0);
    }

    #[test]
    fn performance_commands_edit_velocity_and_controls_atomically() {
        let mut engine = ProjectEngine::new(Project::default());
        engine
            .apply_patch(Patch {
                base_revision: Some(0),
                description: Some("add note and sustain pedal".to_owned()),
                operations: vec![
                    Command::AddNote {
                        track_id: "piano".to_owned(),
                        clip_id: "piano-main".to_owned(),
                        note: NoteEvent {
                            id: "performed-note".to_owned(),
                            start_tick: 0,
                            duration_tick: 1_920,
                            pitch: 60,
                            velocity: 72,
                        },
                    },
                    Command::AddControl {
                        track_id: "piano".to_owned(),
                        clip_id: "piano-main".to_owned(),
                        control: ControlEvent {
                            id: "sustain-down".to_owned(),
                            tick: 0,
                            controller: 64,
                            value: 127,
                        },
                    },
                ],
            })
            .unwrap();

        engine
            .apply_patch(Patch {
                base_revision: Some(1),
                description: Some("shape the performance".to_owned()),
                operations: vec![
                    Command::SetNoteVelocity {
                        track_id: "piano".to_owned(),
                        clip_id: "piano-main".to_owned(),
                        note_id: "performed-note".to_owned(),
                        velocity: 108,
                    },
                    Command::SetControl {
                        track_id: "piano".to_owned(),
                        clip_id: "piano-main".to_owned(),
                        control_id: "sustain-down".to_owned(),
                        tick: 480,
                        controller: 64,
                        value: 72,
                    },
                ],
            })
            .unwrap();

        let clip = engine.project().midi_clip("piano", "piano-main").unwrap();
        assert_eq!(clip.notes[0].velocity, 108);
        assert_eq!(
            (
                clip.controls[0].tick,
                clip.controls[0].controller,
                clip.controls[0].value
            ),
            (480, 64, 72)
        );

        let before_invalid_patch = engine.project().clone();
        let invalid = engine.apply_patch(Patch {
            base_revision: Some(2),
            description: None,
            operations: vec![
                Command::SetNoteVelocity {
                    track_id: "piano".to_owned(),
                    clip_id: "piano-main".to_owned(),
                    note_id: "performed-note".to_owned(),
                    velocity: 40,
                },
                Command::SetControl {
                    track_id: "piano".to_owned(),
                    clip_id: "piano-main".to_owned(),
                    control_id: "sustain-down".to_owned(),
                    tick: -1,
                    controller: 64,
                    value: 0,
                },
            ],
        });
        assert!(matches!(invalid, Err(ProjectError::InvalidTick(-1))));
        assert_eq!(engine.project(), &before_invalid_patch);
        assert_eq!(engine.revision(), 2);

        engine
            .apply(Command::RemoveControl {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                control_id: "sustain-down".to_owned(),
            })
            .unwrap();
        assert!(
            engine
                .project()
                .midi_clip("piano", "piano-main")
                .unwrap()
                .controls
                .is_empty()
        );
        assert!(engine.undo());
        assert_eq!(
            engine
                .project()
                .midi_clip("piano", "piano-main")
                .unwrap()
                .controls
                .len(),
            1
        );
    }

    #[test]
    fn tempo_map_converts_ticks() {
        let map = TempoMap::default();
        assert!((map.seconds_at(960, 960) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn meter_exposes_exact_beat_and_bar_ticks() {
        let compound = TimeSignature {
            numerator: 6,
            denominator: 8,
        };
        assert!(compound.is_valid());
        assert_eq!(compound.beat_length_tick(960).unwrap(), 480);
        assert_eq!(compound.bar_length_tick(960).unwrap(), 2_880);
        assert!(
            !TimeSignature {
                numerator: 3,
                denominator: 3,
            }
            .is_valid()
        );
    }

    #[test]
    fn quantization_moves_only_selected_onsets_and_preserves_performance_data() {
        let mut engine = ProjectEngine::new(Project::default());
        engine
            .apply_patch(Patch {
                base_revision: Some(0),
                description: Some("humanize then quantize".to_owned()),
                operations: vec![
                    Command::AddNote {
                        track_id: "piano".to_owned(),
                        clip_id: "piano-main".to_owned(),
                        note: NoteEvent {
                            id: "early".to_owned(),
                            start_tick: 110,
                            duration_tick: 700,
                            pitch: 60,
                            velocity: 74,
                        },
                    },
                    Command::AddNote {
                        track_id: "piano".to_owned(),
                        clip_id: "piano-main".to_owned(),
                        note: NoteEvent {
                            id: "late".to_owned(),
                            start_tick: 1_100,
                            duration_tick: 300,
                            pitch: 64,
                            velocity: 102,
                        },
                    },
                    Command::AddControl {
                        track_id: "piano".to_owned(),
                        clip_id: "piano-main".to_owned(),
                        control: ControlEvent {
                            id: "pedal".to_owned(),
                            tick: 120,
                            controller: 64,
                            value: 90,
                        },
                    },
                ],
            })
            .unwrap();
        engine
            .apply(Command::QuantizeNotes {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                start_tick: 0,
                end_tick: 960,
                grid_tick: 480,
                strength: 100,
            })
            .unwrap();

        let clip = engine.project().midi_clip("piano", "piano-main").unwrap();
        let early = clip.notes.iter().find(|note| note.id == "early").unwrap();
        let late = clip.notes.iter().find(|note| note.id == "late").unwrap();
        assert_eq!(
            (early.start_tick, early.duration_tick, early.velocity),
            (0, 700, 74)
        );
        assert_eq!(
            (late.start_tick, late.duration_tick, late.velocity),
            (1_100, 300, 102)
        );
        assert_eq!((clip.controls[0].tick, clip.controls[0].value), (120, 90));

        engine
            .apply(Command::SetTimeSignature {
                numerator: 6,
                denominator: 8,
            })
            .unwrap();
        assert_eq!(engine.project().time_signature.numerator, 6);
        assert_eq!(
            engine
                .project()
                .time_signature
                .bar_length_tick(960)
                .unwrap(),
            2_880
        );
        assert_eq!(quantize_tick(240, 480, 100).unwrap(), 480);
        assert_eq!(quantize_tick(240, 480, 50).unwrap(), 360);
    }

    #[test]
    fn quantization_rejects_invalid_grid_and_meter() {
        let mut engine = ProjectEngine::new(Project::default());
        assert!(matches!(
            engine.apply(Command::QuantizeNotes {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                start_tick: 0,
                end_tick: 960,
                grid_tick: 0,
                strength: 100,
            }),
            Err(ProjectError::InvalidQuantizeGrid)
        ));
        assert!(matches!(
            engine.apply(Command::SetTimeSignature {
                numerator: 4,
                denominator: 3,
            }),
            Err(ProjectError::InvalidTimeSignature { .. })
        ));
        assert_eq!(engine.revision(), 0);
    }

    #[test]
    fn loading_json_runs_the_same_whole_project_validation_as_package_save() {
        let mut project = Project::default();
        project.time_signature.denominator = 3;
        let json = serde_json::to_string(&project).unwrap();
        assert!(matches!(
            Project::from_json(&json),
            Err(ProjectError::InvalidTimeSignature {
                numerator: 4,
                denominator: 3
            })
        ));

        let mut duplicate = Project::default();
        duplicate.tracks.push(duplicate.tracks[0].clone());
        let json = serde_json::to_string(&duplicate).unwrap();
        assert!(matches!(
            Project::from_json(&json),
            Err(ProjectError::DuplicateTrack(id)) if id == "piano"
        ));
    }

    #[test]
    fn json_round_trip_is_stable() {
        let project = Project::demo();
        let json = project.to_pretty_json().unwrap();
        assert_eq!(Project::from_json(&json).unwrap(), project);
    }

    #[test]
    fn summary_keeps_ai_context_small_and_actionable() {
        let project = Project::demo();
        let summary = project.summary();
        assert_eq!(summary.revision, project.revision);
        assert_eq!(summary.duration_tick, project.duration_tick());
        assert_eq!(summary.tracks.len(), 1);
        assert_eq!(summary.tracks[0].note_count, 12);
        assert_eq!(summary.tracks[0].control_count, 0);
        assert_eq!(
            summary.tracks[0].pitch_range,
            Some(PitchRange { min: 53, max: 67 })
        );
        assert_eq!(summary.tracks[0].clips[0].content_end_tick, 15_360);
    }

    #[test]
    fn patch_preview_validates_without_mutating_and_reports_affected_tracks() {
        let mut engine = ProjectEngine::new(Project::default());
        let patch = Patch {
            base_revision: Some(0),
            description: Some("preview harmony".to_owned()),
            operations: vec![
                Command::CreateTrack {
                    track_id: "harmony".to_owned(),
                    name: "Harmony".to_owned(),
                    instrument: "piano".to_owned(),
                },
                Command::SetTempo { tick: 0, bpm: 96.0 },
            ],
        };
        let before = engine.project().clone();
        let preview = engine.preview_patch(&patch).unwrap();
        assert_eq!(preview.base_revision, 0);
        assert_eq!(preview.resulting_revision, 1);
        assert_eq!(preview.operation_count, 2);
        assert_eq!(preview.affected_tracks, vec!["harmony"]);
        assert_eq!(preview.description, "preview harmony");
        assert_eq!(engine.revision(), 0);
        assert_eq!(engine.project(), &before);
        assert!(engine.project().track("harmony").is_none());

        engine.apply_patch(patch).unwrap();
        assert_eq!(engine.revision(), 1);
        assert_eq!(engine.project().tempo_map.points[0].bpm, 96.0);
        assert!(engine.project().track("harmony").is_some());
    }

    #[test]
    fn patch_preview_rejects_stale_revision_and_invalid_operations() {
        let engine = ProjectEngine::new(Project::default());
        let stale = Patch {
            base_revision: Some(4),
            description: None,
            operations: Vec::new(),
        };
        assert!(matches!(
            engine.preview_patch(&stale),
            Err(ProjectError::RevisionConflict {
                expected: 4,
                actual: 0
            })
        ));

        let invalid = Patch {
            base_revision: Some(0),
            description: None,
            operations: vec![Command::AddNote {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                note: NoteEvent {
                    id: "invalid-preview".to_owned(),
                    start_tick: 0,
                    duration_tick: 0,
                    pitch: 60,
                    velocity: 90,
                },
            }],
        };
        assert!(matches!(
            engine.preview_patch(&invalid),
            Err(ProjectError::InvalidDuration)
        ));
    }

    #[test]
    fn clip_window_exposes_precise_event_ids_and_local_timeline_positions() {
        let mut engine = ProjectEngine::new(Project::default());
        engine
            .apply_patch(Patch {
                base_revision: Some(0),
                description: None,
                operations: vec![
                    Command::AddClip {
                        track_id: "piano".to_owned(),
                        clip_id: "offset".to_owned(),
                        start_tick: 3_840,
                        length_tick: 3_840,
                    },
                    Command::AddNote {
                        track_id: "piano".to_owned(),
                        clip_id: "offset".to_owned(),
                        note: NoteEvent {
                            id: "overlapping-note".to_owned(),
                            start_tick: 0,
                            duration_tick: 1_920,
                            pitch: 60,
                            velocity: 91,
                        },
                    },
                    Command::AddNote {
                        track_id: "piano".to_owned(),
                        clip_id: "offset".to_owned(),
                        note: NoteEvent {
                            id: "outside-note".to_owned(),
                            start_tick: 2_400,
                            duration_tick: 480,
                            pitch: 67,
                            velocity: 80,
                        },
                    },
                    Command::AddControl {
                        track_id: "piano".to_owned(),
                        clip_id: "offset".to_owned(),
                        control: ControlEvent {
                            id: "half-pedal".to_owned(),
                            tick: 480,
                            controller: 64,
                            value: 72,
                        },
                    },
                    Command::AddControl {
                        track_id: "piano".to_owned(),
                        clip_id: "offset".to_owned(),
                        control: ControlEvent {
                            id: "end-exclusive".to_owned(),
                            tick: 960,
                            controller: 64,
                            value: 0,
                        },
                    },
                ],
            })
            .unwrap();

        let window = engine
            .project()
            .clip_window("piano", "offset", 4_320, 4_800)
            .unwrap();
        assert_eq!(window.clip_start_tick, 3_840);
        assert_eq!(window.notes.len(), 1);
        assert_eq!(window.notes[0].id, "overlapping-note");
        assert_eq!(window.notes[0].local_start_tick, 0);
        assert_eq!(window.notes[0].absolute_start_tick, 3_840);
        assert_eq!(window.notes[0].pitch_name, "C4");
        assert_eq!(
            window.notes[0].start_position,
            MusicalPosition {
                bar: 2,
                beat: 1,
                tick_in_beat: 0,
            }
        );
        assert_eq!(
            window.notes[0].duration_quarters,
            TickRatio {
                numerator: 2,
                denominator: 1,
            }
        );
        assert_eq!(window.notes[0].common_duration, Some(CommonDuration::Half));
        assert_eq!(window.controls.len(), 1);
        assert_eq!(window.controls[0].id, "half-pedal");
        assert_eq!(window.controls[0].local_tick, 480);
        assert_eq!(window.controls[0].absolute_tick, 4_320);
        assert_eq!(
            window.controls[0].meaning,
            Some(MidiControlMeaning::SustainPedal)
        );
        assert_eq!(
            window.controls[0].position,
            MusicalPosition {
                bar: 2,
                beat: 1,
                tick_in_beat: 480,
            }
        );
        assert_eq!(window.ppq, 960);
        assert_eq!(window.beat_length_tick, 960);
        assert_eq!(window.bar_length_tick, 3_840);
    }

    #[test]
    fn musical_positions_keep_off_grid_timing_and_follow_the_project_meter() {
        let meter = TimeSignature {
            numerator: 6,
            denominator: 8,
        };
        assert_eq!(
            meter.musical_position(960, 3_120).unwrap(),
            MusicalPosition {
                bar: 2,
                beat: 1,
                tick_in_beat: 240,
            }
        );
        assert_eq!(midi_note_name(0), "C-1");
        assert_eq!(midi_note_name(60), "C4");
        assert_eq!(midi_note_name(127), "G9");
    }

    #[test]
    fn event_view_keeps_unusual_durations_exact_instead_of_normalizing_them() {
        let mut project = Project::default();
        project
            .midi_clip_mut("piano", "piano-main")
            .unwrap()
            .notes
            .push(NoteEvent {
                id: "free-duration".to_owned(),
                start_tick: 73,
                duration_tick: 701,
                pitch: 61,
                velocity: 65,
            });

        let window = project.clip_window("piano", "piano-main", 0, 960).unwrap();
        assert_eq!(window.notes[0].pitch_name, "C#4");
        assert_eq!(window.notes[0].start_position.tick_in_beat, 73);
        assert_eq!(
            window.notes[0].duration_quarters,
            TickRatio {
                numerator: 701,
                denominator: 960,
            }
        );
        assert_eq!(window.notes[0].common_duration, None);
    }

    #[test]
    fn clip_window_rejects_empty_or_backwards_ranges() {
        let project = Project::default();
        assert!(matches!(
            project.clip_window("piano", "piano-main", 960, 960),
            Err(ProjectError::InvalidTickWindow {
                start: 960,
                end: 960
            })
        ));
    }

    #[test]
    fn patch_is_atomic_and_undoes_as_one_change() {
        let mut engine = ProjectEngine::new(Project::default());
        let patch = Patch {
            base_revision: Some(0),
            description: Some("add chord".to_owned()),
            operations: vec![
                Command::AddNote {
                    track_id: "piano".to_owned(),
                    clip_id: "piano-main".to_owned(),
                    note: NoteEvent {
                        id: "c".to_owned(),
                        start_tick: 0,
                        duration_tick: 960,
                        pitch: 60,
                        velocity: 90,
                    },
                },
                Command::AddNote {
                    track_id: "piano".to_owned(),
                    clip_id: "piano-main".to_owned(),
                    note: NoteEvent {
                        id: "e".to_owned(),
                        start_tick: 0,
                        duration_tick: 960,
                        pitch: 64,
                        velocity: 90,
                    },
                },
            ],
        };
        let change = engine.apply_patch(patch).unwrap();
        assert_eq!(change.revision, 1);
        assert_eq!(engine.project().scheduled_notes().len(), 2);
        assert!(engine.undo());
        assert_eq!(engine.project().scheduled_notes().len(), 0);

        let invalid = Patch {
            base_revision: Some(1),
            description: None,
            operations: vec![Command::AddNote {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                note: NoteEvent {
                    id: "bad".to_owned(),
                    start_tick: 0,
                    duration_tick: 0,
                    pitch: 60,
                    velocity: 90,
                },
            }],
        };
        assert!(engine.apply_patch(invalid).is_err());
        assert_eq!(engine.project().scheduled_notes().len(), 0);
    }

    #[test]
    fn revision_survives_project_serialization() {
        let mut engine = ProjectEngine::new(Project::default());
        engine
            .apply(Command::SetTempo { tick: 0, bpm: 96.0 })
            .unwrap();
        let json = engine.project().to_pretty_json().unwrap();
        let loaded = Project::from_json(&json).unwrap();
        assert_eq!(loaded.revision, 1);
        assert_eq!(ProjectEngine::new(loaded).revision(), 1);
    }
}
