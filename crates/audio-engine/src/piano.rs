use super::{Instrument, InstrumentError, InstrumentEvent, InstrumentSession};
use std::f32::consts::{PI, TAU};

const MAX_VOICES: usize = 96;
const PIANO_LOWEST_PITCH: u8 = 21;
const PIANO_HIGHEST_PITCH: u8 = 108;
const STRING_PARTIALS: usize = 9;

/// An asset-free, deterministic piano model.
///
/// The public instrument seam stays intentionally small.  Internally the
/// model combines velocity-dependent hammer excitation, one to three
/// slightly detuned strings, inharmonic modal partials, key/pedal damping,
/// sympathetic strings, and a stereo soundboard.  It is not a circuit-level
/// simulation; it is a realtime-oriented physical approximation that gives
/// the editor a useful built-in piano even when no sample pack is installed.
#[derive(Clone, Debug)]
pub struct PianoSynth {
    /// Controls the amount of upper-partial energy produced by the hammer.
    pub brightness: f32,
    /// Base string inharmonicity.  Piano strings are stiff, so their partials
    /// are slightly sharper than an ideal harmonic series.
    pub stretch: f32,
}

impl Default for PianoSynth {
    fn default() -> Self {
        Self {
            brightness: 0.82,
            stretch: 0.0008,
        }
    }
}

impl Instrument for PianoSynth {
    fn create_session(
        &self,
        sample_rate: u32,
    ) -> Result<Box<dyn InstrumentSession>, InstrumentError> {
        Ok(Box::new(self.session(sample_rate)))
    }

    fn tail_seconds(&self) -> f32 {
        4.8
    }
}

impl PianoSynth {
    fn session(&self, sample_rate: u32) -> PianoSession {
        let sample_rate = sample_rate as f32;
        PianoSession {
            sample_rate,
            brightness: self.brightness.clamp(0.05, 1.5),
            stretch: self.stretch.clamp(0.0, 0.004),
            damper_pedal: 0.0,
            soft_pedal: false,
            voices: Vec::new(),
            sympathetic: SympatheticBank::new(sample_rate),
            soundboard: Soundboard::new(sample_rate),
        }
    }
}

struct PianoSession {
    sample_rate: f32,
    brightness: f32,
    stretch: f32,
    damper_pedal: f32,
    soft_pedal: bool,
    voices: Vec<PianoVoice>,
    sympathetic: SympatheticBank,
    soundboard: Soundboard,
}

impl PianoSession {
    fn note_on(&mut self, pitch: u8, velocity: u8) {
        if velocity == 0 {
            self.note_off(pitch);
            return;
        }

        self.sympathetic.note_on(pitch, velocity, self.damper_pedal);
        self.soundboard.strike(pitch, velocity);
        if self.voices.len() >= MAX_VOICES {
            self.steal_quietest_voice();
        }
        self.voices.push(PianoVoice::new(
            pitch,
            velocity,
            self.sample_rate,
            self.brightness,
            self.stretch,
            self.soft_pedal,
        ));
    }

    fn note_off(&mut self, pitch: u8) {
        self.sympathetic.note_off(pitch);
        // MIDI NoteOff releases the oldest matching active key. Releasing
        // every same-pitch voice would truncate legitimate overlapping notes.
        if let Some(voice) = self
            .voices
            .iter_mut()
            .find(|voice| voice.pitch == pitch && !voice.key_released)
        {
            voice.key_released = true;
            voice.release();
        }
    }

    fn set_damper_pedal(&mut self, position: f32) {
        let previous = self.damper_pedal;
        self.damper_pedal = position.clamp(0.0, 1.0);
        self.sympathetic.set_damper_pedal(self.damper_pedal);
        let was_down = previous >= 0.5;
        let is_down = self.damper_pedal >= 0.5;
        if was_down != is_down {
            self.soundboard.pedal_change(is_down);
        }
    }

    fn all_notes_off(&mut self) {
        self.sympathetic.all_notes_off();
        for voice in &mut self.voices {
            voice.key_released = true;
            voice.release();
        }
    }

