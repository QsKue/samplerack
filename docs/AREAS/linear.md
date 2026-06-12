# LinearResampler (`src/linear.rs`)

Source: `src/linear.rs` — `LinearResampler`. The cheapest real backend. Dependency-free, FFT-free.

Linear interpolation: each output frame is a linear blend of the two input frames bracketing its
fractional position (`a + (b - a) * frac`). ~1 frame of latency (`Latency::new(1, 0, 0)` — it needs
the `i+1` sample). **No anti-alias filtering**, so downsampling will alias.

## When to use

- Control-rate / modulation signals, small ratios, or where CPU clearly trumps fidelity.
- As a low-latency option when the ~`HALF_TAPS` latency of [`SincResampler`](sinc.md) is too much.

For audio-quality sample-rate conversion (especially downsampling), prefer `SincResampler`.

## How it works

`read_pos` advances by `1/ratio` input frames per output frame. `emit` reads frames `i` and `i+1` from
`InputHistory`, lerps by the fraction, and advances. `process` requires both bracketing frames buffered
(`i+1 < end`); `flush` (`allow_pad`) lets `i+1` zero-pad past end-of-stream to emit the final frame.
`InputHistory::trim` keeps the current frame and after.

## Gotchas

- **One frame held back during streaming.** `process` won't emit a frame until its `i+1` sample has
  arrived; `flush` releases the last frame (zero-padding `i+1`). Call `flush` until it returns `0`.
- **Aliasing on downsampling** is expected — this backend does not band-limit. Use sinc if that matters.
- **Fixed channel count.** Built for one `channels`; `process`/`flush` return empty/`0` on a mismatch.
- **`set_ratio`** just updates the step; no table or state to rebuild.

## Tests

Shares the `lib.rs` suite (the `output_length_tracks_ratio` case runs both linear and sinc).
