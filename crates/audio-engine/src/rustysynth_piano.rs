//! Optional stateful SoundFont adapter using the MIT-licensed RustySynth.

use super::{Instrument, InstrumentError, InstrumentEvent, InstrumentSession};
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RustySynthError {
    #[error("could not open SoundFont: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse SoundFont: {0}")]
    SoundFont(String),
}

#[derive(Clone, Debug)]
pub struct RustySynthPiano {
    sound_font: Arc<SoundFont>,
    pub program: i32,
    pub block_size: usize,
    pub maximum_polyphony: usize,
    pub enable_reverb_and_chorus: bool,
}

impl RustySynthPiano {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, RustySynthError> {
        let mut file = File::open(path)?;
        let sound_font = SoundFont::new(&mut file)
            .map_err(|error| RustySynthError::SoundFont(error.to_string()))?;
        Ok(Self::from_sound_font(sound_font))
    }

    pub fn from_sound_font(sound_font: SoundFont) -> Self {
        Self {
            sound_font: Arc::new(sound_font),
            program: 0,
            block_size: 64,
            maximum_polyphony: 64,
            enable_reverb_and_chorus: true,
        }
    }
}

impl Instrument for RustySynthPiano {
    fn create_session(
        &self,
        sample_rate: u32,
    ) -> Result<Box<dyn InstrumentSession>, InstrumentError> {
        let mut settings = SynthesizerSettings::new(sample_rate as i32);
        settings.block_size = self.block_size;
        settings.maximum_polyphony = self.maximum_polyphony;
        settings.enable_reverb_and_chorus = self.enable_reverb_and_chorus;
        let mut synthesizer = Synthesizer::new(&self.sound_font, &settings)
            .map_err(|error| InstrumentError::Backend(error.to_string()))?;
        synthesizer.process_midi_message(0, 0xC0, self.program, 0);
        Ok(Box::new(RustySynthSession { synthesizer }))
    }

    fn tail_seconds(&self) -> f32 {
        3.0
    }
}

struct RustySynthSession {
    synthesizer: Synthesizer,
}

impl InstrumentSession for RustySynthSession {
    fn send_event(&mut self, event: InstrumentEvent) {
        match event {
            InstrumentEvent::NoteOn { pitch, velocity } => {
                self.synthesizer.note_on(0, pitch as i32, velocity as i32);
            }
            InstrumentEvent::NoteOff { pitch } => {
                self.synthesizer.note_off(0, pitch as i32);
            }
            InstrumentEvent::ControlChange { controller, value } => {
                self.synthesizer
                    .process_midi_message(0, 0xB0, controller as i32, value as i32);
            }
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.synthesizer.render(left, right);
    }
}