    fn reset(&mut self) {
        self.voices.clear();
        self.damper_pedal = 0.0;
        self.soft_pedal = false;
        self.sympathetic = SympatheticBank::new(self.sample_rate);
        self.soundboard = Soundboard::new(self.sample_rate);
    }

    fn steal_quietest_voice(&mut self) {
        let candidate = self
            .voices
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| left.energy().total_cmp(&right.energy()))
            .map(|(index, _)| index);
        if let Some(index) = candidate {
            self.voices.swap_remove(index);
        }
    }
}

impl InstrumentSession for PianoSession {
    fn send_event(&mut self, event: InstrumentEvent) {
        match event {
            InstrumentEvent::NoteOn { pitch, velocity } => self.note_on(pitch, velocity),
            InstrumentEvent::NoteOff { pitch } => self.note_off(pitch),
            InstrumentEvent::ControlChange {
                controller: 64,
                value,
            } => self.set_damper_pedal(value as f32 / 127.0),
            InstrumentEvent::ControlChange {
                controller: 67,
                value,
            } => self.soft_pedal = value >= 64,
            InstrumentEvent::ControlChange {
                controller: 120, ..
            } => self.reset(),
            InstrumentEvent::ControlChange {
                controller: 121, ..
            } => {
                self.soft_pedal = false;
                self.set_damper_pedal(0.0);
            }
            InstrumentEvent::ControlChange {
                controller: 123, ..
            } => self.all_notes_off(),
            InstrumentEvent::ControlChange { .. } => {}
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        assert_eq!(left.len(), right.len());
        left.fill(0.0);
        right.fill(0.0);

        for frame in 0..left.len() {
            let mut dry_left = 0.0_f32;
            let mut dry_right = 0.0_f32;
            for voice in &mut self.voices {
                let (voice_left, voice_right) =
                    voice.next_sample(self.sample_rate, self.damper_pedal);
                dry_left += voice_left;
                dry_right += voice_right;
            }

            let (resonance_left, resonance_right) = self.sympathetic.next_sample();
            dry_left += resonance_left;
            dry_right += resonance_right;
            let (board_left, board_right) =
                self.soundboard
                    .next_sample(dry_left, dry_right, self.damper_pedal);

            left[frame] = soft_limit(dry_left + board_left);
            right[frame] = soft_limit(dry_right + board_right);
        }

        self.voices.retain(|voice| !voice.finished());
    }
}

struct PianoVoice {
    pitch: u8,
    modes: Vec<StringMode>,
    hammer: Hammer,
    age_frames: u64,
    released_frames: Option<u64>,
    key_released: bool,
    damper_amount: f32,
    pan_left: f32,
    pan_right: f32,
}

impl PianoVoice {
    fn new(
        pitch: u8,
        velocity: u8,
        sample_rate: f32,
        brightness: f32,
        stretch: f32,
        soft_pedal: bool,
    ) -> Self {
        let frequency = midi_frequency(pitch);
        let velocity = velocity as f32 / 127.0;
        let string_count = string_count(pitch, soft_pedal);
        let mut modes = Vec::with_capacity(string_count * STRING_PARTIALS);
        let pitch_position = ((pitch as f32 - 64.5) / 43.5).clamp(-1.0, 1.0);
        let keyboard_pan = pitch_position * 0.42;
        let soft_gain = if soft_pedal { 0.72 } else { 1.0 };
        let velocity_gain = (0.055 + velocity.powf(1.35) * 0.245) * soft_gain;
        let string_gain = 1.0 / (string_count as f32).sqrt();

        for string_index in 0..string_count {
            let detune = string_detune_cents(string_count, string_index, pitch);
            let string_frequency = frequency * 2.0_f32.powf(detune / 1200.0);
            let string_pan =
                keyboard_pan + (string_index as f32 - (string_count - 1) as f32 * 0.5) * 0.035;
            let (string_left, string_right) = equal_power_pan(string_pan);

            for partial_index in 1..=STRING_PARTIALS {
                let partial = partial_index as f32;
                let pitch_extreme = ((pitch as f32 - 64.0).abs() / 44.0).powi(2);
                let inharmonicity = stretch * (0.62 + pitch_extreme * 0.9);
                let ratio = partial * (1.0 + inharmonicity * partial * partial).sqrt();
                let mode_frequency = string_frequency * ratio;
                if mode_frequency >= sample_rate * 0.47 {
                    break;
                }

                let spectral_slope = 1.78 - velocity * brightness * 0.56;
                let contact_filter = (-partial * partial * (0.008 + (1.0 - velocity) * 0.014)
                    / brightness.max(0.1))
                .exp();
                let mode_gain = velocity_gain * string_gain * contact_filter
                    / partial.powf(spectral_slope.max(0.9));
                let fundamental_t60 = string_t60(pitch);
                let mode_t60 = fundamental_t60 / (1.0 + 0.16 * (partial - 1.0).powf(1.28));
                let damper_t60 = 0.11 + (1.0 - pitch_position.max(0.0)) * 0.16;
                let seed = (pitch as u32) << 16 | (string_index as u32) << 8 | partial_index as u32;
                let phase = (hash_unit(seed) - 0.5) * 0.09 * PI;
                modes.push(StringMode::new(
                    mode_frequency,
                    mode_gain,
                    phase,
                    mode_t60,
                    damper_t60,
                    sample_rate,
                    string_left,
                    string_right,
                ));
            }
        }

        let (pan_left, pan_right) = equal_power_pan(keyboard_pan);
        Self {
            pitch,
            modes,
            hammer: Hammer::new(pitch, velocity, brightness, sample_rate, soft_pedal),
            age_frames: 0,
            released_frames: None,
            key_released: false,
            damper_amount: 0.0,
            pan_left,
            pan_right,
        }
    }

