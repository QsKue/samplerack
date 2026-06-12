# Roadmap

The planned direction for `samplerack`. The crate's job is narrow and stable: own the `Resampler`
contract and provide a useful set of resampler backends behind it. Growth means **adding backends**
under the existing trait, not widening the trait.

Keep this file current: check items off as they land (git is the detailed task log). Each backend
documents its characteristics, latency, and gotchas in its `docs/AREAS/*` entry.

---

## Current ✅

- **`Resampler` contract** — the trait, `ResampleResult`, `sanitize_ratio`, reporting latency as
  `sinerack::Latency` (ADR 0001). Always built.
- **Per-backend feature gating** (ADR 0003) — default = trait + `NoopResampler` only; every real backend
  is its own opt-in feature, mirroring pitchrack ADR 0007.
- **`NoopResampler`** — pass-through baseline (valid at `ratio == 1.0`). Always available.
- **`LinearResampler`** *(feature `linear`)* — linear interpolation; cheapest, ~1 frame latency, no
  anti-aliasing. FFT-free.
- **`SincResampler`** *(feature `sinc`)* — FFT-free windowed-sinc polyphase interpolation (16 taps/side,
  512 sub-phases, Blackman window); anti-aliased downsampling via cutoff scaling. The dependency-free
  high-quality backend and the `rubato` distillation.
- **`RubatoResampler`** *(feature `rubato`)* — std, high-fidelity 128-tap backend wrapping `rubato`'s
  async sinc resampler (ADR 0002). Content-aligned and length-matched, so it is a drop-in for
  `SincResampler`; the consumer picks per build (light/`no_std` vs maximum fidelity).

---

## Consumers

- **mixrack — ✅ DONE: replaced `rubato` for sample-rate conversion** (line-in/device rate → pipeline
  rate). mixrack keeps its own `Resampler` trait (`src/sources/resamplers/`); its `sinc.rs` wrapper
  selects a samplerack backend by `#[cfg]`. mixrack exposes one feature per backend (`resample-linear` /
  `resample-sinc` / `resample-rubato`), each forwarding to the matching samplerack feature — mirroring
  its `pitch-*` detector flags. Default = `resample-rubato` (preserves the pre-samplerack SRC quality);
  `rubato`/`audioadapter-buffers` deps live in samplerack's optional feature now. samplerack also
  migrated from a workspace member to a `[patch]` target.
- **phaserack** (later) — the resample half of time-domain **pitch shifting** (WSOLA increment 2 and
  any generic stretch-then-resample backend). Note PSOLA/parametric pitch shifting need **no**
  resampler, so this is not on the autotune critical path.

---

## Plausible future backends (speculative — not scheduled)

> Real, well-documented options that *could* land if a concrete need appears; none is committed.

- **Cubic / higher-order interpolation** — between linear and sinc on the cost/quality curve; a cheap
  quality bump with a longer kernel than linear. Would join the interpolating family.
- **Cubic-spline or variable-cutoff sinc** — finer control of the anti-alias transition for extreme
  ratios.
- **FFT / sync resampler** (the other rubato engine) — efficient for fixed rational ratios on long
  blocks, but it would pull an FFT (`rustfft`-style) back in. If ever justified it must be a separate
  **feature-gated** module so the default crate stays FFT-free / `no_std`-able. This is the point at
  which a domain namespace (e.g. `interpolating/` vs `spectral/`, mirroring pitchrack) would earn its
  keep; until then the flat module tree is correct.

## `no_std`

The crate is already FFT-free and uses only `alloc` types (`Vec`), so a `no_std` build is a small step
(`#![no_std]` + `extern crate alloc`, gated behind a default-on `std` feature) — **once `sinerack`
supports it**, since `Latency` comes from there. Tracked alongside the wider audio `no_std` effort.

---

Whatever lands, it implements `Resampler` like the existing backends, reports its `sinerack::Latency`,
and gets a `docs/AREAS/*` entry. The trait is expected to stay as-is; revisit it only if a real
backend needs a capability it cannot express.
