use std::collections::VecDeque;

use rubato::{
    Async, FixedAsync, Indexing, Resampler as _, SincInterpolationParameters,
    SincInterpolationType, WindowFunction, audioadapter_buffers::direct::InterleavedSlice,
};
use sinerack::Latency;

use crate::resampler::{ResampleResult, Resampler, sanitize_ratio};

// rubato sinc-interpolation tuning — rubato's "good quality" profile. `SINC_LEN`
// (filter length) is the audible knob: longer = sharper anti-alias, more CPU and
// latency. This is the *std*, high-fidelity backend; it is ~4x the filter length of
// the FFT-free [`SincResampler`](crate::SincResampler) (32 taps), which is the price
// of dropping the `rustfft`/`realfft` dependency chain.
const SINC_LEN: usize = 128;
const F_CUTOFF: f32 = 0.95;
const OVERSAMPLING_FACTOR: usize = 80;
/// Input frames consumed per rubato call (we run `FixedAsync::Input`, so the input
/// chunk is fixed and the output frame count varies — that variance is absorbed by
/// the `out_cache` surplus buffer).
const CHUNK_FRAMES: usize = 1024;
/// Upper bound on dynamic ratio changes relative to the initial ratio. A ratio change
/// rebuilds the resampler, so this is generous; it just sizes rubato's scratch.
const MAX_RELATIVE_RATIO: f64 = 8.0;

/// Windowed-sinc resampler backed by [`rubato`] — the **std, high-fidelity** backend.
///
/// This is the counterpart to the dependency-free [`SincResampler`](crate::SincResampler):
/// it wraps rubato's asynchronous sinc resampler (a 128-tap polyphase filter, std-only,
/// pulls `rustfft`) for the sharpest anti-aliasing, where the FFT-free backend trades
/// some quality for a `no_std`-candidate, zero-dependency build. Both implement the
/// same [`Resampler`] contract and are **content-time-aligned from input frame 0** (the
/// rubato group delay is trimmed internally), so they are drop-in interchangeable.
///
/// Gated behind the `rubato` cargo feature.
pub struct RubatoResampler {
    channels: usize,
    ratio: f64,
    /// The rubato resampler, lazily (re)built in [`Self::ensure_built`] — building needs
    /// the ratio, and a ratio change rebuilds. `None` until the first `process`/`flush`.
    rub: Option<Async<f32>>,
    /// The ratio `rub` was last built for, so a no-op `set_ratio` doesn't rebuild.
    built_ratio: f64,
    /// Pending input, interleaved. rubato needs whole fixed-size chunks, so input
    /// arriving in arbitrary slices is buffered here until a chunk's worth is available.
    in_fifo: VecDeque<f32>,
    /// Contiguous staging chunk fed to rubato (it needs a `&[f32]`, not a `VecDeque`).
    in_buf: Vec<f32>,
    out_buf: Vec<f32>,
    /// Output rubato produced that didn't fit the caller's slice yet. One rubato call
    /// emits a whole variable-size chunk that can overflow a small output slice; the
    /// surplus is parked here and drained first on the next call.
    out_cache: VecDeque<f32>,
    /// rubato's group delay in output frames: the resampler buffers ~half a kernel past
    /// input frame 0 before it can emit it, so its first `output_delay` output frames are
    /// warm-up silence. We discard them (see `lead_trim`) so the stream is content-aligned
    /// — matching the FFT-free backend, keeping a resampled deck aligned with a dry one.
    output_delay: usize,
    /// Countdown of leading warm-up frames still to discard. Re-armed to `output_delay`
    /// on build and on `reset` (a seek), so the trim re-applies for every fresh stream.
    lead_trim: usize,
    /// Real input frames fed / output frames delivered (post-trim) since the last build,
    /// used by `flush_tail` to know how much output is still owed at end of stream.
    in_total: u64,
    out_total: u64,
    /// Set once the EOF tail has been drained, so repeated `flush` calls don't re-run it.
    flushed: bool,
}

impl RubatoResampler {
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