    fn release(&mut self) {
        if self.released_frames.is_none() {
            self.released_frames = Some(0);
        }
    }

    fn next_sample(&mut self, sample_rate: f32, damper_pedal: f32) -> (f32, f32) {
        let attack_frames = (sample_rate * 0.0025).max(1.0);
        let attack = (self.age_frames as f32 / attack_frames).clamp(0.0, 1.0);
        let damper_target = if self.key_released {
            1.0 - damper_pedal.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let damper_seconds = if damper_target > self.damper_amount {
            0.006
        } else {
            0.035
        };
        self.damper_amount = move_towards(
            self.damper_amount,
            damper_target,
            1.0 / (sample_rate * damper_seconds).max(1.0),
        );

        let mut left = 0.0_f32;
        let mut right = 0.0_f32;
        for mode in &mut self.modes {
            let sample = mode.next_sample(self.damper_amount) * attack;
            left += sample * mode.pan_left;
            right += sample * mode.pan_right;
        }

        let hammer = self.hammer.next_sample();
        left += hammer * self.pan_left;
        right += hammer * self.pan_right;
        self.age_frames += 1;
        if let Some(frames) = &mut self.released_frames {
            *frames += 1;
        }
        (left, right)
    }

    fn energy(&self) -> f32 {
        self.modes.iter().map(StringMode::energy).sum::<f32>() + self.hammer.energy()
    }

    fn finished(&self) -> bool {
        let Some(released_frames) = self.released_frames else {
            return false;
        };
        released_frames > 256 && self.energy() < 1.0e-10
    }
}

struct StringMode {
    real: f32,
    imaginary: f32,
    step_cos: f32,
    step_sin: f32,
    open_decay: f32,
    damper_decay: f32,
    pan_left: f32,
    pan_right: f32,
}

impl StringMode {
    #[allow(clippy::too_many_arguments)]
    fn new(
        frequency: f32,
        gain: f32,
        phase: f32,
        open_t60: f32,
        damper_t60: f32,
        sample_rate: f32,
        pan_left: f32,
        pan_right: f32,
    ) -> Self {
        let phase_step = TAU * frequency / sample_rate;
        Self {
            real: gain * phase.cos(),
            imaginary: gain * phase.sin(),
            step_cos: phase_step.cos(),
            step_sin: phase_step.sin(),
            open_decay: t60_decay(open_t60, sample_rate),
            damper_decay: t60_decay(damper_t60, sample_rate),
            pan_left,
            pan_right,
        }
    }

