use sinerack::Latency;

use crate::internals::InputHistory;
use crate::resampler::{ResampleResult, Resampler, sanitize_ratio};

/// Taps per side of the windowed-sinc kernel; the full kernel is `2 * HALF_TAPS`.
const HALF_TAPS: usize = 16;
/// Number of precomputed sub-sample phases (polyphase branches). The fractional
/// read position is snapped to the nearest of these — 512 keeps the phase error
/// well below the kernel's own stopband.
const SUB_PHASES: usize = 512;

/// Windowed-sinc polyphase resampler — the high-quality backend.
///
/// Each output frame is a `2*HALF_TAPS`-tap windowed-sinc (bandlimited)
/// interpolation of the input at its fractional position, with the kernel snapped
/// to one of `SUB_PHASES` precomputed sub-sample phases (a distilled, allocation-
/// light take on rubato's sinc interpolator). When **downsampling** (`ratio < 1`)
/// the sinc cutoff is scaled to the output Nyquist so the result is anti-aliased;
/// when upsampling the cutoff is the input Nyquist. FFT-free and dependency-free.
pub struct SincResampler {
    channels: usize,
    ratio: f64,
    /// Absolute per-channel input position of the next output frame.
    read_pos: f64,
    history: InputHistory,

    /// Polyphase kernel table, `SUB_PHASES` rows of `2*HALF_TAPS` coefficients,
    /// each row normalized to unity DC gain. Rebuilt only when the cutoff changes.
    table: Vec<f32>,
    /// The cutoff (`min(1, ratio)`) the current `table` was built for.
    table_cutoff: f64,
}

impl SincResampler {
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
        let ratio = sanitize_ratio(ratio);
        let cutoff = ratio.min(1.0);
        Self {
            channels,
            ratio,
            read_pos: 0.0,
            history: InputHistory::new(channels.max(1)),
            table: build_table(cutoff),
            table_cutoff: cutoff,
        }
    }

    fn ensure_table(&mut self) {
        let cutoff = self.ratio.min(1.0);
        if (cutoff - self.table_cutoff).abs() > 1e-9 {
            self.table = build_table(cutoff);
            self.table_cutoff = cutoff;
        }
    }

    fn emit(&mut self, output: &mut [f32], at: usize, allow_pad: bool) -> bool {
        let i = self.read_pos.floor() as i64;
        if i >= self.history.end() {
            return false;
        }
        // Streaming needs the forward taps present; flush lets them zero-pad.
        if !allow_pad && i + HALF_TAPS as i64 >= self.history.end() {
            return false;
        }
        let frac = self.read_pos - i as f64;
        let sub = ((frac * SUB_PHASES as f64).round() as usize).min(SUB_PHASES - 1);
        let row = &self.table[sub * 2 * HALF_TAPS..(sub + 1) * 2 * HALF_TAPS];
        let base = i - HALF_TAPS as i64 + 1;
        for ch in 0..self.channels {
            let mut acc = 0.0f32;
            for (k, &coeff) in row.iter().enumerate() {
                acc += coeff * self.history.at(base + k as i64, ch);
            }
            output[at * self.channels + ch] = acc;
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
        // Retain the left taps of the next output frame.
        self.history
            .trim(self.read_pos.floor() as i64 - HALF_TAPS as i64 + 1);
        written
    }
}

impl Resampler for SincResampler {
    fn process(&mut self, input: &[f32], output: &mut [f32], channels: usize) -> ResampleResult {
        if channels != self.channels || channels == 0 || output.len() < channels {
            return ResampleResult::default();
        }
        self.ensure_table();
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
        // The kernel reaches `HALF_TAPS` frames ahead of the read position.
        Latency::new(HALF_TAPS, 0, 0)
    }

    fn set_ratio(&mut self, ratio: f64) {
        self.ratio = sanitize_ratio(ratio);
        // Table rebuild is deferred to the next `process` (see `ensure_table`).
    }

    fn ratio(&self) -> f64 {
        self.ratio
    }
}

/// Builds the polyphase kernel: for each sub-phase `f`, a `2*HALF_TAPS`-tap
/// Blackman-windowed sinc low-pass at the given `cutoff` (cycles/sample), each row
/// normalized to unity DC gain so the resampler preserves level.
fn build_table(cutoff: f64) -> Vec<f32> {
    let taps = 2 * HALF_TAPS;
    let mut table = vec![0.0f32; SUB_PHASES * taps];
    for sub in 0..SUB_PHASES {
        let f = sub as f64 / SUB_PHASES as f64;
        let row = &mut table[sub * taps..(sub + 1) * taps];
        let mut sum = 0.0f64;
        for (k, slot) in row.iter_mut().enumerate() {
            // Distance from the output position (i + f) to tap (i - HALF_TAPS + 1 + k).
            let d = f + (HALF_TAPS as f64 - 1.0) - k as f64;
            let w = blackman(k, taps);
            let c = w * sinc(cutoff * d);
            *slot = c as f32;
            sum += c;
        }
        if sum.abs() > f64::MIN_POSITIVE {
            let norm = (1.0 / sum) as f32;
            for slot in row.iter_mut() {
                *slot *= norm;
            }
        }
    }
    table
}

#[inline]
fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

#[inline]
fn blackman(k: usize, taps: usize) -> f64 {
    let n = k as f64;
    let m = (taps - 1) as f64;
    let a = std::f64::consts::TAU * n / m;
    0.42 - 0.5 * a.cos() + 0.08 * (2.0 * a).cos()
}