    /// Builds a resampler with an explicit `ratio = output_rate / input_rate`. The
    /// underlying rubato filter is built lazily on the first `process`/`flush`.
    pub fn with_ratio(ratio: f64, channels: usize) -> Self {
        Self {
            channels,
            ratio: sanitize_ratio(ratio),
            rub: None,
            built_ratio: 0.0,
            in_fifo: VecDeque::new(),
            in_buf: Vec::new(),
            out_buf: Vec::new(),
            out_cache: VecDeque::new(),
            output_delay: 0,
            lead_trim: 0,
            in_total: 0,
            out_total: 0,
            flushed: false,
        }
    }

    /// (Re)builds the rubato filter for the current ratio when needed. A rebuild drops
    /// the per-configuration state (cached output, frame counters, flush flag) so nothing
    /// from the old ratio leaks into the new one.
    fn ensure_built(&mut self) {
        if self.rub.is_some() && (self.built_ratio - self.ratio).abs() < 1e-12 {
            return;
        }

        self.out_cache.clear();
        self.in_total = 0;
        self.out_total = 0;
        self.flushed = false;

        let params = SincInterpolationParameters {
            sinc_len: SINC_LEN,
            f_cutoff: F_CUTOFF,
            oversampling_factor: OVERSAMPLING_FACTOR,
            interpolation: SincInterpolationType::Linear,
            window: WindowFunction::BlackmanHarris2,
        };
        let r = Async::<f32>::new_sinc(
            self.ratio,
            MAX_RELATIVE_RATIO,
            &params,
            CHUNK_FRAMES,
            self.channels,
            FixedAsync::Input,
        )
        .expect("rubato parameters are valid");

        self.in_buf
            .resize(r.input_frames_max() * self.channels, 0.0);
        self.out_buf
            .resize(r.output_frames_max() * self.channels, 0.0);
        self.output_delay = r.output_delay();
        self.lead_trim = self.output_delay;
        self.built_ratio = self.ratio;
        self.rub = Some(r);
    }

    /// Runs rubato chunks (each consuming a fixed chunk from `in_fifo`, post-trim output
    /// pushed to `out_cache`) until the cache holds at least `target_samples` or there is
    /// no full input chunk left. Returns nothing; output is read out via `drain_cache`.
    fn produce_until(&mut self, target_samples: usize) {
        while self.out_cache.len() < target_samples {
            if !self.run_one_chunk() {
                break;
            }
        }
    }

    /// Runs a single rubato chunk if a full one is buffered. Returns `false` (no
    /// progress) when input is short of a chunk or rubato errors. Inlined field access
    /// (no `&mut self` helpers) keeps the `rub`/`in_buf`/`out_buf` borrows disjoint.
    fn run_one_chunk(&mut self) -> bool {
        let channels = self.channels;
        let needed = match self.rub.as_ref() {
            Some(r) => r.input_frames_next(),
            None => return false,
        };
        if self.in_fifo.len() / channels < needed {
            return false;
        }

        let in_samples = needed * channels;
        for (slot, &sample) in self.in_buf[..in_samples]
            .iter_mut()
            .zip(self.in_fifo.iter())
        {
            *slot = sample;
        }

        let produced;
        {
            let rub = self.rub.as_mut().unwrap();
            let out_frames = rub.output_frames_next();
            let out_samples = out_frames * channels;
            let input_adapter =
                InterleavedSlice::new(&self.in_buf[..in_samples], channels, needed).unwrap();
            let mut output_adapter =
                InterleavedSlice::new_mut(&mut self.out_buf[..out_samples], channels, out_frames)
                    .unwrap();
            match rub.process_into_buffer(&input_adapter, &mut output_adapter, None) {
                Ok((consumed, p)) => {
                    for _ in 0..consumed * channels {
                        self.in_fifo.pop_front();
                    }
                    self.in_total += consumed as u64;
                    produced = p;
                }
                Err(_) => return false,
            }
        }

        self.cache_produced(produced);
        true
    }

    /// Trims any pending leading warm-up frames from a freshly produced chunk in
    /// `out_buf[..produced * channels]` and pushes the rest to `out_cache`.
    fn cache_produced(&mut self, produced: usize) {
        self.cache_produced_capped(produced, u64::MAX);
    }