    fn next_sample(&mut self, damper_amount: f32) -> f32 {
        let output = self.imaginary;
        let decay = self.open_decay * (1.0 - damper_amount * (1.0 - self.damper_decay));
        let next_real = (self.real * self.step_cos - self.imaginary * self.step_sin) * decay;
        let next_imaginary = (self.imaginary * self.step_cos + self.real * self.step_sin) * decay;
        self.real = next_real;
        self.imaginary = next_imaginary;
        output
    }

    fn energy(&self) -> f32 {
        self.real * self.real + self.imaginary * self.imaginary
    }
}

struct Hammer {
    rng: u32,
    remaining_frames: u32,
    total_frames: u32,
    low_pass: f32,
    filter_amount: f32,
    level: f32,
    thump_phase: f32,
    thump_step: f32,
}

impl Hammer {
    fn new(pitch: u8, velocity: f32, brightness: f32, sample_rate: f32, soft_pedal: bool) -> Self {
        let duration = 0.008 + (1.0 - pitch as f32 / 127.0) * 0.012;
        let total_frames = (duration * sample_rate).round().max(1.0) as u32;
        let soft_gain = if soft_pedal { 0.55 } else { 1.0 };
        Self {
            rng: 0xA511_E9B3 ^ (pitch as u32).wrapping_mul(0x9E37_79B9),
            remaining_frames: total_frames,
            total_frames,
            low_pass: 0.0,
            filter_amount: (0.08 + velocity * brightness * 0.5).clamp(0.05, 0.72),
            level: velocity.powf(1.45) * 0.052 * soft_gain,
            thump_phase: 0.0,
            thump_step: TAU * (72.0 + midi_frequency(pitch) * 0.08).min(420.0) / sample_rate,
        }
    }

    fn next_sample(&mut self) -> f32 {
        if self.remaining_frames == 0 {
            return 0.0;
        }
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        let noise = (self.rng as f32 / u32::MAX as f32) * 2.0 - 1.0;
        self.low_pass += (noise - self.low_pass) * self.filter_amount;
        let progress = 1.0 - self.remaining_frames as f32 / self.total_frames as f32;
        let envelope = (1.0 - progress).powi(3) * (progress * 16.0).min(1.0);
        let thump = self.thump_phase.sin() * (1.0 - progress).powi(4);
        self.thump_phase += self.thump_step;
        self.remaining_frames -= 1;
        (self.low_pass * 0.68 + thump * 0.32) * envelope * self.level
    }

    fn energy(&self) -> f32 {
        if self.remaining_frames == 0 {
            0.0
        } else {
            self.level * self.level
        }
    }
}

struct SympatheticBank {
    strings: Vec<SympatheticString>,
    damper_pedal: f32,
}

impl SympatheticBank {
    fn new(sample_rate: f32) -> Self {
        let strings = (PIANO_LOWEST_PITCH..=PIANO_HIGHEST_PITCH)
            .map(|pitch| SympatheticString::new(pitch, sample_rate))
            .collect();
        Self {
            strings,
            damper_pedal: 0.0,
        }
    }

    fn set_damper_pedal(&mut self, position: f32) {
        self.damper_pedal = position.clamp(0.0, 1.0);
    }

    fn note_on(&mut self, pitch: u8, velocity: u8, damper_pedal: f32) {
        if let Some(string) = self.string_mut(pitch) {
            string.held_count = string.held_count.saturating_add(1);
        }
        let source_frequency = midi_frequency(pitch);
        let velocity = velocity as f32 / 127.0;
        for string in &mut self.strings {
            let openness = if string.held_count > 0 {
                1.0
            } else {
                damper_pedal
            };
            if string.pitch == pitch || openness <= 0.02 {
                continue;
            }
            let score = harmonic_match(source_frequency, string.frequency);
            if score > 0.08 {
                string.excite(velocity.powf(1.3) * score * openness * 0.012);
            }
        }
    }

