# Architecture

This document describes the structure of the `samplerack` crate. Keep it aligned with `AGENTS.md` and
update it when the `Resampler` contract, module boundaries, or data flow change.

## Shape

`samplerack` is a single library crate with a small flat module tree:

```text
lib.rs              crate root: module declarations, re-exports, cross-backend test suite
├── resampler.rs    the Resampler trait + value types + NoopResampler (the contract; always built)
├── linear.rs       LinearResampler — linear interpolation (cheap, no anti-alias)      [feature linear]
├── sinc.rs         SincResampler — FFT-free windowed-sinc polyphase (anti-aliased)     [feature sinc]
├── rubato_backend.rs  RubatoResampler — std 128-tap rubato wrapper                     [feature rubato]
└── internals.rs    InputHistory — shared streaming input buffer        [cfg(any(linear, sinc))]
```

The contract (`resampler.rs`) is the public surface every consumer codes against; backends are
implementations of it, **each behind its own feature** (the default builds only the trait + `Noop`).
There is no engine, no session, no async, no I/O. The default dependency is just **SineRack** (for
`Latency`); the `rubato` feature additionally pulls `rubato` + `audioadapter-buffers` (and transitively
`rustfft`). The tree is **flat** because there is one family of backends today (interpolating
resamplers); a `no_std`-style domain split (like pitchrack's `time_domain` / `frequency_domain`) would
only be introduced if a genuinely different family lands (e.g. an FFT/sync resampler) — see
`docs/ROADMAP.md`.

## Features (per-backend opt-in — ADR 0003)

- **default** (`[]`) — the `Resampler` trait + `NoopResampler` only. No backend, no `sanitize_ratio`,
  no `InputHistory`; dependency-free beyond SineRack. "Need no conversion → pull no backend."
- **`linear`** — `LinearResampler` (FFT-free, dependency-free).
- **`sinc`** — `SincResampler` (FFT-free, dependency-free). The `no_std`-candidate high-quality backend.
- **`rubato`** — `RubatoResampler`, wrapping `rubato`'s async sinc resampler (std, pulls `rustfft`).
  For builds where SRC fidelity outweighs a light dependency surface; interchangeable with
  `SincResampler` (same contract, content-aligned, length-matched).

`linear`/`sinc` share `internals::InputHistory` and are both FFT-free, so the default and any FFT-free
subset stay `rustfft`-free.

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
- `NoopResampler` — pass-through (valid at `ratio == 1.0`); always available, the default and a test
  baseline.
- `LinearResampler` *(feature `linear`)* — linear interpolation; `new(in_rate, out_rate, channels)` or
  `with_ratio(ratio, channels)`.
- `SincResampler` *(feature `sinc`)* — FFT-free windowed-sinc polyphase; same constructors. Anti-aliases
  downsampling by scaling the kernel cutoff to the output Nyquist.
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

- **One uniform trait.** Every resampler implements `Resampler`; backends are added as feature-gated
  modules.
- **Per-backend opt-in.** The default builds only the trait + `Noop`; each real backend is its own
  feature (ADR 0003), so a consumer compiles only the algorithm(s) it wants — mirroring pitchrack's
  detectors.
- **Latency via SineRack.** `latency()` returns `sinerack::Latency` (sinc = `HALF_TAPS` input frames,
  linear = 1, noop = 0) so the engine can sum it.
- **FFT-free by default.** No backend (or any `linear`/`sinc` subset) pulls `rustfft`; the crate stays a
  candidate for `no_std`. That is the whole point of distilling the sinc backend out of `rubato`. The
  std-only `rubato` backend is opt-in behind a feature, so it never burdens a light build.
- **Engine-agnostic.** No session/source/routing concepts. The engine owns scheduling and policy.

## Testing & checks

```bash
cargo fmt --all --check
cargo clippy --no-default-features --all-targets -- -D warnings   # trait + Noop only
cargo clippy --all-features --all-targets -- -D warnings          # every backend
cargo build --no-default-features                                 # trait-only compiles
cargo test --all-features                                         # run the full suite
```

Tests live with their backend: each backend module has its own `#[cfg(test)]` tests, and the `lib.rs`
cross-backend suite (gated `all(feature = "linear", feature = "sinc")`) covers length-tracks-ratio,
frequency preservation up/down, anti-aliased downsampling, ratio-1 near-identity, stereo independence,
finite/bounded output, reset-equals-fresh, and `set_ratio`. Run with `--all-features`; the default build
has no backend to test.

## Documentation coupling

When you change a module's responsibility, update the matching `docs/AREAS/*` file; when you change
the `Resampler` contract or a result type's semantics, update this document and add a
`docs/DECISIONS/` ADR if the choice is durable. See `AGENTS.md` for the full docs-maintenance policy.
