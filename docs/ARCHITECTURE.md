# Architecture

This document describes the structure of the `samplerack` crate. Keep it aligned with `AGENTS.md` and
update it when the `Resampler` contract, module boundaries, or data flow change.

## Shape

`samplerack` is a single library crate with a small flat module tree:

```text
lib.rs              crate root: module declarations, re-exports, cross-backend test suite
├── resampler.rs    the Resampler trait + value types + NoopResampler (the contract)
├── linear.rs       LinearResampler — linear interpolation (cheap, no anti-alias)
├── sinc.rs         SincResampler — FFT-free windowed-sinc polyphase (default, anti-aliased)
├── rubato_backend.rs  RubatoResampler — std 128-tap rubato wrapper (feature `rubato`)
└── internals.rs    InputHistory — shared streaming input buffer (absolute addressing + trim)
```

The contract (`resampler.rs`) is the public surface every consumer codes against; backends are
implementations of it. There is no engine, no session, no async, no I/O. The default dependency is
just **SineRack** (for `Latency`); the optional `rubato` feature pulls `rubato` + `audioadapter-buffers`
(and transitively `rustfft`) for the std high-fidelity backend. The tree is **flat** because there is
one family of backends today (interpolating resamplers); a `no_std`-style domain split (like
pitchrack's `time_domain` / `frequency_domain`) would only be introduced if a genuinely different
family lands (e.g. an FFT/sync resampler) — see `docs/ROADMAP.md`.

## Features

- **default** (`[]`) — FFT-free, dependency-free beyond SineRack: `Noop` + `Linear` + `Sinc`. The
  `no_std`-candidate build.
- **`rubato`** — adds `RubatoResampler`, wrapping `rubato`'s async sinc resampler (std, pulls
  `rustfft`). For builds where SRC fidelity outweighs a light dependency surface; interchangeable with
  `SincResampler` (same contract, content-aligned, length-matched).

## Public API

The public contract is intentionally small:

- `Resampler: Send` — the trait. A resampler operates on interleaved `f32`:
  ```rust
  fn process(&mut self, input: &[f32], output: &mut [f32], channels: usize) -> ResampleResult;
  fn flush(&mut self, output: &mut [f32], channels: usize) -> usize;
  fn reset(&mut self);
  fn latency(&self) -> sinerack::Latency;
  fn set_ratio(&mut self, ratio: f64);   // output_rate / input_rate
  fn ratio(&self) -> f64;
  ```
- `ResampleResult { input_frames_consumed, output_frames_written }` — what one `process` call did.
- `NoopResampler` — pass-through (valid at `ratio == 1.0`); the default and a test baseline.
- `LinearResampler` — linear interpolation; `new(in_rate, out_rate, channels)` or
  `with_ratio(ratio, channels)`.
- `SincResampler` — FFT-free windowed-sinc polyphase; same constructors. Anti-aliases downsampling by
  scaling the kernel cutoff to the output Nyquist.
- `RubatoResampler` *(feature `rubato`)* — std 128-tap `rubato`-backed sinc; same constructors. Buffers
  input into whole rubato chunks internally, trims rubato's group delay for content alignment, and caps
  its flush tail to the time-aligned length so it stays a drop-in for `SincResampler`.

Adding a backend should not change this trait: add a new module with a struct that implements
`Resampler` and re-export it from `lib.rs`.

## Data flow

A consumer drives a resampler per audio block:

```text
set_ratio(output_rate / input_rate)              // or build with new(in, out, ch)
loop over input blocks:
  process(input, output, channels)
    -> guard: channels matches build && channels != 0 && output big enough (else empty result)
    -> push input into InputHistory; emit output frames while the read window is buffered
    -> returns frames consumed (all of this block) + frames written (bounded by output capacity)
  (emit `output_frames_written`; repeat)
after the last input block:
  flush(output, channels) -> drains the filter lookahead against zero-padding; repeat until it returns 0
latency() -> sinerack::Latency, summed by the engine across pipeline stages
reset() between independent streams
```

`process` is **partial on both ends** — it may write fewer output frames than the buffer holds (when
the read window runs past the buffered input) and it buffers the whole input block internally, so it
reports `input_frames_consumed == input_frames`. The read position advances by `1 / ratio` input
frames per output frame; `InputHistory` retains the kernel's left taps and trims the rest. The sinc
backend rebuilds its polyphase table only when the cutoff (`min(1, ratio)`) changes.

## Key design properties

- **One uniform trait.** Every resampler implements `Resampler`; backends are added as modules.
- **Latency via SineRack.** `latency()` returns `sinerack::Latency` (sinc = `HALF_TAPS` input frames,
  linear = 1, noop = 0) so the engine can sum it.
- **FFT-free by default.** The default build has no `rustfft`; the crate stays a candidate for `no_std`.
  That is the whole point of distilling the sinc backend out of `rubato`. The std-only `rubato` backend
  is opt-in behind a feature, so it never burdens a light build.
- **Engine-agnostic.** No session/source/routing concepts. The engine owns scheduling and policy.

## Testing & checks

```bash
cargo build
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The `lib.rs` test suite covers both real backends: length-tracks-ratio, frequency preservation on
up- and down-sampling, anti-aliased downsampling, ratio-1 near-identity, stereo channel independence,
finite/bounded output, reset-equals-fresh, and `set_ratio` taking effect.

## Documentation coupling

When you change a module's responsibility, update the matching `docs/AREAS/*` file; when you change
the `Resampler` contract or a result type's semantics, update this document and add a
`docs/DECISIONS/` ADR if the choice is durable. See `AGENTS.md` for the full docs-maintenance policy.
