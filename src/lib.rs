//! `samplerack` — sample-rate conversion (resampling) for the q-lib audio engine.
//!
//! A leaf DSP crate in the q-lib audio split: it owns the [`Resampler`] contract
//! (change a signal's sample rate by a `ratio = output_rate / input_rate`) and its
//! implementations, built on the shared [`sinerack`] core. It is the counterpart to
//! [`phaserack`](https://github.com/QsKue/phaserack)'s time-stretcher — a stretcher
//! changes length while preserving pitch; a resampler changes both, which is also
//! the second half of time-domain pitch shifting (time-stretch then resample) and
//! the engine's sample-rate-conversion primitive (e.g. a 48 kHz line-in into a
//! 44.1 kHz pipeline).
//!
//! Backends report their delay as a [`sinerack::Latency`] so the engine can sum it
//! across the pipeline. The crate is dependency-free beyond `sinerack` and FFT-free
//! (no `rustfft`), so it stays light and is a candidate for a future `no_std` build.

mod internals;
mod linear;
mod resampler;
#[cfg(feature = "rubato")]
mod rubato_backend;
mod sinc;

pub use linear::LinearResampler;
pub use resampler::{NoopResampler, ResampleResult, Resampler};
#[cfg(feature = "rubato")]
pub use rubato_backend::RubatoResampler;
pub use sinc::SincResampler;

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives a resampler to completion over `input`, returning interleaved output.
    fn run(r: &mut dyn Resampler, input: &[f32], channels: usize) -> Vec<f32> {
        let mut out = Vec::new();
        let mut buf = vec![0.0f32; 1024 * channels];
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
    fn constructors_reject_bad_args() {
        assert!(LinearResampler::new(48_000, 44_100, 0).is_err());
        assert!(LinearResampler::new(0, 44_100, 1).is_err());
        assert!(SincResampler::new(48_000, 0, 1).is_err());
    }

    #[test]
    fn noop_passes_through() {
        let mut r = NoopResampler;
        let input = sine(440.0, 48_000, 4800, 1);
        let out = run(&mut r, &input, 1);
        assert_eq!(out, input);
    }

    #[test]
    fn output_length_tracks_ratio() {
        // 48k -> 24k (ratio 0.5) should roughly halve the frame count.
        for (ctor_sinc, name) in [(true, "sinc"), (false, "linear")] {
            let input = sine(300.0, 48_000, 48_000, 1);
            let mut r: Box<dyn Resampler> = if ctor_sinc {
                Box::new(SincResampler::new(48_000, 24_000, 1).unwrap())
            } else {
                Box::new(LinearResampler::new(48_000, 24_000, 1).unwrap())
            };
            let out = run(r.as_mut(), &input, 1);
            let ratio = frames(&out, 1) as f32 / frames(&input, 1) as f32;
            assert!(
                (ratio - 0.5).abs() < 0.02,
                "{name}: length ratio {ratio} should be ~0.5"
            );
        }
    }

    #[test]
    fn upsampling_preserves_frequency_sinc() {
        // 16k -> 48k (ratio 3.0): a 1 kHz tone stays 1 kHz, and frame count triples.
        let in_rate = 16_000;
        let out_rate = 48_000;
        let input = sine(1000.0, in_rate, 16_000, 1);
        let mut r = SincResampler::new(in_rate, out_rate, 1).unwrap();
        let out = run(&mut r, &input, 1);
        let ratio = frames(&out, 1) as f32 / frames(&input, 1) as f32;
        assert!((ratio - 3.0).abs() < 0.05, "frame ratio {ratio} ~ 3.0");
        let f = est_freq(&out, out_rate, 1);
        assert!((f - 1000.0).abs() < 15.0, "freq {f} should stay ~1000 Hz");
    }

    #[test]
    fn downsampling_preserves_frequency_sinc() {
        // 48k -> 32k: a 500 Hz tone stays 500 Hz after anti-aliased downsampling.
        let in_rate = 48_000;
        let out_rate = 32_000;
        let input = sine(500.0, in_rate, 48_000, 1);
        let mut r = SincResampler::new(in_rate, out_rate, 1).unwrap();
        let out = run(&mut r, &input, 1);
        let f = est_freq(&out, out_rate, 1);
        assert!((f - 500.0).abs() < 10.0, "freq {f} should stay ~500 Hz");
    }

    #[test]
    fn ratio_one_is_near_identity_sinc() {
        // ratio 1.0 should reproduce the input closely (a windowed-sinc at integer
        // positions is ~a unit impulse), bar the kernel's start/end transient.
        let input = sine(440.0, 48_000, 9600, 1);
        let mut r = SincResampler::new(48_000, 48_000, 1).unwrap();
        let out = run(&mut r, &input, 1);
        let n = frames(&out, 1).min(frames(&input, 1));
        // Compare the steady middle, skipping the filter's edge transients.
        let lo = 64;
        let hi = n.saturating_sub(64);
        let mut max_err = 0.0f32;
        for i in lo..hi {
            max_err = max_err.max((out[i] - input[i]).abs());
        }
        assert!(max_err < 0.02, "ratio-1 max error {max_err} should be tiny");
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
        let mut r = SincResampler::new(in_rate, 44_100, 2).unwrap();
        let out = run(&mut r, &input, 2);
        let right_energy: f32 = out.iter().skip(1).step_by(2).map(|s| s * s).sum();
        assert!(right_energy < 1e-3, "right channel should stay silent");
    }

    #[test]
    fn output_is_finite_and_bounded() {
        let input = sine(220.0, 44_100, 22_050, 2);
        let mut r = SincResampler::new(44_100, 48_000, 2).unwrap();
        let out = run(&mut r, &input, 2);
        assert!(!out.is_empty());
        assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.5));
    }

    #[test]
    fn reset_equals_fresh() {
        let input = sine(440.0, 48_000, 20_000, 1);
        let mut reused = SincResampler::new(48_000, 44_100, 1).unwrap();
        let _ = run(&mut reused, &input, 1);
        reused.reset();
        let after = run(&mut reused, &input, 1);
        let mut fresh = SincResampler::new(48_000, 44_100, 1).unwrap();
        let from_fresh = run(&mut fresh, &input, 1);
        assert_eq!(after, from_fresh, "reset must behave like a fresh build");
    }

    #[test]
    fn set_ratio_changes_output_rate() {
        // Reconfiguring the ratio mid-life takes effect (and rebuilds the sinc table).
        let mut r = SincResampler::with_ratio(1.0, 1);
        assert_eq!(r.ratio(), 1.0);
        r.set_ratio(0.5);
        assert_eq!(r.ratio(), 0.5);
        let input = sine(300.0, 48_000, 24_000, 1);
        let out = run(&mut r, &input, 1);
        let ratio = frames(&out, 1) as f32 / frames(&input, 1) as f32;
        assert!((ratio - 0.5).abs() < 0.03, "post set_ratio length ~0.5");
    }
}
