//! Neutral, deterministic observations about musical arrangement.
//!
//! This module deliberately does not score a composition or prescribe a
//! stylistic correction.  It reports recurring material, changes between
//! neighboring bars, contour intervals, and repeated expression controls so a
//! an evaluator can interpret against the brief without turning measurements
//! into a universal style rule.

use crate::TickRange;
use music_core::{Project, Tick, TimeSignature, TrackId, TrackSource};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const ARRANGEMENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ArrangementAnalysisError {
    #[error("project meter cannot be mapped to a bar grid: {0}")]
    InvalidMeter(String),
}

/// Stateless entry point for arrangement observations.  The project remains
/// the only source of truth; no edits or hidden normalization occur here.
#[derive(Clone, Copy, Debug, Default)]
pub struct ArrangementAnalyzer;

impl ArrangementAnalyzer {
    pub fn analyze(
        &self,
        project: &Project,
    ) -> Result<ArrangementReport, ArrangementAnalysisError> {
        let bar_length_tick = project
            .time_signature
            .bar_length_tick(project.ppq)
            .map_err(|error| ArrangementAnalysisError::InvalidMeter(error.to_string()))?;
        if bar_length_tick <= 0 {
            return Err(ArrangementAnalysisError::InvalidMeter(
                "bar length must be positive".to_owned(),
            ));
        }

        let materials = collect_material(project);
        let duration_tick = project.duration_tick().max(0);
        let end_tick = duration_tick.max(bar_length_tick);
        let bar_count = (end_tick + bar_length_tick - 1) / bar_length_tick;
        let profiles = build_profiles(&materials, bar_count, bar_length_tick);
        let mut findings = Vec::new();

        for material in &materials {
            findings.extend(repeated_material_findings(
                material,
                &profiles,
                bar_length_tick,
            ));
            findings.extend(contour_findings(material, bar_length_tick));
            findings.extend(expression_findings(material, &profiles, bar_length_tick));
        }
        findings.extend(transition_findings(project, &profiles, bar_length_tick));
        findings.sort_by_key(|finding| {
            (
                finding.location.range.start_tick,
                finding.location.range.end_tick,
                finding.category,
                finding.id.clone(),
            )
        });

        let active_bars = profiles
            .iter()
            .filter(|profile| profile.note_count > 0)
            .count();
        let total_notes = materials.iter().map(|material| material.notes.len()).sum();
        let total_controls = materials
            .iter()
            .map(|material| material.controls.len())
            .sum();
        let metrics = ArrangementMetrics {
            duration_tick,
            total_notes,
            total_controls,
            midi_track_count: project
                .tracks
                .iter()
                .filter(|track| matches!(track.source, TrackSource::Midi { .. }))
                .count(),
            active_bars,
            bar_count: bar_count as usize,
            tempo_point_count: project.tempo_map.points.len(),
            finding_count: findings.len(),
        };

        Ok(ArrangementReport {
            schema_version: ARRANGEMENT_SCHEMA_VERSION,
            project_revision: project.revision,
            ppq: project.ppq,
            time_signature: project.time_signature.clone(),
            analyzed_range: TickRange {
                start_tick: 0,
                end_tick,
            },
            semantics: ArrangementReportSemantics {
                findings_are_advisory: true,
                absence_is_not_a_quality_guarantee: true,
                application_may_be_blocked: false,
            },
            metrics,
            findings,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ArrangementReport {
    pub schema_version: u32,
    pub project_revision: u64,
    pub ppq: u16,
    pub time_signature: TimeSignature,
    pub analyzed_range: TickRange,
    pub semantics: ArrangementReportSemantics,
    pub metrics: ArrangementMetrics,
    /// Findings are questions for an evaluator, not automatic defects.
    pub findings: Vec<ArrangementFinding>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ArrangementReportSemantics {
    pub findings_are_advisory: bool,
    pub absence_is_not_a_quality_guarantee: bool,
    pub application_may_be_blocked: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ArrangementMetrics {
    pub duration_tick: Tick,
    pub total_notes: usize,
    pub total_controls: usize,
    pub midi_track_count: usize,
    pub active_bars: usize,
    pub bar_count: usize,
    pub tempo_point_count: usize,
    pub finding_count: usize,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum ArrangementFindingCategory {
    RepeatedMaterial,
    TransitionChange,
    ContourInterval,
    ExpressionPattern,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ArrangementFinding {
    pub id: String,
    pub category: ArrangementFindingCategory,
    pub location: ArrangementLocation,
    pub observation: String,
    pub evidence: Vec<ArrangementEvidence>,
    /// An open creative question; it intentionally does not prescribe a fix.
    pub creative_question: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ArrangementLocation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<TrackId>,
    pub range: TickRange,
    pub start_bar: i64,
    pub end_bar: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ArrangementEvidence {
    pub metric: String,
    pub value: String,
}

#[derive(Clone, Debug)]
struct Material {
    track_id: TrackId,
    notes: Vec<NotePoint>,
    controls: Vec<ControlPoint>,
}

#[derive(Clone, Copy, Debug)]
struct NotePoint {
    start_tick: Tick,
    end_tick: Tick,
    pitch: u8,
    velocity: u8,
}

#[derive(Clone, Copy, Debug)]
struct ControlPoint {
    tick: Tick,
    controller: u8,
    value: u8,
}

#[derive(Clone, Debug, Default)]
struct BarProfile {
    note_count: usize,
    onset_count: usize,
    min_pitch: Option<u8>,
    max_pitch: Option<u8>,
    mean_pitch: Option<f64>,
    max_polyphony: usize,
    median_gap: Option<Tick>,
    active_tracks: BTreeSet<TrackId>,
    rhythm_signature: Vec<(Tick, Vec<Tick>)>,
    velocity_direction_signature: Vec<i8>,
    pedal_signature: Vec<(Tick, u8, bool)>,
}

fn collect_material(project: &Project) -> Vec<Material> {
    let mut materials = Vec::new();
    for track in &project.tracks {
        let TrackSource::Midi { clips, .. } = &track.source else {
            continue;
        };
        let mut notes = Vec::new();
        let mut controls = Vec::new();
        for clip in clips {
            for note in &clip.notes {
                let start_tick = clip.start_tick + note.start_tick;
                notes.push(NotePoint {
                    start_tick,
                    end_tick: start_tick + note.duration_tick,
                    pitch: note.pitch,
                    velocity: note.velocity,
                });
            }
            for control in &clip.controls {
                controls.push(ControlPoint {
                    tick: clip.start_tick + control.tick,
                    controller: control.controller,
                    value: control.value,
                });
            }
        }
        notes.sort_by_key(|note| (note.start_tick, note.pitch));
        controls.sort_by_key(|control| (control.tick, control.controller));
        if !notes.is_empty() || !controls.is_empty() {
            materials.push(Material {
                track_id: track.id.clone(),
                notes,
                controls,
            });
        }
    }
    materials
}

fn build_profiles(
    materials: &[Material],
    bar_count: Tick,
    bar_length_tick: Tick,
) -> Vec<BarProfile> {
    let mut profiles = vec![BarProfile::default(); bar_count.max(1) as usize];
    for (index, profile) in profiles.iter_mut().enumerate() {
        let start = index as Tick * bar_length_tick;
        let end = start + bar_length_tick;
        let notes: Vec<_> = materials
            .iter()
            .flat_map(|material| material.notes.iter().copied())
            .filter(|note| note.start_tick >= start && note.start_tick < end)
            .collect();
        populate_note_profile(profile, &notes, start);
        for material in materials {
            if material
                .notes
                .iter()
                .any(|note| note.start_tick >= start && note.start_tick < end)
            {
                profile.active_tracks.insert(material.track_id.clone());
            }
        }
    }
    profiles
}

fn profile_for_material(
    material: &Material,
    bar_index: usize,
    bar_length_tick: Tick,
) -> BarProfile {
    let start = bar_index as Tick * bar_length_tick;
    let end = start + bar_length_tick;
    let notes: Vec<_> = material
        .notes
        .iter()
        .copied()
        .filter(|note| note.start_tick >= start && note.start_tick < end)
        .collect();
    let mut profile = BarProfile::default();
    if notes.is_empty() {
        profile.pedal_signature = pedal_signature(&material.controls, start, end);
        return profile;
    }

    populate_note_profile(&mut profile, &notes, start);
    profile.pedal_signature = pedal_signature(&material.controls, start, end);
    profile
}

fn populate_note_profile(profile: &mut BarProfile, notes: &[NotePoint], start: Tick) {
    profile.note_count = notes.len();
    if notes.is_empty() {
        return;
    }
    let mut by_onset: BTreeMap<Tick, Vec<Tick>> = BTreeMap::new();
    let mut pitch_sum = 0.0;
    for note in notes {
        let relative = note.start_tick - start;
        by_onset
            .entry(relative)
            .or_default()
            .push(note.end_tick - note.start_tick);
        profile.min_pitch = Some(
            profile
                .min_pitch
                .map_or(note.pitch, |value| value.min(note.pitch)),
        );
        profile.max_pitch = Some(
            profile
                .max_pitch
                .map_or(note.pitch, |value| value.max(note.pitch)),
        );
        pitch_sum += f64::from(note.pitch);
    }
    for durations in by_onset.values_mut() {
        durations.sort_unstable();
    }
    profile.onset_count = by_onset.len();
    profile.mean_pitch = Some(pitch_sum / notes.len() as f64);
    profile.rhythm_signature = by_onset.into_iter().collect();
    profile.max_polyphony = notes
        .iter()
        .map(|note| {
            notes
                .iter()
                .filter(|other| {
                    other.start_tick < note.end_tick && other.end_tick > note.start_tick
                })
                .count()
        })
        .max()
        .unwrap_or(0);
    let onsets: Vec<_> = profile
        .rhythm_signature
        .iter()
        .map(|(tick, _)| *tick)
        .collect();
    profile.median_gap = median_gap(&onsets);
    profile.velocity_direction_signature =
        velocity_direction_signature(notes, start, &profile.rhythm_signature);
}

fn build_track_profiles(
    material: &Material,
    bar_count: usize,
    bar_length_tick: Tick,
) -> Vec<BarProfile> {
    (0..bar_count)
        .map(|index| profile_for_material(material, index, bar_length_tick))
        .collect()
}

fn repeated_material_findings(
    material: &Material,
    all_profiles: &[BarProfile],
    bar_length_tick: Tick,
) -> Vec<ArrangementFinding> {
    let profiles = build_track_profiles(material, all_profiles.len(), bar_length_tick);
    let mut findings = Vec::new();
    let mut index = 0;
    while index < profiles.len() {
        if profiles[index].rhythm_signature.is_empty() {
            index += 1;
            continue;
        }
        let mut end = index + 1;
        while end < profiles.len()
            && profiles[end].rhythm_signature == profiles[index].rhythm_signature
        {
            end += 1;
        }
        if end - index >= 4 {
            let start_tick = index as Tick * bar_length_tick;
            let end_tick = end as Tick * bar_length_tick;
            findings.push(ArrangementFinding {
                id: finding_id("repeat", &material.track_id, start_tick, end_tick),
                category: ArrangementFindingCategory::RepeatedMaterial,
                location: location(
                    Some(material.track_id.clone()),
                    start_tick,
                    end_tick,
                    index as i64 + 1,
                    end as i64,
                ),
                observation: format!(
                    "track '{}' repeats the same onset/duration profile across {} consecutive bars",
                    material.track_id,
                    end - index
                ),
                evidence: vec![ArrangementEvidence {
                    metric: "rhythm_signature".to_owned(),
                    value: format_signature(&profiles[index].rhythm_signature),
                }],
                creative_question: "What role does this recurring profile play in the intended pulse, texture, or form?".to_owned(),
            });
            index = end;
        } else {
            index += 1;
        }
    }
    findings
}

fn transition_findings(
    project: &Project,
    profiles: &[BarProfile],
    bar_length_tick: Tick,
) -> Vec<ArrangementFinding> {
    let mut findings = Vec::new();
    for index in 0..profiles.len().saturating_sub(1) {
        let left = &profiles[index];
        let right = &profiles[index + 1];
        if left.note_count == 0 || right.note_count == 0 {
            continue;
        }
        let mut dimensions = Vec::new();
        if relative_change(left.note_count, right.note_count) >= 0.75 {
            dimensions.push(format!(
                "note_count {} -> {}",
                left.note_count, right.note_count
            ));
        }
        if mean_delta(left.mean_pitch, right.mean_pitch).abs() >= 7.0 {
            dimensions.push(format!(
                "mean_pitch {:.1} -> {:.1}",
                left.mean_pitch.unwrap_or_default(),
                right.mean_pitch.unwrap_or_default()
            ));
        }
        if gap_ratio(left.median_gap, right.median_gap) >= 2.0 {
            dimensions.push(format!(
                "median_onset_gap {:?} -> {:?}",
                left.median_gap, right.median_gap
            ));
        }
        if left.max_polyphony.abs_diff(right.max_polyphony) >= 2 {
            dimensions.push(format!(
                "max_polyphony {} -> {}",
                left.max_polyphony, right.max_polyphony
            ));
        }
        if left.active_tracks != right.active_tracks {
            dimensions.push(format!(
                "active_tracks {:?} -> {:?}",
                left.active_tracks, right.active_tracks
            ));
        }
        let left_bpm = bpm_at(project, index as Tick * bar_length_tick);
        let right_bpm = bpm_at(project, (index + 1) as Tick * bar_length_tick);
        if relative_f64_change(left_bpm, right_bpm) >= 0.08 {
            dimensions.push(format!("bpm {:.1} -> {:.1}", left_bpm, right_bpm));
        }
        if dimensions.len() < 2 {
            continue;
        }
        let start_tick = index as Tick * bar_length_tick;
        let end_tick = start_tick + bar_length_tick * 2;
        findings.push(ArrangementFinding {
            id: finding_id("transition", "project", start_tick, end_tick),
            category: ArrangementFindingCategory::TransitionChange,
            location: location(
                None,
                start_tick,
                end_tick,
                index as i64 + 1,
                index as i64 + 2,
            ),
            observation: format!(
                "the boundary between bars {} and {} changes {} independent dimensions",
                index + 1,
                index + 2,
                dimensions.len()
            ),
            evidence: dimensions
                .into_iter()
                .map(|value| ArrangementEvidence {
                    metric: "boundary_measurement".to_owned(),
                    value,
                })
                .collect(),
            creative_question:
                "How does this simultaneous contrast relate to the intended formal boundary?"
                    .to_owned(),
        });
    }
    findings
}

fn contour_findings(material: &Material, bar_length_tick: Tick) -> Vec<ArrangementFinding> {
    if material.notes.len() < 3 {
        return Vec::new();
    }
    let mut pitches: Vec<u8> = material.notes.iter().map(|note| note.pitch).collect();
    pitches.sort_unstable();
    let median = pitches[pitches.len() / 2];
    let threshold = median.max(60);
    let mut upper_by_onset: BTreeMap<Tick, u8> = BTreeMap::new();
    for note in &material.notes {
        if note.pitch >= threshold {
            upper_by_onset
                .entry(note.start_tick)
                .and_modify(|pitch| *pitch = (*pitch).max(note.pitch))
                .or_insert(note.pitch);
        }
    }
    let points: Vec<_> = upper_by_onset.into_iter().collect();
    let mut findings = Vec::new();
    for index in 1..points.len().saturating_sub(1) {
        let previous = points[index - 1];
        let current = points[index];
        let next = points[index + 1];
        let interval = i16::from(current.1) - i16::from(previous.1);
        if interval.unsigned_abs() < 9 {
            continue;
        }
        let next_interval = i16::from(next.1) - i16::from(current.1);
        if next_interval.unsigned_abs() > 7 {
            continue;
        }
        let start_tick = previous.0;
        let end_tick = (next.0 + bar_length_tick / 8).max(current.0 + 1);
        let start_bar = start_tick / bar_length_tick + 1;
        let end_bar = end_tick / bar_length_tick + 1;
        findings.push(ArrangementFinding {
            id: finding_id("contour", &material.track_id, current.0, end_tick),
            category: ArrangementFindingCategory::ContourInterval,
            location: location(
                Some(material.track_id.clone()),
                start_tick,
                end_tick,
                start_bar,
                end_bar,
            ),
            observation: format!(
                "upper-register onset contour moves {} semitones at tick {}",
                interval, current.0
            ),
            evidence: vec![ArrangementEvidence {
                metric: "neighboring_pitches".to_owned(),
                value: format!(
                    "{} -> {} -> {} (next interval {} semitones)",
                    previous.1, current.1, next.1, next_interval
                ),
            }],
            creative_question:
                "What musical role does this contour interval serve in the intended phrase?"
                    .to_owned(),
        });
    }
    findings
}

fn expression_findings(
    material: &Material,
    all_profiles: &[BarProfile],
    bar_length_tick: Tick,
) -> Vec<ArrangementFinding> {
    let profiles = build_track_profiles(material, all_profiles.len(), bar_length_tick);
    let mut findings = velocity_direction_findings(material, &profiles, bar_length_tick);
    findings.extend(pedal_pattern_findings(material, &profiles, bar_length_tick));
    findings
}

fn velocity_direction_findings(
    material: &Material,
    profiles: &[BarProfile],
    bar_length_tick: Tick,
) -> Vec<ArrangementFinding> {
    let mut findings = Vec::new();
    let mut index = 0;
    while index < profiles.len() {
        let signature = &profiles[index].velocity_direction_signature;
        if signature.is_empty() {
            index += 1;
            continue;
        }
        let mut end = index + 1;
        while end < profiles.len() && profiles[end].velocity_direction_signature == *signature {
            end += 1;
        }
        if end - index >= 4 {
            let start_tick = index as Tick * bar_length_tick;
            let end_tick = end as Tick * bar_length_tick;
            findings.push(ArrangementFinding {
                id: finding_id(
                    "expression_velocity",
                    &material.track_id,
                    start_tick,
                    end_tick,
                ),
                category: ArrangementFindingCategory::ExpressionPattern,
                location: location(
                    Some(material.track_id.clone()),
                    start_tick,
                    end_tick,
                    index as i64 + 1,
                    end as i64,
                ),
                observation: format!(
                    "the same rise/fall velocity direction repeats across {} consecutive bars",
                    end - index
                ),
                evidence: vec![ArrangementEvidence {
                    metric: "velocity_direction_signature".to_owned(),
                    value: format_velocity_directions(signature),
                }],
                creative_question: "What role does this recurring dynamic contour play in the intended performance identity?".to_owned(),
            });
            index = end;
        } else {
            index += 1;
        }
    }
    findings
}

fn pedal_pattern_findings(
    material: &Material,
    profiles: &[BarProfile],
    bar_length_tick: Tick,
) -> Vec<ArrangementFinding> {
    let mut findings = Vec::new();
    let mut index = 0;
    while index < profiles.len() {
        let signature = &profiles[index].pedal_signature;
        if signature.is_empty() {
            index += 1;
            continue;
        }
        let mut end = index + 1;
        while end < profiles.len() && profiles[end].pedal_signature == *signature {
            end += 1;
        }
        if end - index >= 4 {
            let start_tick = index as Tick * bar_length_tick;
            let end_tick = end as Tick * bar_length_tick;
            findings.push(ArrangementFinding {
                id: finding_id(
                    "expression_pedal",
                    &material.track_id,
                    start_tick,
                    end_tick,
                ),
                category: ArrangementFindingCategory::ExpressionPattern,
                location: location(
                    Some(material.track_id.clone()),
                    start_tick,
                    end_tick,
                    index as i64 + 1,
                    end as i64,
                ),
                observation: format!(
                    "the same pedal controller timing repeats across {} consecutive bars",
                    end - index
                ),
                evidence: vec![ArrangementEvidence {
                    metric: "pedal_signature".to_owned(),
                    value: format!("{signature:?}"),
                }],
                creative_question: "What role does this recurring pedal cycle play in the intended resonance and texture?".to_owned(),
            });
            index = end;
        } else {
            index += 1;
        }
    }
    findings
}

fn velocity_direction_signature(
    notes: &[NotePoint],
    start: Tick,
    rhythm: &[(Tick, Vec<Tick>)],
) -> Vec<i8> {
    let onset_velocities: Vec<_> = rhythm
        .iter()
        .map(|(onset, _)| {
            notes
                .iter()
                .filter(|note| note.start_tick == start + *onset)
                .map(|note| note.velocity)
                .max()
                .unwrap_or_default()
        })
        .collect();
    onset_velocities
        .windows(2)
        .map(|pair| match pair[1].cmp(&pair[0]) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        })
        .collect()
}

fn format_velocity_directions(signature: &[i8]) -> String {
    signature
        .iter()
        .map(|direction| match direction {
            -1 => "fall",
            0 => "level",
            1 => "rise",
            _ => unreachable!("velocity directions are normalized"),
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn pedal_signature(controls: &[ControlPoint], start: Tick, end: Tick) -> Vec<(Tick, u8, bool)> {
    controls
        .iter()
        .filter(|control| {
            control.tick >= start
                && control.tick < end
                && matches!(control.controller, 64 | 66 | 67)
        })
        .map(|control| {
            (
                control.tick - start,
                control.controller,
                control.value >= 64,
            )
        })
        .collect()
}

fn median_gap(onsets: &[Tick]) -> Option<Tick> {
    if onsets.len() < 2 {
        return None;
    }
    let mut gaps: Vec<_> = onsets.windows(2).map(|pair| pair[1] - pair[0]).collect();
    gaps.sort_unstable();
    Some(gaps[gaps.len() / 2])
}

fn bpm_at(project: &Project, tick: Tick) -> f64 {
    project
        .tempo_map
        .points
        .iter()
        .take_while(|point| point.tick <= tick)
        .last()
        .map(|point| point.bpm)
        .unwrap_or(120.0)
}

fn relative_change(left: usize, right: usize) -> f64 {
    let denominator = left.max(right).max(1) as f64;
    left.abs_diff(right) as f64 / denominator
}

fn relative_f64_change(left: f64, right: f64) -> f64 {
    (left - right).abs() / left.max(right).max(1.0)
}

fn mean_delta(left: Option<f64>, right: Option<f64>) -> f64 {
    right.unwrap_or_default() - left.unwrap_or_default()
}

fn gap_ratio(left: Option<Tick>, right: Option<Tick>) -> f64 {
    let Some(left) = left else { return 0.0 };
    let Some(right) = right else { return 0.0 };
    let larger = left.max(right).max(1) as f64;
    let smaller = left.min(right).max(1) as f64;
    larger / smaller
}

fn format_signature(signature: &[(Tick, Vec<Tick>)]) -> String {
    signature
        .iter()
        .map(|(onset, durations)| format!("{}:[{}]", onset, join_ticks(durations)))
        .collect::<Vec<_>>()
        .join(",")
}

fn join_ticks(values: &[Tick]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("/")
}

fn location(
    track_id: Option<TrackId>,
    start_tick: Tick,
    end_tick: Tick,
    start_bar: i64,
    end_bar: i64,
) -> ArrangementLocation {
    ArrangementLocation {
        track_id,
        range: TickRange {
            start_tick,
            end_tick: end_tick.max(start_tick + 1),
        },
        start_bar,
        end_bar,
    }
}

fn finding_id(prefix: &str, track_id: &str, start_tick: Tick, end_tick: Tick) -> String {
    format!(
        "arrangement-{prefix}-{}-{start_tick}-{end_tick}",
        track_id
            .chars()
            .map(|character| if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            })
            .collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_core::{Clip, ControlEvent, MixerSettings, NoteEvent, TempoMap, TempoPoint, Track};

    fn project_with_notes(notes: Vec<NoteEvent>, controls: Vec<ControlEvent>) -> Project {
        Project {
            tempo_map: TempoMap {
                points: vec![TempoPoint { tick: 0, bpm: 80.0 }],
            },
            tracks: vec![Track {
                id: "piano".to_owned(),
                name: "Piano".to_owned(),
                source: TrackSource::Midi {
                    instrument: "piano".to_owned(),
                    clips: vec![Clip {
                        id: "main".to_owned(),
                        start_tick: 0,
                        length_tick: 960 * 16,
                        notes,
                        controls,
                    }],
                },
                mixer: MixerSettings::default(),
            }],
            ..Project::default()
        }
    }

    fn note(id: &str, start_tick: Tick, duration_tick: Tick, pitch: u8, velocity: u8) -> NoteEvent {
        NoteEvent {
            id: id.to_owned(),
            start_tick,
            duration_tick,
            pitch,
            velocity,
        }
    }

    #[test]
    fn reports_repetition_without_calling_it_bad() {
        let bar = 3_840;
        let mut notes = Vec::new();
        for bar_index in 0..5 {
            let start = bar_index * bar;
            notes.push(note(
                &format!("a{bar_index}"),
                start,
                960,
                48 + bar_index as u8,
                70,
            ));
            notes.push(note(
                &format!("b{bar_index}"),
                start + 1_920,
                960,
                60 + bar_index as u8,
                80,
            ));
        }
        let report = ArrangementAnalyzer
            .analyze(&project_with_notes(notes, Vec::new()))
            .unwrap();
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.category == ArrangementFindingCategory::RepeatedMaterial)
            .expect("repetition should be observable");
        assert!(finding.observation.contains("same onset/duration profile"));
        assert!(finding.creative_question.contains("intended pulse"));
        assert!(!finding.observation.contains("bad"));
    }

    #[test]
    fn reports_multidimensional_boundary_change_as_a_question() {
        let bar = 3_840;
        let mut notes = Vec::new();
        for index in 0..2 {
            notes.push(note(&format!("slow{index}"), index * bar, 1_920, 48, 60));
        }
        for index in 0..2 {
            let start = (index + 2) * bar;
            for step in 0..8 {
                notes.push(note(
                    &format!("fast{index}-{step}"),
                    start + step * 480,
                    360,
                    72 + step as u8,
                    90,
                ));
            }
        }
        let mut project = project_with_notes(notes, Vec::new());
        project.tempo_map.points.push(TempoPoint {
            tick: 2 * bar,
            bpm: 120.0,
        });
        let report = ArrangementAnalyzer.analyze(&project).unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.category == ArrangementFindingCategory::TransitionChange)
        );
    }

    #[test]
    fn expression_observation_does_not_require_variation() {
        let bar = 3_840;
        let mut notes = Vec::new();
        let mut controls = Vec::new();
        for index in 0..4 {
            let start = index * bar;
            notes.push(note(&format!("note{index}"), start, 960, 60, 80));
            controls.push(ControlEvent {
                id: format!("down{index}"),
                tick: start,
                controller: 64,
                value: 127,
            });
            controls.push(ControlEvent {
                id: format!("up{index}"),
                tick: start + 3_600,
                controller: 64,
                value: 0,
            });
        }
        let report = ArrangementAnalyzer
            .analyze(&project_with_notes(notes, controls))
            .unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.category == ArrangementFindingCategory::ExpressionPattern)
        );
    }

    #[test]
    fn report_declares_advisory_non_gating_semantics() {
        let report = ArrangementAnalyzer
            .analyze(&project_with_notes(Vec::new(), Vec::new()))
            .unwrap();
        assert_eq!(report.metrics.midi_track_count, 1);
        assert_eq!(report.metrics.total_notes, 0);
        assert!(report.semantics.findings_are_advisory);
        assert!(report.semantics.absence_is_not_a_quality_guarantee);
        assert!(!report.semantics.application_may_be_blocked);
    }

    #[test]
    fn velocity_shape_uses_bar_relative_onsets() {
        let bar = 3_840;
        let mut notes = Vec::new();
        for index in 0..4 {
            let start = index * bar;
            notes.push(note(&format!("a{index}"), start, 480, 60, 72));
            notes.push(note(&format!("b{index}"), start + 960, 480, 64, 88));
        }
        let report = ArrangementAnalyzer
            .analyze(&project_with_notes(notes, Vec::new()))
            .unwrap();
        assert!(report.findings.iter().any(|finding| {
            finding.category == ArrangementFindingCategory::ExpressionPattern
                && finding.evidence.iter().any(|evidence| {
                    evidence.metric == "velocity_direction_signature" && evidence.value == "rise"
                })
        }));
    }
}
