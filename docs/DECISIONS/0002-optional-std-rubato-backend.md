# Decision: offer an optional std `rubato` backend alongside the FFT-free distillation

## Status

Accepted (2026-06-12). Amends [ADR 0001](0001-resampler-contract-and-fft-free-distillation.md)'s
"distill, don't wrap" stance — *adds* a wrapped backend without retracting the distilled one.

## Context

ADR 0001 distilled an FFT-free `SincResampler` (32-tap) out of `rubato` precisely to shed the
`rustfft` dependency and keep a `no_std`-candidate core, and `mixrack` switched its SRC to it. But the
distilled kernel is **~4× shorter** than `rubato`'s default sinc (32 taps vs 128), so its anti-alias
filter is measurably less sharp. For the desktop builds (q-lab / mixrack on a real machine) the
dependency weight is irrelevant and SRC fidelity is what matters; for a future embedded / `no_std`
build the reverse is true. Forcing one choice on every consumer is wrong: the std-vs-`no_std` split is
a **per-build** decision the consumer should own.

## Decision

- **Keep both backends, let the consumer pick.** `samplerack` offers the FFT-free `SincResampler` as
  the dependency-free default *and* a std, high-fidelity `RubatoResampler` behind an opt-in `rubato`
  cargo feature. This relaxes ADR 0001's "don't wrap" to "don't wrap *in the default build*."
- **`RubatoResampler` wraps `rubato`'s async sinc** (`FixedAsync::Input`, 128-tap, BlackmanHarris2 — the
  same "good quality" profile mixrack's old wrapper used) behind the unchanged `Resampler` trait. It
  buffers arbitrary input slices into whole rubato chunks internally (`in_fifo`), parks the variable
  per-chunk output surplus (`out_cache`), and is driven by the same "call until it returns 0" loop as
  the other backends.
- **Interchangeable, not just available.** The rubato backend is made a true drop-in for `SincResampler`:
  it **trims rubato's group delay** so output is content-time-aligned from input frame 0 (no warm-up
  silence), and **caps its flush tail** to the time-aligned length (`ceil(ratio · in_total)`) so total
  output length tracks input × ratio tightly instead of overshooting by a zero-padded chunk. This means
  a consumer can swap backends by a type alias with no alignment or length change — which is exactly how
  mixrack selects between them.
- **Feature, not runtime.** The choice is a cargo feature (`default = []`; `rubato = ["dep:rubato",
  "dep:audioadapter-buffers"]`), because std-vs-`no_std` and the `rustfft` dependency are compile-time
  concerns. The default stays FFT-free.

## Consequences

- Consumers trade off per build: light/`no_std`-candidate (`SincResampler`) vs maximum fidelity
  (`RubatoResampler`). mixrack exposes this as its own feature and selects the backend with a
  `#[cfg]` type alias in its resampler wrapper.
- `rubato` and `audioadapter-buffers` (and transitively `rustfft`) re-enter the dependency graph **only**
  when the `rubato` feature is on; the default build's surface is unchanged from ADR 0001.
- The "wrap vs distill" line is now: distilled backends are the always-available baseline; wrapping an
  external DSP is acceptable as an **opt-in, feature-gated** backend when it buys quality the distilled
  one does not. A future FFT/sync resampler (ADR 0001 / ROADMAP) would follow the same pattern.
- Slightly more surface to maintain (the rubato chunk/trim/flush buffering), but it is a faithful
  relocation of the wrapper logic mixrack previously carried, now living where the backend choice does.
