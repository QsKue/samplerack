use sinerack::Latency;

use crate::internals::InputHistory;
use crate::resampler::{ResampleResult, Resampler, sanitize_ratio};

/// Linear-interpolation resampler.
///
/// The cheapest real backend: each output frame is a linear blend of the two input
/// frames bracketing its fractional position. Dependency-free, FFT-free, ~1 frame
/// of latency. It has no anti-alias filtering, so downsampling will alias — fine
/// for small ratios, control-rate signals, or where CPU trumps fidelity; use
/// [`SincResampler`](crate::SincResampler) when quality matters.
pub struct LinearResampler {
    channels: usize,
    ratio: f64,
    /// Absolute per-channel input position of the next output frame.
    read_pos: f64,
    history: InputHistory,
}

impl LinearResampler {
    /// Builds a resampler converting `input_rate` → `output_rate` (ratio =
    /// `output_rate / input_rate`).
    pub fn new(input_rate: u32, output_rate: u32, channels: usize) -> Result<Self, String> {
        if channels == 0 {
            return Err("channel count must be greater than zero".to_string());
        }
        if input_rate == 0 || output_rate == 0 {
            return Err("sample rates must be greater than zero".to_string());
        }
        Ok(Self::with_ratio(
            output_rate as f64 / input_rate as f64,
            channels,
        ))
    }

    /// Builds a resampler with an explicit `ratio = output_rate / input_rate`.
    pub fn with_ratio(ratio: f64, channels: usize) -> Self {
        Self {
            channels,
            ratio: sanitize_ratio(ratio),
            read_pos: 0.0,
            history: InputHistory::new(channels.max(1)),
        }
    }

    /// Emits one output frame at the current `read_pos`, advancing it. Returns
    /// false if the bracketing input frames aren't available yet.
    fn emit(&mut self, output: &mut [f32], at: usize, allow_pad: bool) -> bool {
        let i = self.read_pos.floor() as i64;
        // Need frame `i`; with `allow_pad` (flush) `i+1` may zero-pad past the end.
        if i >= self.history.end() {
            return false;
        }
        if !allow_pad && i + 1 >= self.history.end() {
            return false;
        }
        let frac = (self.read_pos - i as f64) as f32;
        for ch in 0..self.channels {
            let a = self.history.at(i, ch);
            let b = self.history.at(i + 1, ch);
            output[at * self.channels + ch] = a + (b - a) * frac;
        }
        self.read_pos += 1.0 / self.ratio;
        true
    }

    fn run(&mut self, output: &mut [f32], allow_pad: bool) -> usize {
        let capacity = output.len() / self.channels;
        let mut written = 0;
        while written < capacity {
            if !self.emit(output, written, allow_pad) {
                break;
            }
            written += 1;
        }
        // Keep the current input frame and everything after it.
        self.history.trim(self.read_pos.floor() as i64);
        written
    }
}

impl Resampler for LinearResampler {
    fn process(&mut self, input: &[f32], output: &mut [f32], channels: usize) -> ResampleResult {
        if channels != self.channels || channels == 0 || output.len() < channels {
            return ResampleResult::default();
        }
        let input_frames = input.len() / channels;
        self.history.push(&input[..input_frames * channels]);
        let written = self.run(output, false);
        ResampleResult {
            input_frames_consumed: input_frames,
            output_frames_written: written,
        }
    }

    fn flush(&mut self, output: &mut [f32], channels: usize) -> usize {
        if channels != self.channels || channels == 0 || output.len() < channels {
            return 0;
        }
        self.run(output, true)
    }

    fn reset(&mut self) {
        self.read_pos = 0.0;
        self.history.clear();
    }

    fn latency(&self) -> Latency {
        // One input frame of lookahead (the `i+1` bracketing sample).
        Latency::new(1, 0, 0)
    }

    fn set_ratio(&mut self, ratio: f64) {
        self.ratio = sanitize_ratio(ratio);
    }

    fn ratio(&self) -> f64 {
        self.ratio
    }
}
