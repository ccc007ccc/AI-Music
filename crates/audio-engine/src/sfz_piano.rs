//! Strict, preload-only SFZ sampler for acoustic-piano asset packs.
//!
//! The first implementation intentionally supports a small, audible subset of
//! SFZ. Unsupported directives, headers, and opcodes fail at load time instead
//! of being silently ignored. All samples are decoded and cached before an
//! [`InstrumentSession`] is created, so rendering never performs file I/O.

use super::{
    AssetPack, AssetPackEngine, Instrument, InstrumentError, InstrumentEvent, InstrumentSession,
};
use crate::sfz_preprocess;
use std::collections::{BTreeMap, HashMap};
use std::f32::consts::FRAC_PI_4;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

const MAX_VOICES: usize = 128;
const MIN_RELEASE_SECONDS: f32 = 0.003;

#[derive(Debug, Error)]
pub enum SfzPianoError {
    #[error("asset pack engine must be 'sfz', found {0:?}")]
    WrongEngine(AssetPackEngine),
    #[error("could not read SFZ definition {path:?}: {source}")]
    ReadDefinition {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not preprocess SFZ definition {path:?}: {message}")]
    Preprocess { path: PathBuf, message: String },
    #[error("SFZ line {line}: directive '{directive}' is not supported yet")]
    UnsupportedDirective { line: usize, directive: String },
    #[error("SFZ line {line}: header '<{header}>' is not supported yet")]
    UnsupportedHeader { line: usize, header: String },
    #[error("SFZ line {line}: opcode '{opcode}' is not supported by the piano sampler")]
    UnsupportedOpcode { line: usize, opcode: String },
    #[error("SFZ line {line}: opcode '{opcode}' is not valid in <{header}>")]
    InvalidOpcodeScope {
        line: usize,
        opcode: String,
        header: &'static str,
    },
    #[error("SFZ line {line}: expected an opcode assignment near '{text}'")]
    InvalidSyntax { line: usize, text: String },
    #[error("SFZ line {line}: variables are not supported yet in '{text}'")]
    UnsupportedVariable { line: usize, text: String },
    #[error("SFZ line {line}: opcode '{opcode}' has invalid value '{value}': {reason}")]
    InvalidOpcodeValue {
        line: usize,
        opcode: String,
        value: String,
        reason: String,
    },
    #[error("SFZ line {line}: region has no sample opcode")]
    MissingSample { line: usize },
    #[error("SFZ line {line}: modulation references undefined custom curve {curve}")]
    UndefinedCurve { line: usize, curve: u16 },
    #[error("SFZ does not contain any playable regions")]
    NoRegions,
    #[error("SFZ line {line}: sample path must stay inside the asset pack: {path:?}")]
    InvalidSamplePath { line: usize, path: PathBuf },
    #[error("SFZ line {line}: sample resolves outside the asset pack: {path:?}")]
    SampleEscapesPack { line: usize, path: PathBuf },
    #[error("could not open sample {path:?} referenced on SFZ line {line}: {source}")]
    ReadSample {
        line: usize,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not decode sample {path:?}: {message}")]
    DecodeSample { path: PathBuf, message: String },
    #[error("sample {path:?} uses unsupported format '{extension}'")]
    UnsupportedSampleFormat { path: PathBuf, extension: String },
    #[error("sample {path:?} must be mono or stereo, found {channels} channels")]
    UnsupportedChannelCount { path: PathBuf, channels: u32 },
    #[error("sample contains no audio frames: {0:?}")]
    EmptySample(PathBuf),
}

#[derive(Clone, Debug)]
pub struct SfzPiano {
    regions: Arc<[SampleRegion]>,
    sample_count: usize,
    decoded_sample_bytes: usize,
    tail_seconds: f32,
    initial_cc: [u8; 128],
    curves: Arc<BTreeMap<u16, CurveDefinition>>,
}

impl SfzPiano {
    /// Loads a validated SFZ asset pack and decodes all referenced samples.
    pub fn from_asset_pack(pack: &AssetPack) -> Result<Self, SfzPianoError> {
        Self::from_asset_pack_for_notes(pack, None)
    }

    /// Loads only the regions reachable by the supplied piano performances.
    /// This keeps large sample libraries practical for offline project renders
    /// while preserving preload-only behavior inside the resulting session.
    pub fn from_asset_pack_for_performance(
        pack: &AssetPack,
        notes: &[(u8, u8)],
    ) -> Result<Self, SfzPianoError> {
        Self::from_asset_pack_for_notes(pack, Some(notes))
    }

    fn from_asset_pack_for_notes(
        pack: &AssetPack,
        notes: Option<&[(u8, u8)]>,
    ) -> Result<Self, SfzPianoError> {
        if pack.manifest().engine != AssetPackEngine::Sfz {
            return Err(SfzPianoError::WrongEngine(pack.manifest().engine));
        }

        let source = sfz_preprocess::expand(
            pack.entry_path(),
            pack.manifest_path()
                .parent()
                .unwrap_or_else(|| Path::new(".")),
        )
        .map_err(|error| SfzPianoError::Preprocess {
            path: pack.entry_path().to_owned(),
            message: error.to_string(),
        })?;
        let definition = parse_definition(&source)?;
        let pack_root = pack
            .manifest_path()
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let sfz_root = pack.entry_path().parent().unwrap_or_else(|| Path::new("."));
        let mut cache = HashMap::<PathBuf, Arc<DecodedSample>>::new();
        let mut regions = Vec::with_capacity(definition.regions.len());

        let default_keyswitch = default_keyswitch(&definition.regions)?;
        for raw in &definition.regions {
            if !region_is_selected(raw, default_keyswitch)? {
                continue;
            }
            validate_curve_references(raw, &definition.curves)?;
            if let Some(notes) = notes
                && !raw_region_matches_performance(raw, notes)?
            {
                continue;
            }
            regions.push(load_region(
                raw,
                &definition.default_path,
                sfz_root,
                pack_root,
                &mut cache,
            )?);
        }
        if regions.is_empty() && (notes.is_none() || notes.is_some_and(|notes| !notes.is_empty())) {
            return Err(SfzPianoError::NoRegions);
        }

        let decoded_sample_bytes = cache
            .values()
            .map(|sample| (sample.left.len() + sample.right.len()).saturating_mul(size_of::<f32>()))
            .sum();
        let release_tail = regions
            .iter()
            .map(|region| region.release_seconds)
            .fold(0.0_f32, f32::max);
        let release_sample_tail = regions
            .iter()
            .filter(|region| region.trigger != Trigger::Attack)
            .map(SampleRegion::maximum_duration_seconds)
            .fold(0.0_f32, f32::max);

        Ok(Self {
            regions: regions.into(),
            sample_count: cache.len(),
            decoded_sample_bytes,
            tail_seconds: release_tail.max(release_sample_tail).max(0.5) + 0.05,
            initial_cc: definition.initial_cc,
            curves: Arc::new(definition.curves),
        })
    }

    pub fn region_count(&self) -> usize {
        self.regions.len()
    }

    pub fn sample_count(&self) -> usize {
        self.sample_count
    }

    pub fn decoded_sample_bytes(&self) -> usize {
        self.decoded_sample_bytes
    }
}

impl Instrument for SfzPiano {
    fn create_session(
        &self,
        sample_rate: u32,
    ) -> Result<Box<dyn InstrumentSession>, InstrumentError> {
        if !(8_000..=384_000).contains(&sample_rate) {
            return Err(InstrumentError::Backend(format!(
                "unsupported sampler rate {sample_rate} Hz"
            )));
        }
        Ok(Box::new(SfzPianoSession::new(
            self.regions.clone(),
            sample_rate,
            self.initial_cc,
            self.curves.clone(),
        )))
    }

    fn tail_seconds(&self) -> f32 {
        self.tail_seconds
    }
}

#[derive(Clone, Debug)]
struct RawValue {
    value: String,
    line: usize,
}

#[derive(Clone, Debug)]
struct RawRegion {
    values: BTreeMap<String, RawValue>,
    line: usize,
}

#[derive(Debug)]
struct ParsedDefinition {
    default_path: PathBuf,
    regions: Vec<RawRegion>,
    initial_cc: [u8; 128],
    curves: BTreeMap<u16, CurveDefinition>,
}

#[derive(Clone, Debug, Default)]
struct CurveDefinition {
    points: BTreeMap<u8, f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Header {
    Control,
    Global,
    Master,
    Group,
    Region,
    Curve,
}

impl Header {
    fn name(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Global => "global",
            Self::Master => "master",
            Self::Group => "group",
            Self::Region => "region",
            Self::Curve => "curve",
        }
    }
}

#[derive(Debug)]
enum Lexeme {
    Header {
        name: String,
        line: usize,
    },
    Opcode {
        name: String,
        value: String,
        line: usize,
    },
}

fn parse_definition(source: &str) -> Result<ParsedDefinition, SfzPianoError> {
    let lexemes = lex(source)?;
    let mut header = None;
    let mut global = BTreeMap::new();
    let mut master = BTreeMap::new();
    let mut group = BTreeMap::new();
    let mut region: Option<RawRegion> = None;
    let mut regions = Vec::new();
    let mut default_path = PathBuf::new();
    let mut initial_cc = [0_u8; 128];
    let mut curve_points = BTreeMap::<u16, BTreeMap<u8, RawValue>>::new();
    let mut current_curve_index = None;

    for lexeme in lexemes {
        match lexeme {
            Lexeme::Header { name, line } => {
                flush_region(&mut region, &mut regions);
                header = Some(match name.as_str() {
                    "control" => Header::Control,
                    "global" => {
                        global.clear();
                        master.clear();
                        group.clear();
                        Header::Global
                    }
                    "master" => {
                        master.clear();
                        group.clear();
                        Header::Master
                    }
                    "group" => {
                        group.clear();
                        Header::Group
                    }
                    "region" => {
                        let mut values = global.clone();
                        values.extend(master.clone());
                        values.extend(group.clone());
                        region = Some(RawRegion { values, line });
                        Header::Region
                    }
                    "curve" => {
                        current_curve_index = None;
                        Header::Curve
                    }
                    _ => {
                        return Err(SfzPianoError::UnsupportedHeader { line, header: name });
                    }
                });
            }
            Lexeme::Opcode { name, value, line } => {
                let Some(current_header) = header else {
                    return Err(SfzPianoError::InvalidSyntax {
                        line,
                        text: format!("{name}={value}"),
                    });
                };
                if name.contains('$') || value.contains('$') {
                    return Err(SfzPianoError::UnsupportedVariable {
                        line,
                        text: format!("{name}={value}"),
                    });
                }
                let raw = RawValue { value, line };
                match current_header {
                    Header::Control => {
                        if name == "default_path" {
                            default_path = PathBuf::from(raw.value);
                        } else if name.starts_with("set_cc") {
                            let controller =
                                parse_control_name(&name, "set_cc").ok_or_else(|| {
                                    invalid_value(
                                        &raw,
                                        &name,
                                        "expected a controller suffix from 0 through 127",
                                    )
                                })?;
                            initial_cc[controller as usize] = parse_cc_value(&raw, false)?;
                        } else if name.starts_with("set_hdcc") {
                            let controller =
                                parse_control_name(&name, "set_hdcc").ok_or_else(|| {
                                    invalid_value(
                                        &raw,
                                        &name,
                                        "expected a controller suffix from 0 through 127",
                                    )
                                })?;
                            initial_cc[controller as usize] = parse_cc_value(&raw, true)?;
                        } else if !is_metadata_opcode(&name) {
                            validate_opcode(&name, line, current_header)?;
                        }
                    }
                    Header::Curve => {
                        if name == "curve_index" {
                            let index = raw.value.parse::<u16>().map_err(|_| {
                                invalid_value(&raw, "curve_index", "expected an integer")
                            })?;
                            if index > 254 {
                                return Err(invalid_value(
                                    &raw,
                                    "curve_index",
                                    "expected a value from 0 through 254",
                                ));
                            }
                            current_curve_index = Some(index);
                            curve_points.entry(index).or_default();
                        } else if let Some(point) = parse_curve_point_name(&name) {
                            let Some(index) = current_curve_index else {
                                return Err(SfzPianoError::InvalidSyntax {
                                    line,
                                    text: format!("{name}={}", raw.value),
                                });
                            };
                            let value = raw
                                .value
                                .parse::<f32>()
                                .map_err(|_| invalid_value(&raw, &name, "expected a number"))?;
                            if !value.is_finite() {
                                return Err(invalid_value(
                                    &raw,
                                    &name,
                                    "expected a finite curve value",
                                ));
                            }
                            curve_points.entry(index).or_default().insert(point, raw);
                        } else {
                            return Err(SfzPianoError::UnsupportedOpcode { line, opcode: name });
                        }
                    }
                    _ => {
                        if is_metadata_opcode(&name) {
                            continue;
                        }
                        validate_opcode(&name, line, current_header)?;
                        match current_header {
                            Header::Global => {
                                global.insert(name, raw);
                            }
                            Header::Master => {
                                master.insert(name, raw);
                            }
                            Header::Group => {
                                group.insert(name, raw);
                            }
                            Header::Region => {
                                region
                                    .as_mut()
                                    .expect("region state exists after a region header")
                                    .values
                                    .insert(name, raw);
                            }
                            Header::Control | Header::Curve => unreachable!(),
                        }
                    }
                }
            }
        }
    }
    flush_region(&mut region, &mut regions);
    if regions.is_empty() {
        return Err(SfzPianoError::NoRegions);
    }
    Ok(ParsedDefinition {
        default_path,
        regions,
        initial_cc,
        curves: curve_points
            .into_iter()
            .map(|(index, points)| {
                (
                    index,
                    CurveDefinition {
                        points: points
                            .into_iter()
                            .map(|(point, raw)| {
                                (point, raw.value.parse::<f32>().unwrap_or_default())
                            })
                            .collect(),
                    },
                )
            })
            .collect(),
    })
}

fn flush_region(region: &mut Option<RawRegion>, regions: &mut Vec<RawRegion>) {
    if let Some(region) = region.take() {
        regions.push(region);
    }
}

fn parse_control_name(name: &str, prefix: &str) -> Option<u8> {
    let suffix = name.strip_prefix(prefix)?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    suffix
        .parse::<u16>()
        .ok()
        .filter(|value| *value <= 127)
        .map(|value| value as u8)
}

fn parse_cc_value(raw: &RawValue, high_definition: bool) -> Result<u8, SfzPianoError> {
    if high_definition {
        let value = raw
            .value
            .parse::<f32>()
            .map_err(|_| invalid_value(raw, "set_hdcc", "expected a number from 0 through 1"))?;
        if !(0.0..=1.0).contains(&value) || !value.is_finite() {
            return Err(invalid_value(
                raw,
                "set_hdcc",
                "expected a number from 0 through 1",
            ));
        }
        Ok((value * 127.0).round() as u8)
    } else {
        let value = raw
            .value
            .parse::<u16>()
            .map_err(|_| invalid_value(raw, "set_cc", "expected an integer from 0 through 127"))?;
        if value > 127 {
            return Err(invalid_value(
                raw,
                "set_cc",
                "expected an integer from 0 through 127",
            ));
        }
        Ok(value as u8)
    }
}

fn parse_curve_point_name(name: &str) -> Option<u8> {
    let suffix = name.strip_prefix('v')?;
    if suffix.len() != 3 || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    suffix
        .parse::<u16>()
        .ok()
        .filter(|value| *value <= 127)
        .map(|value| value as u8)
}

fn validate_opcode(name: &str, line: usize, header: Header) -> Result<(), SfzPianoError> {
    const PLAYBACK_OPCODES: &[&str] = &[
        "sample",
        "key",
        "lokey",
        "hikey",
        "lovel",
        "hivel",
        "pitch_keycenter",
        "tune",
        "volume",
        "amplitude",
        "pan",
        "amp_veltrack",
        "ampeg_attack",
        "ampeg_release",
        "trigger",
        "offset",
        "end",
        "sw_last",
        "sw_lokey",
        "sw_hikey",
        "sw_default",
        "group",
        "polyphony",
        "note_polyphony",
        "off_time",
        "rt_decay",
        "lorand",
        "hirand",
    ];

    if header == Header::Control {
        if name == "default_path" {
            return Ok(());
        }
        return Err(SfzPianoError::InvalidOpcodeScope {
            line,
            opcode: name.to_owned(),
            header: header.name(),
        });
    }
    if name == "default_path" {
        return Err(SfzPianoError::InvalidOpcodeScope {
            line,
            opcode: name.to_owned(),
            header: header.name(),
        });
    }
    if PLAYBACK_OPCODES.contains(&name) || dynamic_opcode_name(name) {
        Ok(())
    } else {
        Err(SfzPianoError::UnsupportedOpcode {
            line,
            opcode: name.to_owned(),
        })
    }
}

fn dynamic_opcode_name(name: &str) -> bool {
    [
        "locc",
        "hicc",
        "on_locc",
        "on_hicc",
        "amplitude_oncc",
        "pan_oncc",
        "amp_veltrack_oncc",
        "ampeg_release_oncc",
        "ampeg_attack_oncc",
        "offset_oncc",
        "amplitude_curvecc",
        "pan_curvecc",
        "amp_veltrack_curvecc",
        "ampeg_attack_curvecc",
        "ampeg_release_curvecc",
        "offset_curvecc",
    ]
    .iter()
    .any(|prefix| {
        name.strip_prefix(prefix)
            .and_then(|suffix| suffix.parse::<u16>().ok())
            .is_some_and(|controller| controller <= 127)
    })
}

fn is_metadata_opcode(name: &str) -> bool {
    name.ends_with("_label") || name.starts_with("label_cc")
}

fn default_keyswitch(regions: &[RawRegion]) -> Result<Option<u8>, SfzPianoError> {
    let mut default = None;
    let mut first_switch = None;
    for raw in regions {
        for name in ["sw_default", "sw_last"] {
            let Some(raw_value) = raw.values.get(name) else {
                continue;
            };
            let note = parse_midi_note(&raw_value.value).ok_or_else(|| {
                invalid_value(raw_value, name, "expected MIDI note 0..127 or a note name")
            })?;
            if name == "sw_default" {
                if default.is_some_and(|existing| existing != note) {
                    return Err(invalid_value(
                        raw_value,
                        name,
                        "all inherited keyswitch defaults must agree",
                    ));
                }
                default = Some(note);
            } else if first_switch.is_none() {
                first_switch = Some(note);
            }
        }
    }
    Ok(default.or(first_switch))
}

fn region_is_selected(
    raw: &RawRegion,
    default_keyswitch: Option<u8>,
) -> Result<bool, SfzPianoError> {
    let disabled_key = raw
        .values
        .get("key")
        .is_some_and(|value| value.value.trim() == "-1")
        || raw
            .values
            .get("lokey")
            .is_some_and(|value| value.value.trim() == "-1");
    if disabled_key
        && !raw.values.keys().any(|name| {
            parse_dynamic_controller(name, "on_locc").is_some()
                || parse_dynamic_controller(name, "on_hicc").is_some()
        })
    {
        let raw_value = raw
            .values
            .get("key")
            .or_else(|| raw.values.get("lokey"))
            .expect("disabled key was found");
        return Err(invalid_value(
            raw_value,
            "key",
            "key=-1 is only supported for an on_locc/on_hicc control-triggered region",
        ));
    }
    let Some(raw_value) = raw.values.get("sw_last") else {
        return Ok(true);
    };
    let switch = parse_midi_note(&raw_value.value).ok_or_else(|| {
        invalid_value(
            raw_value,
            "sw_last",
            "expected MIDI note 0..127 or a note name",
        )
    })?;
    Ok(default_keyswitch.is_none_or(|default| switch == default))
}

fn raw_region_matches_performance(
    raw: &RawRegion,
    notes: &[(u8, u8)],
) -> Result<bool, SfzPianoError> {
    if raw
        .values
        .keys()
        .any(|name| parse_dynamic_controller(name, "on_locc").is_some())
        || raw
            .values
            .keys()
            .any(|name| parse_dynamic_controller(name, "on_hicc").is_some())
    {
        return Ok(true);
    }
    let key = optional_region_note(raw, "key", false)?;
    let low_key = optional_region_note(raw, "lokey", false)?
        .or(key)
        .unwrap_or(0);
    let high_key = optional_region_note(raw, "hikey", false)?
        .or(key)
        .unwrap_or(127);
    let low_velocity = optional_u8(raw, "lovel", 0, 127)?.unwrap_or(0);
    let high_velocity = optional_u8(raw, "hivel", 0, 127)?.unwrap_or(127);
    Ok(notes.iter().any(|(pitch, velocity)| {
        (low_key..=high_key).contains(pitch) && (low_velocity..=high_velocity).contains(velocity)
    }))
}

fn validate_curve_references(
    raw: &RawRegion,
    curves: &BTreeMap<u16, CurveDefinition>,
) -> Result<(), SfzPianoError> {
    for (name, raw_value) in &raw.values {
        if !name.contains("_curvecc") {
            continue;
        }
        let curve = raw_value.value.parse::<u16>().map_err(|_| {
            invalid_value(
                raw_value,
                name,
                "expected an integer curve index from 0 through 254",
            )
        })?;
        if curve > 254 {
            return Err(invalid_value(
                raw_value,
                name,
                "expected an integer curve index from 0 through 254",
            ));
        }
        if curve >= 7 && !curves.contains_key(&curve) {
            return Err(SfzPianoError::UndefinedCurve {
                line: raw_value.line,
                curve,
            });
        }
    }
    Ok(())
}

fn lex(source: &str) -> Result<Vec<Lexeme>, SfzPianoError> {
    let mut lexemes = Vec::new();
    for (line_index, original_line) in source.lines().enumerate() {
        let line_number = line_index + 1;
        let line = original_line
            .split_once("//")
            .map_or(original_line, |(before, _)| before);
        let mut cursor = 0;
        while cursor < line.len() {
            cursor = skip_whitespace(line, cursor);
            if cursor >= line.len() {
                break;
            }
            if line[cursor..].starts_with('#') {
                let directive = line[cursor..]
                    .split_whitespace()
                    .next()
                    .unwrap_or("#")
                    .to_owned();
                return Err(SfzPianoError::UnsupportedDirective {
                    line: line_number,
                    directive,
                });
            }
            if line[cursor..].starts_with('<') {
                let Some(relative_end) = line[cursor + 1..].find('>') else {
                    return Err(SfzPianoError::InvalidSyntax {
                        line: line_number,
                        text: line[cursor..].to_owned(),
                    });
                };
                let end = cursor + 1 + relative_end;
                let name = line[cursor + 1..end].trim().to_ascii_lowercase();
                lexemes.push(Lexeme::Header {
                    name,
                    line: line_number,
                });
                cursor = end + 1;
                continue;
            }

            let name_start = cursor;
            while cursor < line.len()
                && !line.as_bytes()[cursor].is_ascii_whitespace()
                && line.as_bytes()[cursor] != b'='
            {
                cursor += 1;
            }
            let name = line[name_start..cursor].trim().to_ascii_lowercase();
            cursor = skip_whitespace(line, cursor);
            if cursor >= line.len() || line.as_bytes()[cursor] != b'=' || name.is_empty() {
                return Err(SfzPianoError::InvalidSyntax {
                    line: line_number,
                    text: line[name_start..].trim().to_owned(),
                });
            }
            cursor += 1;
            cursor = skip_whitespace(line, cursor);
            let value_start = cursor;
            let value_end = find_value_end(line, cursor);
            let value = unquote(line[value_start..value_end].trim());
            if value.is_empty() {
                return Err(SfzPianoError::InvalidSyntax {
                    line: line_number,
                    text: format!("{name}="),
                });
            }
            lexemes.push(Lexeme::Opcode {
                name,
                value: value.to_owned(),
                line: line_number,
            });
            cursor = value_end;
        }
    }
    Ok(lexemes)
}

fn skip_whitespace(line: &str, mut cursor: usize) -> usize {
    while cursor < line.len() && line.as_bytes()[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    cursor
}

fn find_value_end(line: &str, start: usize) -> usize {
    let mut cursor = start;
    let mut quoted = false;
    while cursor < line.len() {
        match line.as_bytes()[cursor] {
            b'"' => quoted = !quoted,
            byte if byte.is_ascii_whitespace() && !quoted => {
                let next = skip_whitespace(line, cursor);
                if next >= line.len()
                    || line[next..].starts_with('<')
                    || line[next..].starts_with('#')
                    || looks_like_opcode_start(line, next)
                {
                    return cursor;
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    line.len()
}

fn looks_like_opcode_start(line: &str, start: usize) -> bool {
    let mut cursor = start;
    while cursor < line.len()
        && !line.as_bytes()[cursor].is_ascii_whitespace()
        && line.as_bytes()[cursor] != b'='
    {
        cursor += 1;
    }
    cursor < line.len() && line.as_bytes()[cursor] == b'=' && cursor > start
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    Attack,
    Release,
    ReleaseKey,
    Control,
}

#[derive(Clone, Copy, Debug)]
struct CcCondition {
    controller: u8,
    low: u8,
    high: u8,
}

#[derive(Clone, Copy, Debug)]
struct CcModulation {
    controller: u8,
    amount: f32,
    curve: u16,
}

#[derive(Clone, Debug)]
struct SampleRegion {
    sample: Arc<DecodedSample>,
    low_key: u8,
    high_key: u8,
    low_velocity: u8,
    high_velocity: u8,
    root_key: u8,
    tune_cents: f32,
    gain: f32,
    pan: f32,
    velocity_tracking: f32,
    attack_seconds: f32,
    release_seconds: f32,
    trigger: Trigger,
    offset: usize,
    end: usize,
    random_low: f32,
    random_high: f32,
    cc_conditions: Vec<CcCondition>,
    control_triggers: Vec<CcCondition>,
    amplitude_modulations: Vec<CcModulation>,
    pan_modulations: Vec<CcModulation>,
    velocity_tracking_modulations: Vec<CcModulation>,
    release_modulations: Vec<CcModulation>,
    attack_modulations: Vec<CcModulation>,
    offset_modulations: Vec<CcModulation>,
    rt_decay_db_per_second: f32,
    group_id: u32,
    polyphony: Option<u16>,
    note_polyphony: Option<u8>,
    off_time_seconds: Option<f32>,
}

impl SampleRegion {
    fn matches(
        &self,
        pitch: u8,
        velocity: u8,
        trigger: Trigger,
        random: f32,
        cc: &[u8; 128],
    ) -> bool {
        self.trigger == trigger
            && (self.low_key..=self.high_key).contains(&pitch)
            && (self.low_velocity..=self.high_velocity).contains(&velocity)
            && (self.random_low..=self.random_high).contains(&random)
            && self.cc_conditions.iter().all(|condition| {
                (condition.low..=condition.high).contains(&cc[condition.controller as usize])
            })
    }

    fn matches_control(&self, controller: u8, value: u8, random: f32, cc: &[u8; 128]) -> bool {
        self.trigger == Trigger::Control
            && (self.random_low..=self.random_high).contains(&random)
            && self.control_triggers.iter().any(|condition| {
                condition.controller == controller
                    && (condition.low..=condition.high).contains(&value)
            })
            && self.cc_conditions.iter().all(|condition| {
                (condition.low..=condition.high).contains(&cc[condition.controller as usize])
            })
    }

    fn maximum_duration_seconds(&self) -> f32 {
        (self.end.saturating_sub(self.offset)) as f32 / self.sample.sample_rate as f32
    }

    fn modulation_value(
        modulation: CcModulation,
        cc: &[u8; 128],
        curves: &BTreeMap<u16, CurveDefinition>,
    ) -> f32 {
        curve_value(modulation.curve, cc[modulation.controller as usize], curves)
    }

    fn amplitude_gain(&self, cc: &[u8; 128], curves: &BTreeMap<u16, CurveDefinition>) -> f32 {
        self.amplitude_modulations
            .iter()
            .fold(1.0, |gain, modulation| {
                gain * (Self::modulation_value(*modulation, cc, curves) * modulation.amount / 100.0)
            })
    }

    fn modulated_pan(&self, cc: &[u8; 128], curves: &BTreeMap<u16, CurveDefinition>) -> f32 {
        self.pan_modulations
            .iter()
            .fold(self.pan, |pan, modulation| {
                pan + Self::modulation_value(*modulation, cc, curves) * modulation.amount / 100.0
            })
            .clamp(-1.0, 1.0)
    }

    fn modulated_velocity_tracking(
        &self,
        cc: &[u8; 128],
        curves: &BTreeMap<u16, CurveDefinition>,
    ) -> f32 {
        self.velocity_tracking_modulations
            .iter()
            .fold(self.velocity_tracking, |tracking, modulation| {
                tracking
                    + Self::modulation_value(*modulation, cc, curves) * modulation.amount / 100.0
            })
            .clamp(-1.0, 1.0)
    }

    fn modulated_seconds(
        base: f32,
        modulations: &[CcModulation],
        cc: &[u8; 128],
        curves: &BTreeMap<u16, CurveDefinition>,
    ) -> f32 {
        modulations
            .iter()
            .fold(base, |seconds, modulation| {
                seconds + Self::modulation_value(*modulation, cc, curves) * modulation.amount
            })
            .max(0.0)
    }

    fn modulated_offset(&self, cc: &[u8; 128], curves: &BTreeMap<u16, CurveDefinition>) -> usize {
        let offset =
            self.offset_modulations
                .iter()
                .fold(self.offset as f32, |offset, modulation| {
                    offset + Self::modulation_value(*modulation, cc, curves) * modulation.amount
                });
        offset.round().clamp(0.0, self.end.saturating_sub(1) as f32) as usize
    }
}

fn curve_value(curve: u16, controller_value: u8, curves: &BTreeMap<u16, CurveDefinition>) -> f32 {
    let normalized = controller_value as f32 / 127.0;
    match curve {
        0 => normalized,
        1 => normalized * 2.0 - 1.0,
        2 => 1.0 - normalized,
        3 => 1.0 - normalized * 2.0,
        // ARIA curve 4 is a perceptual volume curve. This logarithmic mapping
        // keeps CC7 useful at low values without introducing a hard floor.
        4 => ((controller_value as f32 + 1.0).ln() / 128.0_f32.ln()).clamp(0.0, 1.0),
        5 => normalized.sqrt(),
        6 => (1.0 - normalized).sqrt(),
        _ => curves
            .get(&curve)
            .map_or(normalized, |definition| definition.value(controller_value)),
    }
}

impl CurveDefinition {
    fn value(&self, controller_value: u8) -> f32 {
        if self.points.is_empty() {
            return controller_value as f32 / 127.0;
        }
        let lower = self
            .points
            .range(..=controller_value)
            .next_back()
            .map(|(point, value)| (*point, *value))
            .unwrap_or((0, 0.0));
        let upper = self
            .points
            .range(controller_value..)
            .next()
            .map(|(point, value)| (*point, *value))
            .unwrap_or((127, 1.0));
        if lower.0 == upper.0 {
            return lower.1;
        }
        let position = (controller_value - lower.0) as f32 / (upper.0 - lower.0) as f32;
        lower.1 + (upper.1 - lower.1) * position
    }
}

#[derive(Clone, Debug)]
struct DecodedSample {
    sample_rate: u32,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl DecodedSample {
    fn frames(&self) -> usize {
        self.left.len().min(self.right.len())
    }
}

fn load_region(
    raw: &RawRegion,
    default_path: &Path,
    sfz_root: &Path,
    pack_root: &Path,
    cache: &mut HashMap<PathBuf, Arc<DecodedSample>>,
) -> Result<SampleRegion, SfzPianoError> {
    let sample_value = raw
        .values
        .get("sample")
        .ok_or(SfzPianoError::MissingSample { line: raw.line })?;
    let sample_relative = normalized_relative_path(&sample_value.value);
    validate_relative_sample_path(&sample_relative, sample_value.line)?;
    let default_relative = normalized_relative_path(&default_path.to_string_lossy());
    validate_relative_sample_path(&default_relative, sample_value.line)?;
    let requested_path = sfz_root.join(default_relative).join(sample_relative);
    let sample_path =
        fs::canonicalize(&requested_path).map_err(|source| SfzPianoError::ReadSample {
            line: sample_value.line,
            path: requested_path.clone(),
            source,
        })?;
    if !sample_path.starts_with(pack_root) {
        return Err(SfzPianoError::SampleEscapesPack {
            line: sample_value.line,
            path: sample_path,
        });
    }
    let sample = if let Some(sample) = cache.get(&sample_path) {
        sample.clone()
    } else {
        let decoded = Arc::new(decode_sample(&sample_path)?);
        cache.insert(sample_path.clone(), decoded.clone());
        decoded
    };

    let control_triggers = parse_control_triggers(raw)?;
    let control_region = !control_triggers.is_empty();
    let key = optional_region_note(raw, "key", control_region)?;
    let low_key = optional_region_note(raw, "lokey", control_region)?
        .or(key)
        .unwrap_or(0);
    let high_key = optional_region_note(raw, "hikey", control_region)?
        .or(key)
        .unwrap_or(127);
    let root_key = optional_note(raw, "pitch_keycenter")?.or(key).unwrap_or(60);
    let switch_low = optional_note(raw, "sw_lokey")?;
    let switch_high = optional_note(raw, "sw_hikey")?;
    if let (Some(low), Some(high)) = (switch_low, switch_high)
        && low > high
    {
        return invalid_region_range(raw, "sw_lokey", low, high);
    }
    let low_velocity = optional_u8(raw, "lovel", 0, 127)?.unwrap_or(0);
    let high_velocity = optional_u8(raw, "hivel", 0, 127)?.unwrap_or(127);
    if low_key > high_key {
        return invalid_region_range(raw, "lokey", low_key, high_key);
    }
    if low_velocity > high_velocity {
        return invalid_region_range(raw, "lovel", low_velocity, high_velocity);
    }

    let tune_cents = optional_f32(raw, "tune", -100.0, 100.0)?.unwrap_or(0.0);
    let volume_db = optional_f32(raw, "volume", -144.0, 6.0)?.unwrap_or(0.0);
    let amplitude = optional_f32(raw, "amplitude", -200.0, 200.0)?.unwrap_or(100.0) / 100.0;
    let pan = optional_f32(raw, "pan", -100.0, 100.0)?.unwrap_or(0.0) / 100.0;
    let velocity_tracking =
        optional_f32(raw, "amp_veltrack", -100.0, 100.0)?.unwrap_or(100.0) / 100.0;
    let attack_seconds = optional_f32(raw, "ampeg_attack", 0.0, 100.0)?.unwrap_or(0.0);
    let release_seconds = optional_f32(raw, "ampeg_release", 0.0, 100.0)?.unwrap_or(0.0);
    let trigger = if control_region {
        Trigger::Control
    } else {
        parse_trigger(raw)?
    };
    let offset = optional_usize(raw, "offset")?.unwrap_or(0);
    let end = optional_usize(raw, "end")?
        .map(|end| end.saturating_add(1))
        .unwrap_or_else(|| sample.frames())
        .min(sample.frames());
    if offset >= end {
        return Err(SfzPianoError::InvalidOpcodeValue {
            line: raw.line,
            opcode: "offset/end".to_owned(),
            value: format!("{offset}/{end}"),
            reason: "the playable sample range is empty".to_owned(),
        });
    }
    let random_low = optional_f32(raw, "lorand", 0.0, 1.0)?.unwrap_or(0.0);
    let random_high = optional_f32(raw, "hirand", 0.0, 1.0)?.unwrap_or(1.0);
    if random_low > random_high {
        return Err(SfzPianoError::InvalidOpcodeValue {
            line: raw.line,
            opcode: "lorand/hirand".to_owned(),
            value: format!("{random_low}..{random_high}"),
            reason: "lower bound exceeds upper bound".to_owned(),
        });
    }
    let cc_conditions = parse_cc_conditions(raw)?;
    let amplitude_modulations =
        parse_cc_modulations(raw, "amplitude_oncc", "amplitude_curvecc", -200.0, 200.0)?;
    let pan_modulations = parse_cc_modulations(raw, "pan_oncc", "pan_curvecc", -200.0, 200.0)?;
    let velocity_tracking_modulations = parse_cc_modulations(
        raw,
        "amp_veltrack_oncc",
        "amp_veltrack_curvecc",
        -200.0,
        200.0,
    )?;
    let release_modulations = parse_cc_modulations(
        raw,
        "ampeg_release_oncc",
        "ampeg_release_curvecc",
        -100.0,
        100.0,
    )?;
    let attack_modulations = parse_cc_modulations(
        raw,
        "ampeg_attack_oncc",
        "ampeg_attack_curvecc",
        -100.0,
        100.0,
    )?;
    let offset_modulations = parse_cc_modulations(
        raw,
        "offset_oncc",
        "offset_curvecc",
        -10_000_000.0,
        10_000_000.0,
    )?;
    let rt_decay_db_per_second = optional_f32(raw, "rt_decay", 0.0, 200.0)?.unwrap_or(0.0);
    let group_id = optional_u32(raw, "group")?.unwrap_or(0);
    let polyphony = optional_u16(raw, "polyphony", 1, u16::MAX)?;
    let note_polyphony = optional_u8(raw, "note_polyphony", 1, 127)?;
    let off_time_seconds = optional_f32(raw, "off_time", 0.0, 100.0)?;

    Ok(SampleRegion {
        sample,
        low_key,
        high_key,
        low_velocity,
        high_velocity,
        root_key,
        tune_cents,
        gain: amplitude * 10.0_f32.powf(volume_db / 20.0),
        pan,
        velocity_tracking,
        attack_seconds,
        release_seconds,
        trigger,
        offset,
        end,
        random_low,
        random_high,
        cc_conditions,
        control_triggers,
        amplitude_modulations,
        pan_modulations,
        velocity_tracking_modulations,
        release_modulations,
        attack_modulations,
        offset_modulations,
        rt_decay_db_per_second,
        group_id,
        polyphony,
        note_polyphony,
        off_time_seconds,
    })
}

fn parse_cc_conditions(raw: &RawRegion) -> Result<Vec<CcCondition>, SfzPianoError> {
    parse_condition_ranges(raw, "locc", "hicc")
}

fn parse_control_triggers(raw: &RawRegion) -> Result<Vec<CcCondition>, SfzPianoError> {
    parse_condition_ranges(raw, "on_locc", "on_hicc")
}

fn parse_condition_ranges(
    raw: &RawRegion,
    low_prefix: &str,
    high_prefix: &str,
) -> Result<Vec<CcCondition>, SfzPianoError> {
    let mut ranges = BTreeMap::<u8, (u8, u8)>::new();
    for (name, raw_value) in &raw.values {
        if let Some(controller) = parse_dynamic_controller(name, low_prefix) {
            let value = parse_cc_bound(raw_value, name)?;
            ranges
                .entry(controller)
                .and_modify(|range| range.0 = range.0.max(value))
                .or_insert((value, 127));
        } else if let Some(controller) = parse_dynamic_controller(name, high_prefix) {
            let value = parse_cc_bound(raw_value, name)?;
            ranges
                .entry(controller)
                .and_modify(|range| range.1 = range.1.min(value))
                .or_insert((0, value));
        }
    }
    for (controller, (low, high)) in &ranges {
        if low > high {
            return Err(SfzPianoError::InvalidOpcodeValue {
                line: raw.line,
                opcode: format!("{low_prefix}{controller}/{high_prefix}{controller}"),
                value: format!("{low}..{high}"),
                reason: "lower bound exceeds upper bound".to_owned(),
            });
        }
    }
    Ok(ranges
        .into_iter()
        .map(|(controller, (low, high))| CcCondition {
            controller,
            low,
            high,
        })
        .collect())
}

fn parse_cc_bound(raw: &RawValue, opcode: &str) -> Result<u8, SfzPianoError> {
    let value = raw
        .value
        .parse::<u16>()
        .map_err(|_| invalid_value(raw, opcode, "expected an integer from 0 through 127"))?;
    if value > 127 {
        return Err(invalid_value(
            raw,
            opcode,
            "expected an integer from 0 through 127",
        ));
    }
    Ok(value as u8)
}

fn parse_cc_modulations(
    raw: &RawRegion,
    value_prefix: &str,
    curve_prefix: &str,
    minimum: f32,
    maximum: f32,
) -> Result<Vec<CcModulation>, SfzPianoError> {
    let mut modulations = Vec::new();
    for (name, raw_value) in &raw.values {
        let Some(controller) = parse_dynamic_controller(name, value_prefix) else {
            continue;
        };
        let amount = raw_value.value.parse::<f32>().map_err(|_| {
            invalid_value(
                raw_value,
                name,
                "expected a finite numeric modulation amount",
            )
        })?;
        if !amount.is_finite() || amount < minimum || amount > maximum {
            return Err(invalid_value(
                raw_value,
                name,
                &format!("expected a value from {minimum} through {maximum}"),
            ));
        }
        let curve_name = format!("{curve_prefix}{controller}");
        let curve = raw
            .values
            .get(&curve_name)
            .map(|curve_value| {
                curve_value.value.parse::<u16>().map_err(|_| {
                    invalid_value(
                        curve_value,
                        &curve_name,
                        "expected an integer from 0 through 254",
                    )
                })
            })
            .transpose()?
            .unwrap_or(0);
        if curve > 254 {
            let curve_value = raw.values.get(&curve_name).expect("curve value exists");
            return Err(invalid_value(
                curve_value,
                &curve_name,
                "expected an integer from 0 through 254",
            ));
        }
        modulations.push(CcModulation {
            controller,
            amount,
            curve,
        });
    }
    Ok(modulations)
}

fn parse_dynamic_controller(name: &str, prefix: &str) -> Option<u8> {
    let suffix = name.strip_prefix(prefix)?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    suffix
        .parse::<u16>()
        .ok()
        .filter(|value| *value <= 127)
        .map(|value| value as u8)
}

fn normalized_relative_path(value: &str) -> PathBuf {
    PathBuf::from(value.replace('\\', "/"))
}

fn validate_relative_sample_path(path: &Path, line: usize) -> Result<(), SfzPianoError> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(SfzPianoError::InvalidSamplePath {
            line,
            path: path.to_owned(),
        });
    }
    Ok(())
}

fn optional_note(raw: &RawRegion, name: &str) -> Result<Option<u8>, SfzPianoError> {
    let Some(raw_value) = raw.values.get(name) else {
        return Ok(None);
    };
    parse_midi_note(&raw_value.value)
        .map(Some)
        .ok_or_else(|| invalid_value(raw_value, name, "expected MIDI note 0..127 or a note name"))
}

fn optional_region_note(
    raw: &RawRegion,
    name: &str,
    allow_disabled: bool,
) -> Result<Option<u8>, SfzPianoError> {
    if allow_disabled
        && raw
            .values
            .get(name)
            .is_some_and(|value| value.value.trim() == "-1")
    {
        return Ok(None);
    }
    optional_note(raw, name)
}

fn parse_midi_note(value: &str) -> Option<u8> {
    if let Ok(note) = value.parse::<u8>() {
        return (note <= 127).then_some(note);
    }
    let mut chars = value.chars();
    let letter = chars.next()?.to_ascii_uppercase();
    let mut semitone = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let remainder = chars.as_str();
    let (accidental, octave) = if let Some(octave) = remainder.strip_prefix('#') {
        (1, octave)
    } else if let Some(octave) = remainder.strip_prefix('b') {
        (-1, octave)
    } else {
        (0, remainder)
    };
    semitone += accidental;
    let octave = octave.parse::<i16>().ok()?;
    let note = (octave + 1) * 12 + semitone;
    u8::try_from(note).ok().filter(|note| *note <= 127)
}

fn optional_u8(
    raw: &RawRegion,
    name: &str,
    minimum: u8,
    maximum: u8,
) -> Result<Option<u8>, SfzPianoError> {
    parse_optional(
        raw,
        name,
        |value| value.parse::<u8>().ok(),
        minimum,
        maximum,
    )
}

fn optional_u16(
    raw: &RawRegion,
    name: &str,
    minimum: u16,
    maximum: u16,
) -> Result<Option<u16>, SfzPianoError> {
    parse_optional(
        raw,
        name,
        |value| value.parse::<u16>().ok(),
        minimum,
        maximum,
    )
}

fn optional_u32(raw: &RawRegion, name: &str) -> Result<Option<u32>, SfzPianoError> {
    let Some(raw_value) = raw.values.get(name) else {
        return Ok(None);
    };
    raw_value
        .value
        .parse::<u32>()
        .map(Some)
        .map_err(|_| invalid_value(raw_value, name, "expected a non-negative integer"))
}

fn optional_f32(
    raw: &RawRegion,
    name: &str,
    minimum: f32,
    maximum: f32,
) -> Result<Option<f32>, SfzPianoError> {
    parse_optional(
        raw,
        name,
        |value| value.parse::<f32>().ok(),
        minimum,
        maximum,
    )
}

fn parse_optional<T>(
    raw: &RawRegion,
    name: &str,
    parse: impl FnOnce(&str) -> Option<T>,
    minimum: T,
    maximum: T,
) -> Result<Option<T>, SfzPianoError>
where
    T: PartialOrd + std::fmt::Display,
{
    let Some(raw_value) = raw.values.get(name) else {
        return Ok(None);
    };
    let Some(value) = parse(&raw_value.value) else {
        return Err(invalid_value(raw_value, name, "expected a number"));
    };
    if value < minimum || value > maximum {
        return Err(invalid_value(
            raw_value,
            name,
            &format!("expected a value from {minimum} through {maximum}"),
        ));
    }
    Ok(Some(value))
}

fn optional_usize(raw: &RawRegion, name: &str) -> Result<Option<usize>, SfzPianoError> {
    let Some(raw_value) = raw.values.get(name) else {
        return Ok(None);
    };
    raw_value
        .value
        .parse::<usize>()
        .map(Some)
        .map_err(|_| invalid_value(raw_value, name, "expected a non-negative sample index"))
}

fn parse_trigger(raw: &RawRegion) -> Result<Trigger, SfzPianoError> {
    let Some(raw_value) = raw.values.get("trigger") else {
        return Ok(Trigger::Attack);
    };
    match raw_value.value.as_str() {
        "attack" => Ok(Trigger::Attack),
        "release" => Ok(Trigger::Release),
        "release_key" => Ok(Trigger::ReleaseKey),
        _ => Err(invalid_value(
            raw_value,
            "trigger",
            "only attack, release, and release_key are supported",
        )),
    }
}

fn invalid_region_range<T>(
    raw: &RawRegion,
    opcode: &str,
    low: T,
    high: T,
) -> Result<SampleRegion, SfzPianoError>
where
    T: std::fmt::Display,
{
    Err(SfzPianoError::InvalidOpcodeValue {
        line: raw.line,
        opcode: opcode.to_owned(),
        value: format!("{low}..{high}"),
        reason: "lower bound exceeds upper bound".to_owned(),
    })
}

fn invalid_value(raw: &RawValue, opcode: &str, reason: &str) -> SfzPianoError {
    SfzPianoError::InvalidOpcodeValue {
        line: raw.line,
        opcode: opcode.to_owned(),
        value: raw.value.clone(),
        reason: reason.to_owned(),
    }
}

fn decode_sample(path: &Path) -> Result<DecodedSample, SfzPianoError> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "wav" => decode_wav(path),
        "flac" => decode_flac(path),
        _ => Err(SfzPianoError::UnsupportedSampleFormat {
            path: path.to_owned(),
            extension,
        }),
    }
}

fn decode_wav(path: &Path) -> Result<DecodedSample, SfzPianoError> {
    let mut reader = hound::WavReader::open(path).map_err(|error| SfzPianoError::DecodeSample {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    let spec = reader.spec();
    validate_channels(path, u32::from(spec.channels))?;
    let interleaved = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| SfzPianoError::DecodeSample {
                path: path.to_owned(),
                message: error.to_string(),
            })?,
        hound::SampleFormat::Int => {
            if spec.bits_per_sample == 0 || spec.bits_per_sample > 32 {
                return Err(SfzPianoError::DecodeSample {
                    path: path.to_owned(),
                    message: format!("unsupported {}-bit PCM", spec.bits_per_sample),
                });
            }
            let scale = (1_u64 << (u32::from(spec.bits_per_sample) - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|sample| sample as f32 / scale))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| SfzPianoError::DecodeSample {
                    path: path.to_owned(),
                    message: error.to_string(),
                })?
        }
    };
    deinterleave(
        path,
        spec.sample_rate,
        u32::from(spec.channels),
        interleaved,
    )
}

fn decode_flac(path: &Path) -> Result<DecodedSample, SfzPianoError> {
    let mut reader =
        claxon::FlacReader::open(path).map_err(|error| SfzPianoError::DecodeSample {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    let info = reader.streaminfo();
    validate_channels(path, info.channels)?;
    if info.bits_per_sample == 0 || info.bits_per_sample > 32 {
        return Err(SfzPianoError::DecodeSample {
            path: path.to_owned(),
            message: format!("unsupported {}-bit FLAC", info.bits_per_sample),
        });
    }
    let scale = (1_u64 << (info.bits_per_sample - 1)) as f32;
    let interleaved = reader
        .samples()
        .map(|sample| sample.map(|sample| sample as f32 / scale))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| SfzPianoError::DecodeSample {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    deinterleave(path, info.sample_rate, info.channels, interleaved)
}

fn validate_channels(path: &Path, channels: u32) -> Result<(), SfzPianoError> {
    if !(1..=2).contains(&channels) {
        return Err(SfzPianoError::UnsupportedChannelCount {
            path: path.to_owned(),
            channels,
        });
    }
    Ok(())
}

fn deinterleave(
    path: &Path,
    sample_rate: u32,
    channels: u32,
    interleaved: Vec<f32>,
) -> Result<DecodedSample, SfzPianoError> {
    if interleaved.is_empty() {
        return Err(SfzPianoError::EmptySample(path.to_owned()));
    }
    if channels == 1 {
        return Ok(DecodedSample {
            sample_rate,
            left: interleaved.clone(),
            right: interleaved,
        });
    }
    let mut left = Vec::with_capacity(interleaved.len() / 2);
    let mut right = Vec::with_capacity(interleaved.len() / 2);
    for frame in interleaved.chunks_exact(2) {
        left.push(frame[0]);
        right.push(frame[1]);
    }
    if left.is_empty() {
        return Err(SfzPianoError::EmptySample(path.to_owned()));
    }
    Ok(DecodedSample {
        sample_rate,
        left,
        right,
    })
}

struct SfzPianoSession {
    regions: Arc<[SampleRegion]>,
    curves: Arc<BTreeMap<u16, CurveDefinition>>,
    output_sample_rate: u32,
    voices: Vec<SampleVoice>,
    notes: Vec<NoteInstance>,
    next_note_id: u64,
    sustain_down: bool,
    soft_pedal: bool,
    random_state: u32,
    cc: [u8; 128],
    initial_cc: [u8; 128],
    rendered_frames: u64,
}

impl SfzPianoSession {
    fn new(
        regions: Arc<[SampleRegion]>,
        output_sample_rate: u32,
        initial_cc: [u8; 128],
        curves: Arc<BTreeMap<u16, CurveDefinition>>,
    ) -> Self {
        Self {
            regions,
            curves,
            output_sample_rate,
            voices: Vec::new(),
            notes: Vec::new(),
            next_note_id: 1,
            sustain_down: false,
            soft_pedal: false,
            random_state: 0x4d595df4,
            cc: initial_cc,
            initial_cc,
            rendered_frames: 0,
        }
    }

    fn note_on(&mut self, pitch: u8, velocity: u8) {
        if velocity == 0 {
            self.note_off(pitch);
            return;
        }
        let id = self.next_note_id;
        self.next_note_id = self.next_note_id.wrapping_add(1).max(1);
        self.spawn_regions(Trigger::Attack, pitch, velocity, Some(id), 0.0);
        self.notes.push(NoteInstance {
            id,
            pitch,
            velocity,
            key_down: true,
            started_frame: self.rendered_frames,
        });
    }

    fn note_off(&mut self, pitch: u8) {
        let Some(index) = self
            .notes
            .iter()
            .position(|note| note.pitch == pitch && note.key_down)
        else {
            return;
        };
        self.notes[index].key_down = false;
        let note = self.notes[index];
        self.spawn_regions(
            Trigger::ReleaseKey,
            pitch,
            note.velocity,
            None,
            self.note_held_seconds(note),
        );
        if !self.sustain_down {
            self.finish_note(note);
        }
    }

    fn finish_note(&mut self, note: NoteInstance) {
        for voice in &mut self.voices {
            if voice.note_id == Some(note.id) {
                voice.release(self.output_sample_rate);
            }
        }
        self.spawn_regions(
            Trigger::Release,
            note.pitch,
            note.velocity,
            None,
            self.note_held_seconds(note),
        );
        self.notes.retain(|candidate| candidate.id != note.id);
    }

    fn spawn_regions(
        &mut self,
        trigger: Trigger,
        pitch: u8,
        velocity: u8,
        note_id: Option<u64>,
        held_seconds: f32,
    ) {
        let soft_gain = if self.soft_pedal { 0.72 } else { 1.0 };
        let random = self.next_random();
        let cc = self.cc;
        let curves = self.curves.clone();
        let matched: Vec<_> = self
            .regions
            .iter()
            .filter(|region| region.matches(pitch, velocity, trigger, random, &cc))
            .cloned()
            .collect();
        let mut spawned = Vec::with_capacity(matched.len());
        for region in &matched {
            self.apply_polyphony(region, pitch, velocity);
            spawned.push(SampleVoice::new(
                region,
                pitch,
                velocity,
                note_id,
                VoiceContext {
                    output_sample_rate: self.output_sample_rate,
                    soft_gain,
                    cc: &cc,
                    curves: &curves,
                    held_seconds,
                },
            ));
        }
        while self.voices.len() + spawned.len() > MAX_VOICES {
            if let Some(index) = self
                .voices
                .iter()
                .enumerate()
                .max_by_key(|(_, voice)| voice.age_frames)
                .map(|(index, _)| index)
            {
                self.voices.swap_remove(index);
            } else {
                spawned.remove(0);
            }
        }
        self.voices.append(&mut spawned);
    }

    fn apply_polyphony(&mut self, region: &SampleRegion, pitch: u8, velocity: u8) {
        let fade_seconds = region.off_time_seconds.unwrap_or(0.006);
        if let Some(limit) = region.note_polyphony {
            let same_note_count = self
                .voices
                .iter()
                .filter(|voice| voice.group_id == region.group_id && voice.pitch == pitch)
                .count();
            let to_fade = same_note_count
                .saturating_add(1)
                .saturating_sub(limit as usize);
            let mut candidates: Vec<_> = self
                .voices
                .iter()
                .enumerate()
                .filter(|(_, voice)| {
                    voice.group_id == region.group_id
                        && voice.pitch == pitch
                        && voice.source_velocity <= velocity
                })
                .map(|(index, voice)| (index, voice.age_frames))
                .collect();
            candidates.sort_unstable_by_key(|(_, age)| std::cmp::Reverse(*age));
            for (index, _) in candidates.into_iter().take(to_fade) {
                self.voices[index].fade_out(fade_seconds, self.output_sample_rate);
            }
        }
        if let Some(limit) = region.polyphony {
            let group_count = self
                .voices
                .iter()
                .filter(|voice| voice.group_id == region.group_id)
                .count();
            let to_fade = group_count.saturating_add(1).saturating_sub(limit as usize);
            let mut candidates: Vec<_> = self
                .voices
                .iter()
                .enumerate()
                .filter(|(_, voice)| voice.group_id == region.group_id)
                .map(|(index, voice)| (index, voice.age_frames))
                .collect();
            candidates.sort_unstable_by_key(|(_, age)| std::cmp::Reverse(*age));
            for (index, _) in candidates.into_iter().take(to_fade) {
                self.voices[index].fade_out(fade_seconds, self.output_sample_rate);
            }
        }
    }

    fn spawn_control_regions(&mut self, controller: u8, value: u8) {
        let random = self.next_random();
        let cc = self.cc;
        let curves = self.curves.clone();
        let matched: Vec<_> = self
            .regions
            .iter()
            .filter(|region| region.matches_control(controller, value, random, &cc))
            .cloned()
            .collect();
        for region in &matched {
            self.apply_polyphony(region, 60, 127);
            self.voices.push(SampleVoice::new(
                region,
                60,
                127,
                None,
                VoiceContext {
                    output_sample_rate: self.output_sample_rate,
                    soft_gain: 1.0,
                    cc: &cc,
                    curves: &curves,
                    held_seconds: 0.0,
                },
            ));
        }
        self.enforce_global_voice_limit();
    }

    fn enforce_global_voice_limit(&mut self) {
        while self.voices.len() > MAX_VOICES {
            let Some(index) = self
                .voices
                .iter()
                .enumerate()
                .max_by_key(|(_, voice)| voice.age_frames)
                .map(|(index, _)| index)
            else {
                break;
            };
            self.voices.swap_remove(index);
        }
    }

    fn note_held_seconds(&self, note: NoteInstance) -> f32 {
        self.rendered_frames.saturating_sub(note.started_frame) as f32
            / self.output_sample_rate as f32
    }

    fn next_random(&mut self) -> f32 {
        let mut value = self.random_state;
        value ^= value << 13;
        value ^= value >> 17;
        value ^= value << 5;
        self.random_state = value;
        value as f32 / u32::MAX as f32
    }

    fn set_sustain(&mut self, down: bool) {
        if self.sustain_down && !down {
            let deferred: Vec<_> = self
                .notes
                .iter()
                .copied()
                .filter(|note| !note.key_down)
                .collect();
            for note in deferred {
                self.finish_note(note);
            }
        }
        self.sustain_down = down;
    }

    fn all_notes_off(&mut self) {
        let notes = self.notes.clone();
        self.sustain_down = false;
        for note in notes {
            self.finish_note(note);
        }
    }
}

impl InstrumentSession for SfzPianoSession {
    fn send_event(&mut self, event: InstrumentEvent) {
        match event {
            InstrumentEvent::NoteOn { pitch, velocity } => self.note_on(pitch, velocity),
            InstrumentEvent::NoteOff { pitch } => self.note_off(pitch),
            InstrumentEvent::ControlChange {
                controller: 7,
                value,
            } => {
                self.cc[7] = value;
                self.spawn_control_regions(7, value);
            }
            InstrumentEvent::ControlChange {
                controller: 10,
                value,
            } => {
                self.cc[10] = value;
                self.spawn_control_regions(10, value);
            }
            InstrumentEvent::ControlChange {
                controller: 64,
                value,
            } => {
                self.cc[64] = value;
                self.spawn_control_regions(64, value);
                self.set_sustain(value >= 64);
            }
            InstrumentEvent::ControlChange {
                controller: 67,
                value,
            } => {
                self.cc[67] = value;
                self.spawn_control_regions(67, value);
                self.soft_pedal = value >= 64;
            }
            InstrumentEvent::ControlChange {
                controller: 120, ..
            } => {
                self.voices.clear();
                self.notes.clear();
            }
            InstrumentEvent::ControlChange {
                controller: 121, ..
            } => {
                self.set_sustain(false);
                self.soft_pedal = false;
                self.cc = self.initial_cc;
            }
            InstrumentEvent::ControlChange {
                controller: 123, ..
            } => self.all_notes_off(),
            InstrumentEvent::ControlChange { controller, value } => {
                self.cc[controller as usize] = value;
                self.spawn_control_regions(controller, value);
            }
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        assert_eq!(left.len(), right.len());
        left.fill(0.0);
        right.fill(0.0);
        for frame in 0..left.len() {
            let mut mixed_left = 0.0;
            let mut mixed_right = 0.0;
            for voice in &mut self.voices {
                let (voice_left, voice_right) = voice.next_sample();
                mixed_left += voice_left;
                mixed_right += voice_right;
            }
            left[frame] = mixed_left;
            right[frame] = mixed_right;
        }
        self.voices.retain(|voice| !voice.finished());
        self.rendered_frames = self.rendered_frames.saturating_add(left.len() as u64);
    }
}

#[derive(Clone, Copy)]
struct NoteInstance {
    id: u64,
    pitch: u8,
    velocity: u8,
    key_down: bool,
    started_frame: u64,
}

struct SampleVoice {
    sample: Arc<DecodedSample>,
    position: f64,
    end: usize,
    step: f64,
    gain: f32,
    pan_left: f32,
    pan_right: f32,
    attack_frames: u64,
    release_frames: u64,
    release_elapsed: Option<u64>,
    release_start_gain: f32,
    age_frames: u64,
    note_id: Option<u64>,
    pitch: u8,
    source_velocity: u8,
    group_id: u32,
}

struct VoiceContext<'a> {
    output_sample_rate: u32,
    soft_gain: f32,
    cc: &'a [u8; 128],
    curves: &'a BTreeMap<u16, CurveDefinition>,
    held_seconds: f32,
}

impl SampleVoice {
    fn new(
        region: &SampleRegion,
        pitch: u8,
        velocity: u8,
        note_id: Option<u64>,
        context: VoiceContext<'_>,
    ) -> Self {
        let VoiceContext {
            output_sample_rate,
            soft_gain,
            cc,
            curves,
            held_seconds,
        } = context;
        let semitones = pitch as f64 - region.root_key as f64 + region.tune_cents as f64 / 100.0;
        let step = region.sample.sample_rate as f64 / output_sample_rate as f64
            * 2.0_f64.powf(semitones / 12.0);
        let source_velocity = velocity;
        let normalized_velocity = velocity as f32 / 127.0;
        let velocity_tracking = region.modulated_velocity_tracking(cc, curves);
        let velocity_squared = normalized_velocity * normalized_velocity;
        let velocity_gain = if velocity_tracking >= 0.0 {
            (1.0 - velocity_tracking) + velocity_tracking * velocity_squared
        } else {
            (1.0 + velocity_tracking) + -velocity_tracking * (1.0 - velocity_squared)
        };
        let (pan_left, pan_right) = pan_gains(region.modulated_pan(cc, curves));
        let attack_seconds = SampleRegion::modulated_seconds(
            region.attack_seconds,
            &region.attack_modulations,
            cc,
            curves,
        );
        let release_seconds = SampleRegion::modulated_seconds(
            region.release_seconds,
            &region.release_modulations,
            cc,
            curves,
        );
        let release_sample_gain =
            10.0_f32.powf(-region.rt_decay_db_per_second * held_seconds.max(0.0) / 20.0);
        Self {
            sample: region.sample.clone(),
            position: region.modulated_offset(cc, curves) as f64,
            end: region.end,
            step,
            gain: region.gain
                * region.amplitude_gain(cc, curves)
                * velocity_gain
                * soft_gain
                * release_sample_gain,
            pan_left,
            pan_right,
            attack_frames: seconds_to_frames(attack_seconds, output_sample_rate),
            release_frames: seconds_to_frames(
                release_seconds.max(MIN_RELEASE_SECONDS),
                output_sample_rate,
            ),
            release_elapsed: None,
            release_start_gain: 1.0,
            age_frames: 0,
            note_id,
            pitch,
            source_velocity,
            group_id: region.group_id,
        }
    }

    fn release(&mut self, _sample_rate: u32) {
        if self.release_elapsed.is_none() {
            self.release_start_gain = self.attack_gain();
            self.release_elapsed = Some(0);
        }
    }

    fn fade_out(&mut self, seconds: f32, sample_rate: u32) {
        self.release_frames = seconds_to_frames(seconds.max(MIN_RELEASE_SECONDS), sample_rate);
        self.release_start_gain = self.envelope_gain();
        self.release_elapsed = Some(0);
    }

    fn attack_gain(&self) -> f32 {
        if self.attack_frames == 0 {
            1.0
        } else {
            (self.age_frames as f32 / self.attack_frames as f32).clamp(0.0, 1.0)
        }
    }

    fn envelope_gain(&self) -> f32 {
        if let Some(elapsed) = self.release_elapsed {
            self.release_start_gain
                * (1.0 - elapsed as f32 / self.release_frames.max(1) as f32).clamp(0.0, 1.0)
        } else {
            self.attack_gain()
        }
    }

    fn next_sample(&mut self) -> (f32, f32) {
        if self.finished() {
            return (0.0, 0.0);
        }
        let index = self.position.floor() as usize;
        let next = (index + 1).min(self.end - 1);
        let fraction = (self.position - index as f64) as f32;
        let left =
            self.sample.left[index] + (self.sample.left[next] - self.sample.left[index]) * fraction;
        let right = self.sample.right[index]
            + (self.sample.right[next] - self.sample.right[index]) * fraction;
        let envelope = self.envelope_gain() * self.gain;
        self.position += self.step;
        self.age_frames = self.age_frames.saturating_add(1);
        if let Some(elapsed) = &mut self.release_elapsed {
            *elapsed = elapsed.saturating_add(1);
        }
        (
            left * envelope * self.pan_left,
            right * envelope * self.pan_right,
        )
    }

    fn finished(&self) -> bool {
        self.position >= self.end as f64
            || self
                .release_elapsed
                .is_some_and(|elapsed| elapsed >= self.release_frames)
    }
}

fn seconds_to_frames(seconds: f32, sample_rate: u32) -> u64 {
    (seconds * sample_rate as f32).round().max(0.0) as u64
}

fn pan_gains(pan: f32) -> (f32, f32) {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * FRAC_PI_4;
    let normalization = 2.0_f32.sqrt();
    (angle.cos() * normalization, angle.sin() * normalization)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ASSET_PACK_SCHEMA_VERSION, AssetLicense, AssetPackManifest, InstrumentEvent, InstrumentRack,
    };
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create() -> Self {
            loop {
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let serial = NEXT_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "ai-music-sfz-{}-{timestamp}-{serial}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("could not create test directory {path:?}: {error}"),
                }
            }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_wav(path: &Path, amplitude: f32, frames: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..frames {
            let phase = frame as f32 * 440.0 * std::f32::consts::TAU / 16_000.0;
            writer.write_sample(amplitude * phase.sin()).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn write_pcm_wav(path: &Path, amplitude: f32, frames: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..frames {
            let phase = frame as f32 * 440.0 * std::f32::consts::TAU / 16_000.0;
            writer
                .write_sample((amplitude * phase.sin() * i16::MAX as f32) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    fn write_pack(directory: &TestDirectory, sfz: &str) -> AssetPack {
        fs::write(directory.0.join("piano.sfz"), sfz).unwrap();
        let manifest = AssetPackManifest {
            schema_version: ASSET_PACK_SCHEMA_VERSION,
            id: "test-sfz-piano".to_owned(),
            name: "Test SFZ Piano".to_owned(),
            instrument_id: "piano".to_owned(),
            engine: AssetPackEngine::Sfz,
            entry: "piano.sfz".to_owned(),
            license: AssetLicense {
                spdx: Some("CC0-1.0".to_owned()),
                name: "CC0".to_owned(),
                source: "https://example.test/piano".to_owned(),
                attribution: "Test fixture".to_owned(),
            },
        };
        let manifest_path = directory.0.join("pack.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        AssetPack::load(manifest_path).unwrap()
    }

    fn energy(samples: &[f32]) -> f32 {
        samples.iter().map(|sample| sample * sample).sum()
    }

    #[test]
    fn selects_velocity_layers_through_the_instrument_seam() {
        let directory = TestDirectory::create();
        fs::create_dir(directory.0.join("samples")).unwrap();
        write_wav(&directory.0.join("samples/soft layer.wav"), 0.1, 4_000);
        write_wav(&directory.0.join("samples/loud layer.wav"), 0.7, 4_000);
        let pack = write_pack(
            &directory,
            "<control> default_path=samples/\n\
             <group> amp_veltrack=0 ampeg_release=0.02\n\
             <region> sample=soft layer.wav key=C4 lovel=1 hivel=63\n\
             <region> sample=loud layer.wav key=60 lovel=64 hivel=127\n",
        );
        let piano = SfzPiano::from_asset_pack(&pack).unwrap();
        assert_eq!(piano.region_count(), 2);
        assert_eq!(piano.sample_count(), 2);
        let rack = InstrumentRack::from_asset_pack(&pack).unwrap();
        assert_eq!(rack.catalog()[0].name, "Test SFZ Piano");

        let render_velocity = |velocity| {
            let mut session = rack.get("piano").unwrap().create_session(16_000).unwrap();
            session.send_event(InstrumentEvent::NoteOn {
                pitch: 60,
                velocity,
            });
            let mut left = vec![0.0; 256];
            let mut right = vec![0.0; 256];
            session.render(&mut left, &mut right);
            energy(&left)
        };
        assert!(render_velocity(100) > render_velocity(40) * 20.0);
    }

    #[test]
    fn performance_preload_keeps_only_reachable_velocity_layers() {
        let directory = TestDirectory::create();
        write_wav(&directory.0.join("soft.wav"), 0.1, 4_000);
        write_wav(&directory.0.join("loud.wav"), 0.7, 4_000);
        let pack = write_pack(
            &directory,
            "<region> sample=soft.wav key=60 lovel=1 hivel=63\n\
             <region> sample=loud.wav key=60 lovel=64 hivel=127\n",
        );
        let piano = SfzPiano::from_asset_pack_for_performance(&pack, &[(60, 100)]).unwrap();
        assert_eq!(piano.region_count(), 1);
        assert_eq!(piano.sample_count(), 1);
        assert!(piano.decoded_sample_bytes() > 0);
    }

    #[test]
    fn applies_initial_cc_amplitude_and_runtime_cc_conditions() {
        let directory = TestDirectory::create();
        write_wav(&directory.0.join("tone.wav"), 0.6, 4_000);
        let pack = write_pack(
            &directory,
            "<control> set_cc7=64 set_cc20=0\n\
             <global> amp_veltrack=0 amplitude_oncc7=100\n\
             <master> locc20=1\n\
             <region> sample=tone.wav key=60\n",
        );
        let piano = SfzPiano::from_asset_pack(&pack).unwrap();
        let mut session = piano.create_session(16_000).unwrap();
        session.send_event(InstrumentEvent::NoteOn {
            pitch: 60,
            velocity: 100,
        });
        let mut left = vec![0.0; 256];
        let mut right = vec![0.0; 256];
        session.render(&mut left, &mut right);
        assert_eq!(energy(&left), 0.0);

        session.send_event(InstrumentEvent::ControlChange {
            controller: 20,
            value: 127,
        });
        session.send_event(InstrumentEvent::ControlChange {
            controller: 7,
            value: 127,
        });
        session.send_event(InstrumentEvent::NoteOn {
            pitch: 60,
            velocity: 100,
        });
        session.render(&mut left, &mut right);
        assert!(energy(&left) > 0.1);
    }

    #[test]
    fn control_triggered_regions_render_pedal_mechanical_samples() {
        let directory = TestDirectory::create();
        write_wav(&directory.0.join("pedal-down.wav"), 0.5, 4_000);
        write_wav(&directory.0.join("pedal-up.wav"), 0.25, 4_000);
        let pack = write_pack(
            &directory,
            "<master> lokey=-1 hikey=-1 group=1 polyphony=1 off_time=0.01\n\
             <group> on_locc64=127 on_hicc64=127 locc64=127 hicc64=127 amp_veltrack=0\n\
             <region> sample=pedal-down.wav\n\
             <group> on_locc64=0 on_hicc64=0 locc64=0 hicc64=0 amp_veltrack=0\n\
             <region> sample=pedal-up.wav\n",
        );
        let piano = SfzPiano::from_asset_pack(&pack).unwrap();
        assert_eq!(piano.region_count(), 2);
        let mut session = piano.create_session(16_000).unwrap();
        let mut left = vec![0.0; 256];
        let mut right = vec![0.0; 256];

        session.send_event(InstrumentEvent::ControlChange {
            controller: 64,
            value: 127,
        });
        session.render(&mut left, &mut right);
        let down = energy(&left);
        assert!(down > 0.1);

        session.send_event(InstrumentEvent::ControlChange {
            controller: 64,
            value: 0,
        });
        session.render(&mut left, &mut right);
        let up = energy(&left);
        assert!(up > 0.01);
        assert!(down > up);
    }

    #[test]
    fn applies_rt_decay_to_release_samples_using_hold_time() {
        let directory = TestDirectory::create();
        write_wav(&directory.0.join("silent.wav"), 0.0, 20_000);
        write_wav(&directory.0.join("release.wav"), 0.6, 20_000);
        let pack = write_pack(
            &directory,
            "<global> amp_veltrack=0\n\
             <region> sample=silent.wav key=60\n\
             <region> sample=release.wav key=60 trigger=release rt_decay=20\n",
        );
        let piano = SfzPiano::from_asset_pack(&pack).unwrap();
        let render_release = |hold_frames| {
            let mut session = piano.create_session(16_000).unwrap();
            session.send_event(InstrumentEvent::NoteOn {
                pitch: 60,
                velocity: 100,
            });
            if hold_frames > 0 {
                let mut held = vec![0.0; hold_frames];
                let mut right = vec![0.0; hold_frames];
                session.render(&mut held, &mut right);
            }
            session.send_event(InstrumentEvent::NoteOff { pitch: 60 });
            let mut released = vec![0.0; 256];
            let mut right = vec![0.0; 256];
            session.render(&mut released, &mut right);
            energy(&released)
        };
        let immediate = render_release(0);
        let held = render_release(8_000);
        assert!(immediate > held * 5.0);
    }

    #[test]
    fn note_polyphony_fades_older_same_pitch_voice() {
        let directory = TestDirectory::create();
        write_wav(&directory.0.join("tone.wav"), 0.4, 20_000);
        let pack = write_pack(
            &directory,
            "<global> amp_veltrack=0 note_polyphony=1 off_time=0.005\n\
             <region> sample=tone.wav key=60\n",
        );
        let piano = SfzPiano::from_asset_pack(&pack).unwrap();
        let mut session = SfzPianoSession::new(
            piano.regions.clone(),
            16_000,
            piano.initial_cc,
            piano.curves.clone(),
        );
        session.note_on(60, 80);
        let mut left = vec![0.0; 64];
        let mut right = vec![0.0; 64];
        session.render(&mut left, &mut right);
        session.note_on(60, 100);
        left.resize(128, 0.0);
        right.resize(128, 0.0);
        session.render(&mut left, &mut right);
        assert_eq!(session.voices.len(), 1);
    }

    #[test]
    fn sustain_defers_release_until_the_pedal_lifts() {
        let directory = TestDirectory::create();
        write_wav(&directory.0.join("tone.wav"), 0.5, 20_000);
        let pack = write_pack(
            &directory,
            "<region> sample=tone.wav key=60 amp_veltrack=0 ampeg_release=0.01\n",
        );
        let piano = SfzPiano::from_asset_pack(&pack).unwrap();
        let mut session = piano.create_session(16_000).unwrap();
        session.send_event(InstrumentEvent::NoteOn {
            pitch: 60,
            velocity: 90,
        });
        session.send_event(InstrumentEvent::ControlChange {
            controller: 64,
            value: 127,
        });
        session.send_event(InstrumentEvent::NoteOff { pitch: 60 });
        let mut sustained = vec![0.0; 512];
        let mut right = vec![0.0; 512];
        session.render(&mut sustained, &mut right);
        assert!(energy(&sustained[384..]) > 0.1);

        session.send_event(InstrumentEvent::ControlChange {
            controller: 64,
            value: 0,
        });
        let mut released = vec![0.0; 512];
        session.render(&mut released, &mut right);
        assert!(energy(&released[384..]) < 1.0e-8);
    }

    #[test]
    fn release_regions_sound_only_after_note_off() {
        let directory = TestDirectory::create();
        write_wav(&directory.0.join("silent.wav"), 0.0, 4_000);
        write_wav(&directory.0.join("release.wav"), 0.5, 4_000);
        let pack = write_pack(
            &directory,
            "<region> sample=silent.wav key=60 amp_veltrack=0\n\
             <region> sample=release.wav key=60 amp_veltrack=0 trigger=release\n",
        );
        let piano = SfzPiano::from_asset_pack(&pack).unwrap();
        let mut session = piano.create_session(16_000).unwrap();
        session.send_event(InstrumentEvent::NoteOn {
            pitch: 60,
            velocity: 90,
        });
        let mut left = vec![0.0; 256];
        let mut right = vec![0.0; 256];
        session.render(&mut left, &mut right);
        assert_eq!(energy(&left), 0.0);
        session.send_event(InstrumentEvent::NoteOff { pitch: 60 });
        session.render(&mut left, &mut right);
        assert!(energy(&left) > 0.1);
    }

    #[test]
    fn reports_missing_includes_and_applies_master_inheritance() {
        let include_directory = TestDirectory::create();
        let include_pack = write_pack(&include_directory, "#include \"notes.sfz\"\n");
        assert!(matches!(
            SfzPiano::from_asset_pack(&include_pack),
            Err(SfzPianoError::Preprocess { ref message, .. }) if message.contains("notes.sfz")
        ));

        let master_directory = TestDirectory::create();
        write_wav(&master_directory.0.join("tone.wav"), 0.5, 4_000);
        fs::write(
            master_directory.0.join("region.sfz"),
            "<region> sample=tone.wav key=$KEY\n",
        )
        .unwrap();
        let master_pack = write_pack(
            &master_directory,
            "#define $KEY C4\n\
             <global> amp_veltrack=0 trigger=release\n\
             <master> volume=-3\n\
             #include \"region.sfz\"\n",
        );
        let piano = SfzPiano::from_asset_pack(&master_pack).unwrap();
        let mut session = piano.create_session(16_000).unwrap();
        session.send_event(InstrumentEvent::NoteOn {
            pitch: 60,
            velocity: 90,
        });
        let mut left = vec![0.0; 128];
        let mut right = vec![0.0; 128];
        session.render(&mut left, &mut right);
        assert_eq!(energy(&left), 0.0);
        session.send_event(InstrumentEvent::NoteOff { pitch: 60 });
        session.render(&mut left, &mut right);
        assert!(energy(&left) > 0.01);
    }

    #[test]
    fn rejects_unknown_playback_opcodes_instead_of_ignoring_them() {
        let directory = TestDirectory::create();
        let pack = write_pack(
            &directory,
            "<region> sample=missing.wav key=60 cutoff=1000\n",
        );
        assert!(matches!(
            SfzPiano::from_asset_pack(&pack),
            Err(SfzPianoError::UnsupportedOpcode { ref opcode, .. }) if opcode == "cutoff"
        ));
    }

    #[test]
    fn rejects_sample_paths_that_leave_the_pack() {
        let directory = TestDirectory::create();
        let outside = directory.0.with_extension("outside.wav");
        write_wav(&outside, 0.2, 100);
        let pack = write_pack(&directory, "<region> sample=../outside.wav key=60\n");
        assert!(matches!(
            SfzPiano::from_asset_pack(&pack),
            Err(SfzPianoError::InvalidSamplePath { .. })
        ));
        let _ = fs::remove_file(outside);
    }

    #[test]
    fn decodes_flac_samples_when_the_reference_encoder_is_available() {
        if Command::new("flac").arg("--version").output().is_err() {
            return;
        }
        let directory = TestDirectory::create();
        let wav = directory.0.join("tone.wav");
        let flac = directory.0.join("tone.flac");
        write_pcm_wav(&wav, 0.4, 4_000);
        let status = Command::new("flac")
            .args(["--force", "--silent", "--output-name"])
            .arg(&flac)
            .arg(&wav)
            .status()
            .unwrap();
        assert!(status.success());
        let pack = write_pack(&directory, "<region> sample=tone.flac key=60\n");
        let piano = SfzPiano::from_asset_pack(&pack).unwrap();
        let mut session = piano.create_session(16_000).unwrap();
        session.send_event(InstrumentEvent::NoteOn {
            pitch: 60,
            velocity: 100,
        });
        let mut left = vec![0.0; 256];
        let mut right = vec![0.0; 256];
        session.render(&mut left, &mut right);
        assert!(energy(&left) > 0.1);
    }

    #[test]
    fn loads_a_salamander_style_macro_include_master_and_curve_fixture() {
        let directory = TestDirectory::create();
        fs::create_dir(directory.0.join("Data")).unwrap();
        write_wav(&directory.0.join("natural.wav"), 0.4, 4_000);
        write_wav(&directory.0.join("retuned.wav"), 0.4, 4_000);
        fs::write(
            directory.0.join("Data/natural.sfz"),
            "<region> sample=natural.wav key=60\n",
        )
        .unwrap();
        fs::write(
            directory.0.join("Data/retuned.sfz"),
            "<region> sample=retuned.wav key=60\n",
        )
        .unwrap();
        let pack = write_pack(
            &directory,
            "#define $NATURAL C0\n\
             #define $RETUNED C#0\n\
             <control> set_hdcc20=0.5\n\
             <global> amp_veltrack=0 amplitude_oncc20=100 amplitude_curvecc20=7 sw_default=$NATURAL\n\
             <master> sw_last=$NATURAL\n\
             #include \"Data/natural.sfz\"\n\
             <master> sw_last=$RETUNED\n\
             #include \"Data/retuned.sfz\"\n\
             <curve> curve_index=7 v000=0 v064=0.5 v127=1\n",
        );
        let piano = SfzPiano::from_asset_pack(&pack).unwrap();
        assert_eq!(piano.region_count(), 1);
        assert_eq!(piano.sample_count(), 1);
        assert_eq!(piano.initial_cc[20], 64);
    }
}
