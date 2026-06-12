# Decision: per-backend feature gating (default = trait + Noop only)

## Status

Accepted (2026-06-12). Refines [ADR 0001](0001-resampler-contract-and-fft-free-distillation.md) (which
left `Linear`/`Sinc` always compiled) and sits alongside [ADR 0002](0002-optional-std-rubato-backend.md)
(which already feature-gated `rubato`). Mirrors **pitchrack ADR 0007** (per-detector opt-in) and the
phaserack feature layout.

## Context

After ADR 0002 the feature set was lopsided: `rubato` was opt-in, but `Linear` and `Sinc` were compiled
unconditionally in every build (default `[]` still pulled both). That is inconsistent with the rest of
the `*rack` family, where the default crate gives you *nothing* concrete and you opt into exactly the
algorithms you want (pitchrack: per-detector; phaserack: per-stretcher). A consumer that only needs the
rubato backend still compiled the FFT-free sinc; a consumer that needs no conversion at all still got two
backends it would never call. The crate should let the consumer pull only what it uses.

## Decision

- **Every real backend is its own cargo feature; the default builds only the contract.** `default = []`
  yields the `Resampler` trait, `ResampleResult`, and `NoopResampler` (pass-through) — nothing else.
  Features: `linear`, `sinc` (both FFT-free, dependency-free) and `rubato` (std, pulls
  `rubato`/`audioadapter-buffers`/`rustfft`).
- **`NoopResampler` stays always-available, ungated.** It is trivial, dependency-free, and *is* the
  "I imported the crate but need no conversion" answer — so it is the natural default. (Rationale: if you
  need no conversion you would not pull a backend; Noop covers the ratio-1 / pass-through slot.)
- **Gate the shared helpers to their users.** `internals::InputHistory` compiles under
  `cfg(any(feature = "linear", feature = "sinc"))` (the rubato backend uses its own FIFO);
  `sanitize_ratio` compiles only when some real backend is on. This keeps the trait-only build free of
  dead-code warnings under `clippy -D warnings`.
- **Tests live with their backend.** Each backend module carries its own `#[cfg(test)]` tests; the
  `lib.rs` cross-backend suite is gated `all(feature = "linear", feature = "sinc")`. CI runs
  `--no-default-features` (clippy + build, trait-only) and `--all-features` (clippy + build + test),
  exactly like pitchrack's matrix.

## Consequences

- A consumer compiles only the backend(s) it selects — e.g. `--features rubato` pulls no FFT-free sinc;
  the default pulls no backend at all. mixrack forwards this with one feature per backend
  (`resample-linear`/`resample-sinc`/`resample-rubato`), mirroring its `pitch-*` detector flags.
- The default build's dependency and code surface shrinks to the contract + Noop; the FFT-free promise
  now also means "no backend code you didn't ask for."
- Adding a backend is: a new feature, a `#[cfg(feature = "…")]` module, a gated re-export, and (if it
  needs `InputHistory`/`sanitize_ratio`) widening those `cfg(any(...))` guards — plus a `docs/AREAS/*`
  entry. The trait itself is untouched, as ADR 0001 intended.
