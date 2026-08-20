use super::AudioBuffer;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc::sync_channel,
};
use std::thread;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("no default audio output device")]
    NoOutputDevice,
    #[error("could not query output configuration: {0}")]
    Config(#[from] cpal::DefaultStreamConfigError),
    #[error("could not create output stream: {0}")]
    Build(#[from] cpal::BuildStreamError),
    #[error("could not start output stream: {0}")]
    Play(#[from] cpal::PlayStreamError),
    #[error("unsupported output sample format: {0:?}")]
    UnsupportedFormat(cpal::SampleFormat),
    #[error("audio playback thread stopped before it was ready")]
    ThreadStopped,
}

/// A small, Send + Sync playback control.  The platform-specific CPAL stream
/// stays on its own thread because some CPAL backends deliberately do not
/// allow the stream handle to cross threads.
pub struct PlaybackHandle {
    stop_requested: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
}

impl PlaybackHandle {
    pub fn stop(&self) {
        self.stop_requested.store(true, Ordering::Release);
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
}

impl Drop for PlaybackHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn play_buffer(buffer: AudioBuffer) -> Result<PlaybackHandle, PlayerError> {
    let stop_requested = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let thread_stop = stop_requested.clone();
    let thread_finished = finished.clone();
    let (ready_sender, ready_receiver) = sync_channel(1);

    thread::spawn(move || {
        let result = run_stream(buffer, thread_stop.clone(), thread_finished.clone());
        match result {
            Ok(stream) => {
                let _ = ready_sender.send(Ok(()));
                while !thread_stop.load(Ordering::Acquire)
                    && !thread_finished.load(Ordering::Acquire)
                {
                    thread::sleep(Duration::from_millis(10));
                }
                drop(stream);
                thread_finished.store(true, Ordering::Release);
            }
            Err(error) => {
                thread_finished.store(true, Ordering::Release);
                let _ = ready_sender.send(Err(error));
            }
        }
    });

    match ready_receiver.recv() {
        Ok(Ok(())) => Ok(PlaybackHandle {
            stop_requested,
            finished,
        }),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(PlayerError::ThreadStopped),
    }
}

fn run_stream(
    buffer: AudioBuffer,
    stop_requested: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
) -> Result<cpal::Stream, PlayerError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or(PlayerError::NoOutputDevice)?;
    let supported = device.default_output_config()?;
    let sample_format = supported.sample_format();
    let config = supported.config();
    let channels = config.channels as usize;
    // Rendering and the output device are allowed to use different sample
    // rates.  Prepare one stereo buffer before the realtime callback starts;
    // the callback itself remains allocation-free and never performs I/O.
    let samples = Arc::new(resample_to_stereo(&buffer, config.sample_rate.0));
    let position = Arc::new(AtomicUsize::new(0));

    let stream = match sample_format {
        cpal::SampleFormat::F32 => build_stream_f32(
            &device,
            &config,
            channels,
            samples,
            position,
            stop_requested,
            finished,
        )?,
        cpal::SampleFormat::I16 => build_stream_i16(
            &device,
            &config,
            channels,
            samples,
            position,
            stop_requested,
            finished,
        )?,
        cpal::SampleFormat::U16 => build_stream_u16(
            &device,
            &config,
            channels,
            samples,
            position,
            stop_requested,
            finished,
        )?,
        other => return Err(PlayerError::UnsupportedFormat(other)),
    };
    stream.play()?;
    Ok(stream)
}

fn build_stream_f32(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    samples: Arc<Vec<f32>>,
    position: Arc<AtomicUsize>,
    stop_requested: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    device.build_output_stream(
        config,
        move |output: &mut [f32], _| {
            fill_output(
                output,
                channels,
                &samples,
                &position,
                &stop_requested,
                &finished,
                |sample| sample,
            )
        },
        stream_error,
        None,
    )
}

fn build_stream_i16(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    samples: Arc<Vec<f32>>,
    position: Arc<AtomicUsize>,
    stop_requested: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    device.build_output_stream(
        config,
        move |output: &mut [i16], _| {
            fill_output(
                output,
                channels,
                &samples,
                &position,
                &stop_requested,
                &finished,
                |sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16,
            )
        },
        stream_error,
        None,
    )
}

fn build_stream_u16(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    samples: Arc<Vec<f32>>,
    position: Arc<AtomicUsize>,
    stop_requested: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    device.build_output_stream(
        config,
        move |output: &mut [u16], _| {
            fill_output(
                output,
                channels,
                &samples,
                &position,
                &stop_requested,
                &finished,
                |sample| (((sample.clamp(-1.0, 1.0) + 1.0) * 0.5) * u16::MAX as f32) as u16,
            )
        },
        stream_error,
        None,
    )
}

fn fill_output<T, F>(
    output: &mut [T],
    output_channels: usize,
    samples: &[f32],
    position: &AtomicUsize,
    stop_requested: &AtomicBool,
    finished: &AtomicBool,
    convert: F,
) where
    T: Copy,
    F: Fn(f32) -> T,
{
    if output_channels == 0 {
        return;
    }
    let source_channels = 2;
    let mut cursor = position.load(Ordering::Relaxed);
    let stopped = stop_requested.load(Ordering::Acquire);
    for (index, value) in output.iter_mut().enumerate() {
        if stopped || cursor >= samples.len() {
            *value = convert(0.0);
            continue;
        }
        let source_frame = cursor / source_channels;
        let output_channel = index % output_channels;
        let source_channel = output_channel.min(source_channels - 1);
        let source_index = source_frame * source_channels + source_channel;
        *value = convert(samples.get(source_index).copied().unwrap_or(0.0));
        if output_channel == output_channels - 1 {
            cursor += source_channels;
        }
    }
    position.store(cursor, Ordering::Relaxed);
    if stopped || cursor >= samples.len() {
        finished.store(true, Ordering::Release);
    }
}

fn stream_error(error: cpal::StreamError) {
    eprintln!("audio stream error: {error}");
}

fn resample_to_stereo(buffer: &AudioBuffer, output_sample_rate: u32) -> Vec<f32> {
    let input_frames = buffer.frames();
    if input_frames == 0
        || buffer.channels == 0
        || buffer.sample_rate == 0
        || output_sample_rate == 0
    {
        return Vec::new();
    }

    let output_frames = ((input_frames as u64 * output_sample_rate as u64
        + buffer.sample_rate as u64 / 2)
        / buffer.sample_rate as u64)
        .max(1) as usize;
    let mut output = Vec::with_capacity(output_frames * 2);
    let step = buffer.sample_rate as f64 / output_sample_rate as f64;

    for output_frame in 0..output_frames {
        let source_position = output_frame as f64 * step;
        let first_frame = (source_position.floor() as usize).min(input_frames - 1);
        let second_frame = (first_frame + 1).min(input_frames - 1);
        let fraction = (source_position - first_frame as f64) as f32;
        for channel in 0..2 {
            let first = source_sample(buffer, first_frame, channel);
            let second = source_sample(buffer, second_frame, channel);
            output.push(first + (second - first) * fraction);
        }
    }
    output
}

fn source_sample(buffer: &AudioBuffer, frame: usize, channel: usize) -> f32 {
    let source_channel = if buffer.channels == 1 {
        0
    } else {
        channel.min(buffer.channels - 1)
    };
    buffer.samples[frame * buffer.channels + source_channel]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_resamples_without_changing_duration() {
        let buffer = AudioBuffer {
            sample_rate: 2,
            channels: 2,
            samples: vec![0.0, 0.0, 1.0, -1.0],
        };
        let samples = resample_to_stereo(&buffer, 4);
        assert_eq!(samples.len(), 8);
        assert_eq!(samples, vec![0.0, 0.0, 0.5, -0.5, 1.0, -1.0, 1.0, -1.0]);
    }

    #[test]
    fn playback_duplicates_mono_to_stereo() {
        let buffer = AudioBuffer {
            sample_rate: 48_000,
            channels: 1,
            samples: vec![0.25, -0.5],
        };
        assert_eq!(
            resample_to_stereo(&buffer, 48_000),
            vec![0.25, 0.25, -0.5, -0.5]
        );
    }
}
