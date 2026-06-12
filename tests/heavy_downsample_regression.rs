//! Regression: a heavy downsample (read step > buffered frames) must not panic in
//! `InputHistory::trim`. Before the upper-bound clamp, the FFT-free backends drained
//! past the buffer end and panicked on ordinary ratios (linear < 1/2, sinc < 1/16).

#[cfg(feature = "linear")]
#[test]
fn linear_heavy_downsample_is_length_aligned_no_panic() {
    use samplerack::{LinearResampler, Resampler};
    // ratio 1/3 (step 3.0) — overshoots the buffer end on the last emit.
    let mut r = LinearResampler::new(48_000, 16_000, 1).unwrap();
    let input: Vec<f32> = (0..500).map(|i| (i as f32 * 0.01).sin()).collect();
    let mut out = vec![0.0; 4096];
    let res = r.process(&input, &mut out, 1);
    assert_eq!(res.input_frames_consumed, 500);
    // ~500 * 1/3 output frames, off-by-a-frame at the boundary is fine.
    assert!((res.output_frames_written as i64 - 167).abs() <= 1);
}

#[cfg(feature = "sinc")]
#[test]
fn sinc_heavy_downsample_is_length_aligned_no_panic() {
    use samplerack::{Resampler, SincResampler};
    // ratio 1/24 (step 24) — overshoots even past the sinc half-tap margin.
    let mut r = SincResampler::new(48_000, 2_000, 1).unwrap();
    let input: Vec<f32> = (0..2000).map(|i| (i as f32 * 0.01).sin()).collect();
    let mut out = vec![0.0; 4096];
    let res = r.process(&input, &mut out, 1);
    let mut tail = vec![0.0; 4096];
    let flushed = r.flush(&mut tail, 1);
    assert_eq!(res.input_frames_consumed, 2000);
    // ~2000 * 1/24 ≈ 83 total output frames across process + flush.
    let total = res.output_frames_written + flushed;
    assert!((total as i64 - 83).abs() <= 2, "got {total}");
}
