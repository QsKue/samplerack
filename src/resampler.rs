use sinerack::Latency;

/// How much input was consumed and output produced by a single `process` call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResampleResult {
    pub input_frames_consumed: usize,
    pub output_frames_written: usize,
}

/// A sample-rate converter operating on interleaved `f32` audio.
///
/// A resampler changes a signal's sample rate by a `ratio = output_rate /
/// input_rate`: `ratio > 1` upsamples (more output frames than input), `ratio < 1`
/// downsamples. It is the counterpart to [`phaserack`](https://github.com/QsKue/phaserack)'s
/// time-stretcher — where a stretcher changes length while preserving pitch, a
/// resampler changes both together (the "play it faster" transform), which is also
/// the second half of time-domain pitch shifting (time-stretch then resample).
///
/// The contract mirrors the time-stretcher's: interleaved `f32` in a separate
/// output buffer, **partial on both ends** (a call may consume/write fewer frames
/// than the buffers hold — honor the returned counts), a `flush` tail, and latency
/// reported as a [`sinerack::Latency`] so the engine can sum it across stages.
pub trait Resampler: Send {
    /// Resamples interleaved input into a separate interleaved output buffer.
    /// Implementations may consume/write partial frame counts.
    fn process(&mut self, input: &[f32], output: &mut [f32], channels: usize) -> ResampleResult;

    /// Writes remaining tail frames after the final input block (the resampler's
    /// filter lookahead drained against zero-padding). Call until it returns `0`.
    fn flush(&mut self, output: &mut [f32], channels: usize) -> usize;

    fn reset(&mut self);

    fn latency(&self) -> Latency;

    /// Sets the conversion ratio (`output_rate / input_rate`). Implementations
    /// sanitize it to a finite, positive value.
    fn set_ratio(&mut self, ratio: f64);

    fn ratio(&self) -> f64;
}

/// Clamps a requested ratio to a finite, strictly positive value (falling back to
/// `1.0` for non-finite input). Shared by the real backends (only compiled when one is
/// enabled; the trait-only / Noop default does not use it).
#[cfg(any(feature = "linear", feature = "sinc", feature = "rubato"))]
pub(crate) fn sanitize_ratio(ratio: f64) -> f64 {
    if ratio.is_finite() && ratio > 0.0 {
        ratio.max(f64::MIN_POSITIVE)
    } else {
        1.0
    }
}

/// A pass-through resampler: copies input to output unchanged. Valid only at
/// `ratio == 1.0` (it ignores any other ratio and still passes through). Useful as
/// a default and in tests.
pub struct NoopResampler;

impl Resampler for NoopResampler {
    fn process(&mut self, input: &[f32], output: &mut [f32], channels: usize) -> ResampleResult {
        if channels == 0 {
            return ResampleResult::default();
        }
        let frames = (input.len() / channels).min(output.len() / channels);
        let samples = frames * channels;
        output[..samples].copy_from_slice(&input[..samples]);
        ResampleResult {
            input_frames_consumed: frames,
            output_frames_written: frames,
        }
    }

    fn flush(&mut self, _output: &mut [f32], _channels: usize) -> usize {
        0
    }

    fn reset(&mut self) {}

    fn latency(&self) -> Latency {
        Latency::default()
    }

    fn set_ratio(&mut self, _ratio: f64) {}

    fn ratio(&self) -> f64 {
        1.0
    }
}
