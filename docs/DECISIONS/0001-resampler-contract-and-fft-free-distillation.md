# Decision: own the `Resampler` contract; distill an FFT-free resampler out of rubato

## Status

Accepted (2026-06-12) — the crate's founding decision.

## Context

The q-lib audio engine needs **sample-rate conversion** in two places: the engine's own line-in /
device-rate → pipeline-rate conversion (which `mixrack` did via `rubato`), and the resample half of
time-domain **pitch shifting** in `phaserack` (pitch shift = time-stretch then resample). `rubato`
was the only external DSP the engine leaned on that we had not distilled. It is **`std`**, and its
default `fft_resampler` pulls `realfft → rustfft`, so it is a second `rustfft` surface — at odds with
the wider goal of a light, eventually `no_std` audio core. Resampling is also conceptually "the other
half of pitch shifting," so it belongs in the `*rack` leaf family alongside `phaserack`.

## Decision

- **A new leaf crate, `samplerack`,** owns the `Resampler` contract (`process`/`flush`/`reset`/
  `latency`/`set_ratio`/`ratio` on interleaved `f32`, ratio = `output_rate / input_rate`) and its
  implementations. It mirrors `phaserack`'s `TimeStretcher` shape: separate-buffer interleaved I/O,
  partial consume/produce via `ResampleResult`, a `flush` tail, latency as `sinerack::Latency`. Only
  dependency is `sinerack`.
- **Distill, don't wrap.** Implement our own resamplers rather than wrap `rubato`:
  - `NoopResampler` — pass-through baseline.
  - `LinearResampler` — linear interpolation; cheap, ~1 frame latency, no anti-alias.
  - `SincResampler` — windowed-sinc polyphase (16 taps/side, 512 sub-phases, Blackman window), the
    distilled equivalent of rubato's sinc interpolator. Downsampling scales the kernel cutoff to the
    output Nyquist, so it is anti-aliased. This is the high-quality default.
- **FFT-free, on purpose.** No `rustfft` / FFT dependency. The whole point of distilling is to shed
  that surface; a future FFT/sync backend (rubato's other engine) would be a separate, feature-gated
  module so the default crate stays light and `no_std`-able.
- **Flat module tree.** One backend family (interpolating) today → flat modules, no domain namespace.
  Introduce a namespace (à la pitchrack) only if a genuinely different family lands.

## Consequences

- `mixrack` can replace `rubato` for SRC with a crate we own, removing `rubato` + its `rustfft` from
  the engine; `phaserack`'s generic stretch-then-resample pitch shift (WSOLA increment 2) gets a
  resampler. PSOLA / parametric pitch shifting need **no** resampler, so this is *not* on the autotune
  critical path — a parallel track.
- New backends fit the existing `Resampler` trait and are added as modules; the trait stays as-is.
- `no_std` is a small follow-up (`#![no_std]` + `alloc`, gated behind a `std` feature) but is blocked
  on `sinerack` gaining `no_std` first, since `Latency` lives there.
- We own the resampler quality/latency trade-offs and can tune the kernel; we also own the maintenance
  that `rubato` previously carried.