    /// As [`Self::cache_produced`], but delivers at most `cap_frames` frames. Used by
    /// the flush tail to stop exactly at the time-aligned output length, dropping the
    /// trailing zero-pad ringing rubato emits past it (so the total output length tracks
    /// the input × ratio tightly, matching the FFT-free backend).
    fn cache_produced_capped(&mut self, produced: usize, cap_frames: u64) {
        let trim = self.lead_trim.min(produced);
        self.lead_trim -= trim;
        let deliver = ((produced - trim) as u64).min(cap_frames) as usize;
        self.out_total += deliver as u64;
        let start = trim * self.channels;
        self.out_cache
            .extend(&self.out_buf[start..start + deliver * self.channels]);
    }

    /// Copies up to a whole-frame `output`-worth of cached output out, returning the
    /// number of *samples* written.
    fn drain_cache(&mut self, output: &mut [f32]) -> usize {
        let cap = (output.len() / self.channels) * self.channels;
        let n = cap.min(self.out_cache.len());
        for slot in output.iter_mut().take(n) {
            *slot = self.out_cache.pop_front().unwrap();
        }
        n
    }

    /// Drains the end-of-stream tail into `out_cache`: the final partial input chunk plus
    /// the `output_delay` frames still in rubato's delay line. Mirrors rubato's own
    /// `process_all` — feed the remainder with `partial_len`, then pump zeros until the
    /// time-aligned output length (`ratio * in_total`) is reached.
    fn flush_tail(&mut self) {
        let channels = self.channels;
        // Drain every FULL real chunk still buffered before the partial-tail pump. The
        // streaming driver leaves < 1 chunk here (it runs chunks in `process`), but a
        // caller that flushes with more buffered must not have the surplus dropped — the
        // trait contract promises flush drains the remaining tail, no "< 1 chunk"
        // precondition, and the FFT-free backends drain all buffered history.
        while self.run_one_chunk() {}

        let (needed, ratio) = match self.rub.as_ref() {
            Some(r) => (r.input_frames_next(), r.resample_ratio()),
            None => return,
        };

        let mut real_frames = (self.in_fifo.len() / channels).min(needed);
        self.in_total += real_frames as u64;
        // The leading trim exactly offsets the delay-line tail, so a full drain yields
        // `ratio * in_total` frames — no `+ delay` term.
        let expected = (ratio * self.in_total as f64).ceil() as u64;

        // Bound iterations so a degenerate ratio can't spin forever (one or two passes
        // is the norm, each emitting roughly a chunk's worth).
        for _ in 0..CHUNK_FRAMES {
            if self.out_total >= expected {
                break;
            }

            let chunk = needed * channels;
            let real = real_frames * channels;
            for (slot, &sample) in self.in_buf[..real].iter_mut().zip(self.in_fifo.iter()) {
                *slot = sample;
            }
            self.in_buf[real..chunk].fill(0.0);

            let produced;
            {
                let rub = self.rub.as_mut().unwrap();
                let out_frames = rub.output_frames_next();
                let out_samples = out_frames * channels;
                let input_adapter =
                    InterleavedSlice::new(&self.in_buf[..chunk], channels, needed).unwrap();
                let mut output_adapter = InterleavedSlice::new_mut(
                    &mut self.out_buf[..out_samples],
                    channels,
                    out_frames,
                )
                .unwrap();
                // `partial_len` tells rubato how many real input frames are present; it
                // zero-fills the rest. After the first pass we feed pure zeros.
                let indexing = Indexing {
                    input_offset: 0,
                    output_offset: 0,
                    partial_len: Some(real_frames),
                    active_channels_mask: None,
                };
                produced = match rub.process_into_buffer(
                    &input_adapter,
                    &mut output_adapter,
                    Some(&indexing),
                ) {
                    Ok((_consumed, p)) => p,
                    Err(_) => break,
                };
            }

            // Consume the real remainder from the queue exactly once.
            for _ in 0..real_frames * channels {
                self.in_fifo.pop_front();
            }
            real_frames = 0;

            // Cap delivery at the time-aligned length so the trailing zero-pad ringing
            // past `expected` is dropped rather than appended as a silent tail.
            let remaining = expected.saturating_sub(self.out_total);
            self.cache_produced_capped(produced, remaining);
            if produced == 0 {
                break;
            }
        }
    }
}

