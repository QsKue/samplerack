# AGENTS.md

`samplerack` is a small Rust **library crate** (edition 2024, `AGPL-3.0-or-later`) that does
**sample-rate conversion (resampling)** on interleaved `f32` audio — it changes a signal's sample
rate by a `ratio = output_rate / input_rate`. It is a **leaf** in the q-lib audio split: it owns the
`Resampler` contract and its implementations and nothing else — no engine, no session, no I/O, no
async. Backends are **per-feature opt-in** (like pitchrack's detectors): the default builds only the
`Resampler` trait + `NoopResampler`. It depends only on **SineRack** for the shared `Latency` value type
and is **FFT-free** (no `rustfft`); the `linear`/`sinc` backends keep that, while the optional `rubato`
feature adds a std, high-fidelity backend that pulls `rubato`/`rustfft`. Keep changes minimal,
allocation-conscious on the hot path, and engine-agnostic.

**Why it exists / workspace context.** Resampling is the engine's sample-rate-conversion primitive
(line-in at one rate into a pipeline at another) **and** the second half of time-domain pitch shifting
(time-stretch via [`phaserack`](https://github.com/QsKue/phaserack), then resample). It was distilled
from `rubato` to own the algorithm and shed `rubato`'s `rustfft` dependency (a `no_std` goal).
**MixRack** is the hub / consumer ([QsKue/mixrack](https://github.com/QsKue/mixrack)); **SineRack** is
the shared base ([QsKue/sinerack](https://github.com/QsKue/sinerack)). SampleRack must stand alone —
build and reason about it without the engine — and must not gain engine/session knowledge. All the
audio crates are git submodules and path workspace members.

## Where to look

- `docs/ARCHITECTURE.md` — module tree, the `Resampler` contract, the process/flush/latency data
  flow, and the check commands. Read before touching the trait or the public API.
- `docs/AREAS/*.md` — per-module conventions and gotchas. Read the one for any file you change.
- `docs/DECISIONS/*.md` — durable design decisions with rationale (ADRs).
- `docs/ROADMAP.md` — current implementations and the plausible future backends.

## Architecture in one screen

- `src/lib.rs` — crate root; declares modules and re-exports the trait + types + `NoopResampler` (always)
  and `LinearResampler`/`SincResampler`/`RubatoResampler` (each under its feature). Carries the
  feature-gated cross-backend test suite.
- `src/resampler.rs` — the **contract** (always built): the `Resampler` trait
  (`process`/`flush`/`reset`/`latency`/`set_ratio`/`ratio` on interleaved `f32`), `ResampleResult`,
  the shared `sanitize_ratio` (gated to when a real backend is on), and `NoopResampler` (pass-through).
  Dependency-free beyond `sinerack::Latency`.
- `src/linear.rs` *(feature `linear`)* — `LinearResampler`: linear interpolation; cheap, ~1 frame
  latency, no anti-alias. FFT-free.
- `src/sinc.rs` *(feature `sinc`)* — `SincResampler`: FFT-free windowed-sinc polyphase; high quality,
  anti-aliased downsampling. The dependency-free high-quality backend.
- `src/rubato_backend.rs` *(feature `rubato`)* — `RubatoResampler`: std 128-tap `rubato` wrapper; the
  high-fidelity backend. Content-aligned + length-matched, a drop-in for `SincResampler`.
- `src/internals.rs` *(cfg `any(linear, sinc)`)* — `InputHistory`: the shared streaming input buffer
  (absolute frame addressing, read-around, trim) the distilled backends are built on. The rubato backend
  buffers via its own FIFO instead.

## Conventions (the durable rules)

- **Interleaved `f32`, separate buffers.** `process` reads interleaved input and writes a *separate*
  interleaved output buffer; frame counts derive from `slice.len() / channels`. No in-place mode.
- **Partial consume/produce is normal.** `process` returns a `ResampleResult` (`input_frames_consumed`,
  `output_frames_written`); a call may consume/write fewer frames than the buffers hold. Callers loop
  and honor both counts — never assume a call drains the input or fills the output.
- **Tail via `flush`.** After the final input block, `flush` drains the filter lookahead against
  zero-padding (returns the frame count) until it returns `0`.
- **Ratio is `output_rate / input_rate`.** `ratio > 1` upsamples, `ratio < 1` downsamples. `set_ratio`
  sanitizes to finite-positive; backends that filter (sinc) scale the cutoff to the output Nyquist
  when downsampling so the result is anti-aliased.
- **Report latency as `sinerack::Latency`.** `latency()` returns a `sinerack::Latency` so the engine can
  sum it across stages. Don't invent a local latency type.
- **Per-backend opt-in.** Default = trait + `Noop` only; every real backend is its own feature (ADR
  0003). A new backend = a new feature + a `#[cfg(feature = "…")]` module + a gated re-export. Keep
  `sanitize_ratio`/`internals` gated to the backends that use them so the trait-only build stays clean
  under `clippy -D warnings`.
- **FFT-free by default.** Do not add `rustfft` (or any FFT) to the default or to a `linear`/`sinc`
  build — the crate's value is a light, `no_std`-able resampler. Heavy/std backends (the `rubato`
  feature; a future FFT/sync backend) must stay **opt-in feature-gated modules** so the default surface
  never grows.
- **Stay engine-agnostic and small.** No session, routing, device, or pipeline concepts. SampleRack
  transforms a buffer the caller provides; the engine decides when and why.

## Warning signs

- A method assumes `process` drained the input or filled the output instead of reading `ResampleResult`.
- `latency()` returns something other than `sinerack::Latency`, or a local latency type creeps in.
- `rustfft` / an FFT dependency appears (the thing the distillation set out to avoid).
- A backend reads input it didn't guarantee is buffered (bypassing `InputHistory::at`'s zero-pad), or
  forgets to `trim` and grows memory unbounded.
- Engine/session concepts (sources, sessions, routing) appear anywhere in the crate.

## Making a change

1. Read `docs/ARCHITECTURE.md` (if touching the trait/API boundary) and the relevant `docs/AREAS/*.md`.
2. Keep the change small and engine-agnostic; keep doc updates near the behavior change.
3. Run the checks in `docs/ARCHITECTURE.md` (fmt/clippy/test). When changing the trait or a result's
   semantics, update `NoopResampler`, every backend (`linear`/`sinc`/`rubato`), and the docs together.
   Run clippy/build `--no-default-features` (trait + Noop only) **and** `--all-features`.

## Docs maintenance

- **Code is truth for behavior; docs explain why and what-not-to-do, not line-by-line how.**
- **Git is the task log** — no changelog directory; don't create one.
- Update the smallest useful set: `docs/AREAS/*` for a changed convention/gotcha (one file per real
  module — keep `Source:` paths honest), `docs/ARCHITECTURE.md` for the trait / data flow / API shape,
  a new `docs/DECISIONS/` ADR (from `docs/TEMPLATES/decision-template.md`) for a durable choice,
  `docs/ROADMAP.md` for plan and status. Keep every doc short enough to read at task start.
