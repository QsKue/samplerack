use sinerack::Latency;

use crate::internals::InputHistory;
use crate::resampler::{ResampleResult, Resampler, sanitize_ratio};

/// Taps per side of the windowed-sinc kernel at unity/upsampling (cutoff `1.0`); the
/// full kernel is `2 * HALF_TAPS`. On **downsampling** the half-tap count is scaled up
/// by `1 / cutoff` (see [`half_taps_for`]) so a constant number of sinc lobes stays
/// under the window — without it, a shrinking cutoff leaves only the main lobe inside a
/// fixed 32-tap span and the stopband collapses (aliasing on heavy downsample).
const HALF_TAPS: usize = 16;
/// Upper bound on the scaled half-tap count, so an extreme downsample ratio cannot blow
/// up the kernel table / per-frame cost. At the cap (16× downsample) quality is bounded,
/// not perfect — beyond audio-realistic SRC ratios.
const MAX_HALF_TAPS: usize = HALF_TAPS * 16;

/// Half-tap count for a given `cutoff` (= `min(1, ratio)`): `HALF_TAPS` at unity, scaled
/// by `1 / cutoff` on downsampling so the windowed kernel keeps ~`HALF_TAPS` lobes of the
/// (wider) sinc regardless of the downsample factor. Capped at [`MAX_HALF_TAPS`].
fn half_taps_for(cutoff: f64) -> usize {
    let scaled = (HALF_TAPS as f64 / cutoff.max(f64::MIN_POSITIVE)).ceil();
    (scaled as usize).clamp(HALF_TAPS, MAX_HALF_TAPS)
}
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
/// the sinc cutoff is lowered to the output Nyquist *and the kernel length is scaled up
/// by `1 / cutoff`* (see [`half_taps_for`]) so the window keeps ~`HALF_TAPS` lobes of the
/// wider sinc — that is what actually anti-aliases a heavy downsample; when upsampling the
/// cutoff is the input Nyquist and the kernel is the base length. FFT-free and
/// dependency-free.
pub struct SincResampler {
    channels: usize,
    ratio: f64,
    /// Absolute per-channel input position of the next output frame.
    read_pos: f64,
    history: InputHistory,

    /// Polyphase kernel table, `SUB_PHASES` rows of `2*half_taps` coefficients,
    /// each row normalized to unity DC gain. Rebuilt only when the cutoff changes.
    table: Vec<f32>,
    /// The cutoff (`min(1, ratio)`) the current `table` was built for.
    table_cutoff: f64,
    /// Half-tap count the current `table` was built for ([`half_taps_for`] of the
    /// cutoff) — scales up on downsampling, so every kernel-span calculation reads it
    /// rather than the `HALF_TAPS` constant.
    half_taps: usize,
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
        let half_taps = half_taps_for(cutoff);
        Self {
            channels,
            ratio,
            read_pos: 0.0,
            history: InputHistory::new(channels.max(1)),
            table: build_table(cutoff, half_taps),
            table_cutoff: cutoff,
            half_taps,
        }
    }

    fn ensure_table(&mut self) {
        let cutoff = self.ratio.min(1.0);
        if (cutoff - self.table_cutoff).abs() > 1e-9 {
            self.half_taps = half_taps_for(cutoff);
            self.table = build_table(cutoff, self.half_taps);
            self.table_cutoff = cutoff;
        }
    }

    fn emit(&mut self, output: &mut [f32], at: usize, allow_pad: bool) -> bool {
        let half_taps = self.half_taps;
        let i = self.read_pos.floor() as i64;
        if i >= self.history.end() {
            return false;
        }
        // Streaming needs the forward taps present; flush lets them zero-pad.
        if !allow_pad && i + half_taps as i64 >= self.history.end() {
            return false;
        }
        let frac = self.read_pos - i as f64;
        let sub = ((frac * SUB_PHASES as f64).round() as usize).min(SUB_PHASES - 1);
        let row = &self.table[sub * 2 * half_taps..(sub + 1) * 2 * half_taps];
        let base = i - half_taps as i64 + 1;
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
            .trim(self.read_pos.floor() as i64 - self.half_taps as i64 + 1);
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
        // The kernel reaches `half_taps` frames ahead of the read position (scales with
        // the downsample factor).
        Latency::new(self.half_taps, 0, 0)
    }

    fn set_ratio(&mut self, ratio: f64) {
        self.ratio = sanitize_ratio(ratio);
        // Table rebuild is deferred to the next `process` (see `ensure_table`).
    }

    fn ratio(&self) -> f64 {
        self.ratio
    }
}

