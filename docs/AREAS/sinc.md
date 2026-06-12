# SincResampler (`src/sinc.rs`)

Source: `src/sinc.rs` — `SincResampler`. The dependency-free, FFT-free default backend — an
in-house distillation of `rubato`'s sinc interpolator. For the std, 128-tap `rubato` backend
proper see [`rubato_backend.md`](rubato_backend.md).

A **windowed-sinc polyphase** resampler: each output frame is a `2 * HALF_TAPS`-tap
Blackman-windowed-sinc (bandlimited) interpolation of the input at its fractional position. The
fractional position is snapped to the nearest of `SUB_PHASES` precomputed sub-sample phases
(polyphase branches), so the hot path is a fixed dot product with no per-sample transcendentals.

## Constants

- `HALF_TAPS = 16` → 32-tap kernel. Latency is `HALF_TAPS` input frames (the kernel reaches that far
  ahead of the read position), reported as `Latency::new(HALF_TAPS, 0, 0)`.
- `SUB_PHASES = 512` → sub-sample phase resolution; the residual phase-snapping error sits well below
  the kernel's own stopband. Table size is `SUB_PHASES * 2 * HALF_TAPS` `f32` (~64 KB).

## How it works

- **Table (`build_table`).** For each sub-phase `f ∈ [0,1)`, a row of `2*HALF_TAPS` coefficients:
  `coeff[k] = blackman(k) * sinc(cutoff * d_k)` where `d_k = f + (HALF_TAPS-1) - k` is the distance
  from the output position to tap `k`. Each row is **normalized to unity DC gain** so the resampler
  preserves level.
- **Anti-aliasing.** `cutoff = min(1, ratio)`. Upsampling uses the full input Nyquist (`cutoff = 1`);
  downsampling lowers the cutoff to the **output** Nyquist so the input is band-limited before
  decimation. The table is rebuilt only when the cutoff changes (`ensure_table`, deferred to the next
  `process` after a `set_ratio`).
- **Streaming.** `read_pos` advances by `1/ratio` input frames per output frame. `emit` snaps the
  fraction to a sub-phase and dots the row against `InputHistory` samples `[i-HALF_TAPS+1 ..
  i+HALF_TAPS]`. `process` requires the forward taps to be buffered (`i+HALF_TAPS < end`); `flush`
  lets them zero-pad past end-of-stream. `InputHistory::trim` retains the next frame's left taps.

## Gotchas

- **Start/end transients.** The first/last ~`HALF_TAPS` output frames see zero-padded taps (the ramp
  in/out of the stream). The `ratio_one_is_near_identity` test compares the steady middle for this
  reason.
- **`flush` until 0.** It drains the lookahead against zero-padding; skipping it drops the tail.
- **`set_ratio` rebuilds the table** on the next `process` if the cutoff moved — a one-off cost, not
  per-hop. Holding ratio constant is free.
- **Fixed channel count.** Built for one `channels`; `process`/`flush` return empty/`0` on a mismatch.
- **Per-channel independent.** The same kernel/phase is applied to each channel separately — no
  cross-channel bleed (asserted by `stereo_channels_stay_independent`).

## Tests

Covered by the `lib.rs` suite: length-tracks-ratio, frequency preservation up (16k→48k) and down
(48k→32k, anti-aliased), ratio-1 near-identity, stereo independence, finite/bounded output,
reset-equals-fresh, and `set_ratio` taking effect.