    fn note_off(&mut self, pitch: u8) {
        if let Some(string) = self.string_mut(pitch) {
            string.held_count = string.held_count.saturating_sub(1);
        }
    }

    fn all_notes_off(&mut self) {
        for string in &mut self.strings {
            string.held_count = 0;
        }
    }

    fn next_sample(&mut self) -> (f32, f32) {
        let mut left = 0.0_f32;
        let mut right = 0.0_f32;
        for string in &mut self.strings {
            if string.energy() < 1.0e-14 {
                string.clear();
                continue;
            }
            let damper_amount = if string.held_count > 0 {
                0.0
            } else {
                1.0 - self.damper_pedal
            };
            let sample = string.next_sample(damper_amount);
            left += sample * string.pan_left;
            right += sample * string.pan_right;
        }
        (left * 0.32, right * 0.32)
    }

    fn string_mut(&mut self, pitch: u8) -> Option<&mut SympatheticString> {
        pitch
            .checked_sub(PIANO_LOWEST_PITCH)
            .and_then(|index| self.strings.get_mut(index as usize))
    }
}

struct SympatheticString {
    pitch: u8,
    frequency: f32,
    real: f32,
    imaginary: f32,
    step_cos: f32,
    step_sin: f32,
    open_decay: f32,
    damped_decay: f32,
    held_count: u16,
    pan_left: f32,
    pan_right: f32,
}

impl SympatheticString {
    fn new(pitch: u8, sample_rate: f32) -> Self {
        let frequency = midi_frequency(pitch);
        let phase_step = TAU * frequency / sample_rate;
        let position = ((pitch as f32 - 64.5) / 43.5).clamp(-1.0, 1.0) * 0.48;
        let (pan_left, pan_right) = equal_power_pan(position);
        Self {
            pitch,
            frequency,
            real: 0.0,
            imaginary: 0.0,
            step_cos: phase_step.cos(),
            step_sin: phase_step.sin(),
            open_decay: t60_decay(string_t60(pitch) * 0.72, sample_rate),
            damped_decay: t60_decay(0.095, sample_rate),
            held_count: 0,
            pan_left,
            pan_right,
        }
    }

    fn excite(&mut self, amount: f32) {
        self.real += amount;
    }

    fn next_sample(&mut self, damper_amount: f32) -> f32 {
        let output = self.imaginary;
        let damper_amount = damper_amount.clamp(0.0, 1.0);
        let decay = self.open_decay * (1.0 - damper_amount * (1.0 - self.damped_decay));
        let next_real = (self.real * self.step_cos - self.imaginary * self.step_sin) * decay;
        let next_imaginary = (self.imaginary * self.step_cos + self.real * self.step_sin) * decay;
        self.real = next_real;
        self.imaginary = next_imaginary;
        output
    }

    fn energy(&self) -> f32 {
        self.real * self.real + self.imaginary * self.imaginary
    }

    fn clear(&mut self) {
        self.real = 0.0;
        self.imaginary = 0.0;
    }
}

struct Soundboard {
    modes: Vec<BoardMode>,
    delays: Vec<FeedbackDelay>,
    pedal_impulse: f32,
    pedal_rng: u32,
}

impl Soundboard {
    fn new(sample_rate: f32) -> Self {
        let mode_specs = [
            (58.0, 2.7, -0.52),
            (91.0, 2.4, 0.44),
            (143.0, 2.2, -0.31),
            (219.0, 1.9, 0.28),
            (337.0, 1.65, -0.2),
            (512.0, 1.4, 0.18),
            (781.0, 1.15, -0.15),
            (1_173.0, 0.92, 0.13),
            (1_827.0, 0.68, -0.11),
            (2_731.0, 0.48, 0.1),
        ];
        let modes = mode_specs
            .into_iter()
            .filter(|(frequency, _, _)| *frequency < sample_rate * 0.45)
            .enumerate()
            .map(|(index, (frequency, t60, pan))| {
                BoardMode::new(
                    frequency,
                    t60,
                    0.0075 / (index as f32 + 1.0).sqrt(),
                    pan,
                    sample_rate,
                )
            })
            .collect();
        let delay_specs = [
            (0.0297, 0.71, 0.23, -0.72),
            (0.0371, 0.73, 0.27, 0.61),
            (0.0411, 0.70, 0.31, -0.38),
            (0.0437, 0.72, 0.35, 0.34),
        ];
        let delays = delay_specs
            .into_iter()
            .map(|(seconds, feedback, damping, pan)| {
                FeedbackDelay::new(seconds, feedback, damping, pan, sample_rate)
            })
            .collect();
        Self {
            modes,
            delays,
            pedal_impulse: 0.0,
            pedal_rng: 0xC001_CAFE,
        }
    }