impl Resampler for RubatoResampler {
    fn process(&mut self, input: &[f32], output: &mut [f32], channels: usize) -> ResampleResult {
        if channels != self.channels || channels == 0 || output.len() < channels {
            return ResampleResult::default();
        }
        self.ensure_built();

        let input_frames = input.len() / channels;
        self.in_fifo.extend(&input[..input_frames * channels]);

        self.produce_until(output.len());
        let written = self.drain_cache(output);

        ResampleResult {
            input_frames_consumed: input_frames,
            output_frames_written: written / channels,
        }
    }

    fn flush(&mut self, output: &mut [f32], channels: usize) -> usize {
        if channels != self.channels || channels == 0 || output.len() < channels {
            return 0;
        }
        if self.rub.is_none() {
            return 0;
        }
        if !self.flushed {
            self.flush_tail();
            self.flushed = true;
        }
        self.drain_cache(output) / channels
    }

    fn reset(&mut self) {
        self.in_fifo.clear();
        self.out_cache.clear();
        self.in_total = 0;
        self.out_total = 0;
        self.flushed = false;
        self.lead_trim = self.output_delay;
        if let Some(rub) = self.rub.as_mut() {
            rub.reset();
        }
    }

    fn latency(&self) -> Latency {
        // The inherent group delay (trimmed internally for content alignment, reported
        // here so the engine can account for the stage's buffering).
        Latency::new(self.output_delay, 0, 0)
    }

    fn set_ratio(&mut self, ratio: f64) {
        let ratio = sanitize_ratio(ratio);
        if (self.ratio - ratio).abs() < 1e-12 {
            return;
        }
        self.ratio = ratio;
        // Change the ratio IN PLACE when the filter is already built, so the delay
        // line and buffered input survive. A mid-stream ratio change (e.g. WSOLA pitch
        // tracking calling this every block) must not rebuild a cold filter — that
        // would re-trim `output_delay` frames of real audio as if they were warm-up
        // silence and click. rubato permits an in-place change within
        // `MAX_RELATIVE_RATIO` of the build ratio; outside that range the call fails and
        // we leave `built_ratio` stale so the next `process` does a full rebuild (a rare,
        // large jump where a discontinuity is unavoidable anyway).
        if let Some(rub) = self.rub.as_mut()
            && rub.set_resample_ratio(ratio, true).is_ok()
        {
            self.built_ratio = ratio;
        }
        // When not yet built, the deferred build in `ensure_built` picks up `self.ratio`.
    }