/// Builds the polyphase kernel: for each sub-phase `f`, a `2*half_taps`-tap
/// Blackman-windowed sinc low-pass at the given `cutoff` (cycles/sample), each row
/// normalized to unity DC gain so the resampler preserves level. `half_taps` scales with
/// the downsample factor (see [`half_taps_for`]) so the window always spans ~`HALF_TAPS`
/// lobes of the cutoff-stretched sinc.
fn build_table(cutoff: f64, half_taps: usize) -> Vec<f32> {
    let taps = 2 * half_taps;
    let mut table = vec![0.0f32; SUB_PHASES * taps];
    for sub in 0..SUB_PHASES {
        let f = sub as f64 / SUB_PHASES as f64;
        let row = &mut table[sub * taps..(sub + 1) * taps];
        let mut sum = 0.0f64;
        for (k, slot) in row.iter_mut().enumerate() {
            // Distance from the output position (i + f) to tap (i - half_taps + 1 + k).
            let d = f + (half_taps as f64 - 1.0) - k as f64;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the resampler to completion over `input`, returning interleaved output.
    fn run(r: &mut SincResampler, input: &[f32], channels: usize) -> Vec<f32> {
        let mut out = Vec::new();
        let mut buf = vec![0.0f32; 2048 * channels];
        let block = 512 * channels;
        let mut pos = 0;
        while pos < input.len() {
            let end = (pos + block).min(input.len());
            let res = r.process(&input[pos..end], &mut buf, channels);
            out.extend_from_slice(&buf[..res.output_frames_written * channels]);
            pos = end;
        }
        loop {
            let n = r.flush(&mut buf, channels);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n * channels]);
        }
        out
    }

    fn rms(v: &[f32]) -> f32 {
        if v.is_empty() {
            return 0.0;
        }
        (v.iter().map(|s| s * s).sum::<f32>() / v.len() as f32).sqrt()
    }

    fn tone(freq: f32, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|n| (2.0 * std::f32::consts::PI * freq * n as f32 / rate as f32).sin())
            .collect()
    }

    /// On a heavy 6:1 downsample (48k -> 8k, output Nyquist 4 kHz), a tone above the
    /// output Nyquist must be strongly attenuated (anti-aliasing), while a passband tone
    /// passes at full level. Before the kernel-length scaling fix the fixed 32-tap window
    /// left the stopband only a few dB down and this aliased badly.
    #[test]
    fn heavy_downsample_attenuates_above_output_nyquist() {
        let in_rate = 48_000;
        let out_rate = 8_000; // ratio 1/6, output Nyquist 4 kHz
        let frames = 24_000;

        let mut pass_r = SincResampler::new(in_rate, out_rate, 1).unwrap();
        let pass = run(&mut pass_r, &tone(1_000.0, in_rate, frames), 1); // 1 kHz, passband

        let mut stop_r = SincResampler::new(in_rate, out_rate, 1).unwrap();
        let stop = run(&mut stop_r, &tone(6_000.0, in_rate, frames), 1); // 6 kHz, stopband

        let (p, s) = (rms(&pass), rms(&stop));
        assert!(p > 0.3, "passband tone should pass at full level, rms {p}");
        assert!(
            s / p < 0.05,
            "stopband tone must be >26 dB down (anti-aliased); ratio {}",
            s / p
        );
    }

    /// Passband level is preserved (unity DC/low-frequency gain) across a downsample.
    #[test]
    fn downsample_preserves_passband_level() {
        let mut r = SincResampler::new(48_000, 8_000, 1).unwrap();
        let out = run(&mut r, &tone(500.0, 48_000, 24_000), 1);
        // A 500 Hz sine has RMS ~0.707; the resampled passband tone should match closely.
        assert!((rms(&out) - 0.707).abs() < 0.05, "passband rms {}", rms(&out));
    }
}