    fn strike(&mut self, pitch: u8, velocity: u8) {
        let position = ((pitch as f32 - 64.0) / 44.0).clamp(-1.0, 1.0);
        let amount = (velocity as f32 / 127.0).powf(1.2) * 0.0025;
        for (index, mode) in self.modes.iter_mut().enumerate() {
            let spread = 1.0 + position * ((index % 3) as f32 - 1.0) * 0.08;
            mode.excite(amount * spread);
        }
    }

    fn pedal_change(&mut self, down: bool) {
        self.pedal_impulse += if down { 0.012 } else { -0.008 };
    }

    fn next_sample(&mut self, dry_left: f32, dry_right: f32, damper_pedal: f32) -> (f32, f32) {
        let mono = (dry_left + dry_right) * 0.5;
        self.pedal_rng ^= self.pedal_rng << 13;
        self.pedal_rng ^= self.pedal_rng >> 17;
        self.pedal_rng ^= self.pedal_rng << 5;
        let pedal_noise =
            ((self.pedal_rng as f32 / u32::MAX as f32) * 2.0 - 1.0) * self.pedal_impulse;
        self.pedal_impulse *= 0.994;
        if self.pedal_impulse.abs() < 1.0e-6 {
            self.pedal_impulse = 0.0;
        }

        let excitation = mono * (0.095 + damper_pedal.clamp(0.0, 1.0) * 0.055) + pedal_noise;
        let mut modal_left = 0.0_f32;
        let mut modal_right = 0.0_f32;
        for mode in &mut self.modes {
            let (left, right) = mode.next_sample(excitation);
            modal_left += left;
            modal_right += right;
        }

        let room_input = mono * 0.032 + (modal_left + modal_right) * 0.12;
        let mut room_left = 0.0_f32;
        let mut room_right = 0.0_f32;
        for delay in &mut self.delays {
            let sample = delay.next_sample(room_input);
            room_left += sample * delay.pan_left;
            room_right += sample * delay.pan_right;
        }
        (
            modal_left + room_left * 0.22,
            modal_right + room_right * 0.22,
        )
    }
}

struct BoardMode {
    real: f32,
    imaginary: f32,
    step_cos: f32,
    step_sin: f32,
    decay: f32,
    excitation_gain: f32,
    pan_left: f32,
    pan_right: f32,
}

impl BoardMode {
    fn new(frequency: f32, t60: f32, gain: f32, pan: f32, sample_rate: f32) -> Self {
        let phase_step = TAU * frequency / sample_rate;
        let (pan_left, pan_right) = equal_power_pan(pan);
        Self {
            real: 0.0,
            imaginary: 0.0,
            step_cos: phase_step.cos(),
            step_sin: phase_step.sin(),
            decay: t60_decay(t60, sample_rate),
            excitation_gain: gain,
            pan_left,
            pan_right,
        }
    }

    fn excite(&mut self, amount: f32) {
        self.real += amount * self.excitation_gain;
    }