    fn ratio(&self) -> f64 {
        self.ratio
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives a resampler to completion over `input`, returning interleaved output.
    fn run(r: &mut dyn Resampler, input: &[f32], channels: usize) -> Vec<f32> {
        let mut out = Vec::new();
        let mut buf = vec![0.0f32; 4096 * channels];
        let block = 512 * channels;
        let mut pos = 0;
        while pos < input.len() {
            let end = (pos + block).min(input.len());
            let res = r.process(&input[pos..end], &mut buf, channels);
            out.extend_from_slice(&buf[..res.output_frames_written * channels]);
            pos += res.input_frames_consumed.max(1) * channels;
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

    fn sine(freq: f32, rate: u32, frames: usize, channels: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * channels);
        for n in 0..frames {
            let s = (2.0 * std::f32::consts::PI * freq * n as f32 / rate as f32).sin();
            for _ in 0..channels {
                v.push(s);
            }
        }
        v
    }

    fn frames(v: &[f32], channels: usize) -> usize {
        v.len() / channels
    }

    /// Frequency estimate via zero-crossing count, at the given output rate.
    fn est_freq(v: &[f32], rate: u32, channels: usize) -> f32 {
        let n = v.len() / channels;
        let mut crossings = 0;
        for i in 1..n {
            let a = v[(i - 1) * channels];
            let b = v[i * channels];
            if (a <= 0.0 && b > 0.0) || (a >= 0.0 && b < 0.0) {
                crossings += 1;
            }
        }
        (crossings as f32 / 2.0) * rate as f32 / n as f32
    }

    #[test]
    fn constructor_rejects_bad_args() {
        assert!(RubatoResampler::new(48_000, 0, 1).is_err());
        assert!(RubatoResampler::new(0, 44_100, 1).is_err());
        assert!(RubatoResampler::new(48_000, 44_100, 0).is_err());
    }

    #[test]
    fn output_length_tracks_ratio() {
        // 48k -> 24k (ratio 0.5) should roughly halve the frame count.
        let input = sine(300.0, 48_000, 48_000, 1);
        let mut r = RubatoResampler::new(48_000, 24_000, 1).unwrap();
        let out = run(&mut r, &input, 1);
        let ratio = frames(&out, 1) as f32 / frames(&input, 1) as f32;
        assert!((ratio - 0.5).abs() < 0.02, "length ratio {ratio} ~ 0.5");
    }

    #[test]
    fn upsampling_preserves_frequency() {
        // 16k -> 48k (ratio 3.0): a 1 kHz tone stays 1 kHz, frame count triples.
        let in_rate = 16_000;
        let out_rate = 48_000;
        let input = sine(1000.0, in_rate, 16_000, 1);
        let mut r = RubatoResampler::new(in_rate, out_rate, 1).unwrap();
        let out = run(&mut r, &input, 1);
        let ratio = frames(&out, 1) as f32 / frames(&input, 1) as f32;
        assert!((ratio - 3.0).abs() < 0.05, "frame ratio {ratio} ~ 3.0");
        let f = est_freq(&out, out_rate, 1);
        assert!((f - 1000.0).abs() < 15.0, "freq {f} should stay ~1000 Hz");
    }

    #[test]
    fn downsampling_preserves_frequency() {
        // 48k -> 32k: a 500 Hz tone stays 500 Hz after anti-aliased downsampling.
        let in_rate = 48_000;
        let out_rate = 32_000;
        let input = sine(500.0, in_rate, 48_000, 1);
        let mut r = RubatoResampler::new(in_rate, out_rate, 1).unwrap();
        let out = run(&mut r, &input, 1);
        let f = est_freq(&out, out_rate, 1);
        assert!((f - 500.0).abs() < 10.0, "freq {f} should stay ~500 Hz");
    }

    #[test]
    fn leading_output_is_content_aligned() {
        // A DC input passes a lowpass unchanged. With the group delay trimmed, the very
        // first output frame sits at the signal level, not warm-up silence — matching the
        // FFT-free backend so a deck stays aligned regardless of the chosen backend.
        let input = vec![1.0_f32; 8000];
        let mut r = RubatoResampler::new(48_000, 44_100, 1).unwrap();
        let out = run(&mut r, &input, 1);
        assert!(
            out[0] > 0.9,
            "trimmed output must start at the signal level; got {}",
            out[0]
        );
    }

    #[test]
    fn stereo_channels_stay_independent() {
        // Left = 300 Hz, right = silence: resampling must not bleed L into R.
        let in_rate = 48_000;
        let frames_in = 24_000;
        let mut input = Vec::with_capacity(frames_in * 2);
        for n in 0..frames_in {
            let s = (2.0 * std::f32::consts::PI * 300.0 * n as f32 / in_rate as f32).sin();
            input.push(s);
            input.push(0.0);
        }
        let mut r = RubatoResampler::new(in_rate, 44_100, 2).unwrap();
        let out = run(&mut r, &input, 2);
        let right_energy: f32 = out.iter().skip(1).step_by(2).map(|s| s * s).sum();
        assert!(right_energy < 1e-3, "right channel should stay silent");
    }

    #[test]
    fn reset_equals_fresh() {
        let input = sine(440.0, 48_000, 20_000, 1);
        let mut reused = RubatoResampler::new(48_000, 44_100, 1).unwrap();
        let _ = run(&mut reused, &input, 1);
        reused.reset();
        let after = run(&mut reused, &input, 1);
        let mut fresh = RubatoResampler::new(48_000, 44_100, 1).unwrap();
        let from_fresh = run(&mut fresh, &input, 1);
        assert_eq!(after, from_fresh, "reset must behave like a fresh build");
    }

    #[test]
    fn output_is_finite_and_bounded() {
        let input = sine(220.0, 44_100, 22_050, 2);
        let mut r = RubatoResampler::new(44_100, 48_000, 2).unwrap();
        let out = run(&mut r, &input, 2);
        assert!(!out.is_empty());
        assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.5));
    }
}
