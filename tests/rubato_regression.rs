#![cfg(feature = "rubato")]

//! Regressions for the rubato backend audit findings:
//! A1/B1/C2 — a mid-stream `set_ratio` must change the ratio in place, not rebuild a
//!            cold filter that re-trims real audio (dropped frames + clicks).
//! C3       — `flush` must drain ALL buffered input, not just one chunk.

use samplerack::{Resampler, RubatoResampler};

fn sine(freq: f32, rate: u32, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|n| (2.0 * std::f32::consts::PI * freq * n as f32 / rate as f32).sin())
        .collect()
}

/// Process `input` in 512-frame blocks; if `nudge_at` is Some, call `set_ratio` once at
/// that block boundary. Returns total output frames (process + full flush).
fn run_blocks(r: &mut RubatoResampler, input: &[f32], nudge_at: Option<(usize, f64)>) -> usize {
    let mut buf = vec![0.0f32; 4096];
    let block = 512;
    let mut total = 0;
    let mut pos = 0;
    let mut bi = 0;
    while pos < input.len() {
        if let Some((at, ratio)) = nudge_at
            && bi == at
        {
            r.set_ratio(ratio);
        }
        let end = (pos + block).min(input.len());
        let res = r.process(&input[pos..end], &mut buf, 1);
        total += res.output_frames_written;
        pos = end;
        bi += 1;
    }
    loop {
        let n = r.flush(&mut buf, 1);
        if n == 0 {
            break;
        }
        total += n;
    }
    total
}

#[test]
fn midstream_ratio_change_does_not_drop_frames() {
    // Build at 1:1. A control run vs a run with a negligible mid-stream ratio nudge must
    // produce the same number of frames. Before the in-place fix the nudge rebuilt a cold
    // filter and re-trimmed ~output_delay (~64) frames of real audio.
    let input = sine(440.0, 48_000, 20_000);

    let mut control = RubatoResampler::new(48_000, 48_000, 1).unwrap();
    let control_len = run_blocks(&mut control, &input, None);

    let mut nudged = RubatoResampler::new(48_000, 48_000, 1).unwrap();
    let nudged_len = run_blocks(&mut nudged, &input, Some((20, 1.0 + 1e-9)));

    let diff = (control_len as i64 - nudged_len as i64).abs();
    assert!(
        diff <= 2,
        "mid-stream ratio change dropped {diff} frames (control {control_len}, nudged {nudged_len})"
    );
}

#[test]
fn flush_drains_all_buffered_input() {
    // One process of a long input into a tiny output slice buffers most of it internally;
    // flush must then emit the rest. Before the fix flush_tail fed only one chunk and
    // dropped the remainder.
    let input = sine(300.0, 48_000, 20_000); // 48k -> 24k, ratio 0.5
    let mut r = RubatoResampler::new(48_000, 24_000, 1).unwrap();

    let mut small = vec![0.0f32; 256];
    let res = r.process(&input, &mut small, 1);
    let mut total = res.output_frames_written;
    let mut buf = vec![0.0f32; 4096];
    loop {
        let n = r.flush(&mut buf, 1);
        if n == 0 {
            break;
        }
        total += n;
    }
    // ~20000 * 0.5 = 10000 expected.
    assert!(
        (total as i64 - 10_000).abs() <= 64,
        "expected ~10000 output frames after full flush, got {total}"
    );
}