    fn next_sample(&mut self, input: f32) -> (f32, f32) {
        self.real += input * self.excitation_gain;
        let output = self.imaginary;
        let next_real = (self.real * self.step_cos - self.imaginary * self.step_sin) * self.decay;
        let next_imaginary =
            (self.imaginary * self.step_cos + self.real * self.step_sin) * self.decay;
        self.real = next_real;
        self.imaginary = next_imaginary;
        (output * self.pan_left, output * self.pan_right)
    }
}

struct FeedbackDelay {
    buffer: Vec<f32>,
    cursor: usize,
    feedback: f32,
    damping: f32,
    filtered: f32,
    pan_left: f32,
    pan_right: f32,
}

impl FeedbackDelay {
    fn new(seconds: f32, feedback: f32, damping: f32, pan: f32, sample_rate: f32) -> Self {
        let length = (seconds * sample_rate).round().max(1.0) as usize;
        let (pan_left, pan_right) = equal_power_pan(pan);
        Self {
            buffer: vec![0.0; length],
            cursor: 0,
            feedback,
            damping,
            filtered: 0.0,
            pan_left,
            pan_right,
        }
    }

    fn next_sample(&mut self, input: f32) -> f32 {
        let delayed = self.buffer[self.cursor];
        self.filtered += (delayed - self.filtered) * self.damping;
        self.buffer[self.cursor] = input + self.filtered * self.feedback;
        self.cursor += 1;
        if self.cursor == self.buffer.len() {
            self.cursor = 0;
        }
        delayed
    }
}

fn midi_frequency(pitch: u8) -> f32 {
    440.0 * 2.0_f32.powf((pitch as f32 - 69.0) / 12.0)
}

fn string_count(pitch: u8, soft_pedal: bool) -> usize {
    if soft_pedal {
        match pitch {
            0..=34 => 1,
            _ => 2,
        }
    } else {
        match pitch {
            0..=34 => 1,
            35..=51 => 2,
            _ => 3,
        }
    }
}

fn string_detune_cents(string_count: usize, index: usize, pitch: u8) -> f32 {
    let pitch_scale = 0.75 + (pitch as f32 / 127.0) * 0.55;
    let cents = match string_count {
        1 => 0.0,
        2 => [-0.62, 0.58][index],
        _ => [-0.86, 0.0, 0.79][index],
    };
    cents * pitch_scale
}

fn string_t60(pitch: u8) -> f32 {
    let position = ((pitch as f32 - PIANO_LOWEST_PITCH as f32)
        / (PIANO_HIGHEST_PITCH - PIANO_LOWEST_PITCH) as f32)
        .clamp(0.0, 1.0);
    8.2 - 6.45 * position.powf(0.72)
}

fn harmonic_match(left_frequency: f32, right_frequency: f32) -> f32 {
    let mut best = 0.0_f32;
    for left_partial in 1..=6 {
        for right_partial in 1..=6 {
            let left = left_frequency * left_partial as f32;
            let right = right_frequency * right_partial as f32;
            let cents = 1200.0 * (left / right).log2().abs();
            let closeness = (-(cents / 17.0).powi(2)).exp();
            let weight = 1.0 / ((left_partial * right_partial) as f32).sqrt();
            best = best.max(closeness * weight);
        }
    }
    best
}

fn equal_power_pan(pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    (((1.0 - pan) * 0.5).sqrt(), ((1.0 + pan) * 0.5).sqrt())
}

fn t60_decay(seconds: f32, sample_rate: f32) -> f32 {
    10.0_f32.powf(-3.0 / (seconds.max(0.01) * sample_rate))
}

fn move_towards(current: f32, target: f32, max_delta: f32) -> f32 {
    let delta = target - current;
    if delta.abs() <= max_delta {
        target
    } else {
        current + delta.signum() * max_delta
    }
}

fn hash_unit(mut value: u32) -> f32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7FEB_352D);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846C_A68B);
    value ^= value >> 16;
    value as f32 / u32::MAX as f32
}

fn soft_limit(value: f32) -> f32 {
    value / (1.0 + value.abs() * 0.24)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_note(velocity: u8, damper_pedal: u8) -> Vec<f32> {
        let instrument = PianoSynth::default();
        let mut session = instrument.create_session(16_000).unwrap();
        session.send_event(InstrumentEvent::NoteOn {
            pitch: 60,
            velocity,
        });
        let mut left = vec![0.0; 1_600];
        let mut right = vec![0.0; 1_600];
        session.render(&mut left, &mut right);
        if damper_pedal > 0 {
            session.send_event(InstrumentEvent::ControlChange {
                controller: 64,
                value: damper_pedal,
            });
        }
        session.send_event(InstrumentEvent::NoteOff { pitch: 60 });
        let mut tail_left = vec![0.0; 12_800];
        let mut tail_right = vec![0.0; 12_800];
        session.render(&mut tail_left, &mut tail_right);
        left.extend(tail_left);
        left
    }

    fn energy(samples: &[f32]) -> f32 {
        samples.iter().map(|sample| sample * sample).sum()
    }

    #[test]
    fn piano_session_keeps_state_between_blocks() {
        let instrument = PianoSynth::default();
        let mut session = instrument.create_session(16_000).unwrap();
        session.send_event(InstrumentEvent::NoteOn {
            pitch: 60,
            velocity: 100,
        });
        let mut left = vec![0.0; 512];
        let mut right = vec![0.0; 512];
        session.render(&mut left, &mut right);
        let first_energy = energy(&left);
        session.render(&mut left, &mut right);
        let second_energy = energy(&left);
        assert!(first_energy > 0.01);
        assert!(second_energy > 0.01);
    }

    #[test]
    fn rendering_is_deterministic() {
        let first = render_note(92, 0);
        let second = render_note(92, 0);
        assert_eq!(first, second);
    }

    #[test]
    fn overlapping_same_pitch_note_off_releases_one_voice_at_a_time() {
        let instrument = PianoSynth::default();
        let mut session = instrument.session(16_000);
        session.note_on(60, 96);
        session.note_on(60, 82);
        assert_eq!(
            session
                .voices
                .iter()
                .filter(|voice| voice.pitch == 60 && !voice.key_released)
                .count(),
            2
        );
        assert_eq!(
            session.sympathetic.strings[60 - PIANO_LOWEST_PITCH as usize].held_count,
            2
        );

        session.note_off(60);
        assert_eq!(
            session
                .voices
                .iter()
                .filter(|voice| voice.pitch == 60 && !voice.key_released)
                .count(),
            1
        );
        assert_eq!(
            session.sympathetic.strings[60 - PIANO_LOWEST_PITCH as usize].held_count,
            1
        );

        session.note_off(60);
        assert_eq!(
            session
                .voices
                .iter()
                .filter(|voice| voice.pitch == 60 && !voice.key_released)
                .count(),
            0
        );
        assert_eq!(
            session.sympathetic.strings[60 - PIANO_LOWEST_PITCH as usize].held_count,
            0
        );
    }

    #[test]
    fn velocity_changes_excitation_energy() {
        let quiet = render_note(35, 0);
        let loud = render_note(118, 0);
        assert!(energy(&loud[..1_600]) > energy(&quiet[..1_600]) * 3.0);
    }

    #[test]
    fn sustain_pedal_keeps_the_strings_open() {
        let damped = render_note(96, 0);
        let sustained = render_note(96, 127);
        let tail_start = 1_600 + 8_000;
        assert!(energy(&sustained[tail_start..]) > energy(&damped[tail_start..]) * 2.0);
    }

    #[test]
    fn half_pedal_produces_intermediate_decay() {
        let damped = render_note(96, 0);
        let half = render_note(96, 72);
        let sustained = render_note(96, 127);
        let tail_start = 1_600 + 4_000;
        let damped_energy = energy(&damped[tail_start..]);
        let half_energy = energy(&half[tail_start..]);
        let sustained_energy = energy(&sustained[tail_start..]);
        assert!(half_energy > damped_energy * 1.2);
        assert!(half_energy < sustained_energy * 0.9);
    }

    #[test]
    fn dense_chord_stays_finite() {
        let instrument = PianoSynth::default();
        let mut session = instrument.create_session(16_000).unwrap();
        for pitch in 36..=96 {
            session.send_event(InstrumentEvent::NoteOn {
                pitch,
                velocity: 110,
            });
        }
        let mut left = vec![0.0; 8_000];
        let mut right = vec![0.0; 8_000];
        session.render(&mut left, &mut right);
        assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
        assert!(
            left.iter()
                .chain(&right)
                .all(|sample| sample.abs() <= 1.0 / 0.24)
        );
    }
}
