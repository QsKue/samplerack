# RubatoResampler (`src/rubato_backend.rs`, feature `rubato`)

Source: `src/rubato_backend.rs` — `RubatoResampler`. The **std, high-fidelity** backend: a thin wrapper
over [`rubato`](https://crates.io/crates/rubato)'s asynchronous sinc resampler. Opt-in behind the
`rubato` cargo feature (pulls `rubato` + `audioadapter-buffers`, transitively `rustfft`). For the
dependency-free default see [`sinc.md`](sinc.md); the rationale for keeping both is
[ADR 0002](../DECISIONS/0002-optional-std-rubato-backend.md).

It is a true **drop-in for `SincResampler`**: same constructors, content-time-aligned from input frame
0, and length-matched to input × ratio — a consumer can switch backends by a type alias with no
alignment or length change.

## Constants (rubato "good quality" profile)

- `SINC_LEN = 128` — filter length (the audible knob: longer = sharper anti-alias, more CPU/latency).
  ~4× the FFT-free backend's 32 taps — the fidelity this backend buys.
- `F_CUTOFF = 0.95`, `OVERSAMPLING_FACTOR = 80`, `WindowFunction::BlackmanHarris2`,
  `SincInterpolationType::Linear` — rubato's good-quality settings.
- `CHUNK_FRAMES = 1024` — fixed input chunk (`FixedAsync::Input`); output frame count per chunk varies.
- `MAX_RELATIVE_RATIO = 8.0` — sizes rubato's internal scratch; a ratio change rebuilds anyway.

## How it works

rubato is **chunk-based** (consumes a fixed `CHUNK_FRAMES` input, emits a whole variable-size output)
and has a **group delay**; the contract is partial-on-both-ends and content-aligned. The wrapper bridges
the two:

- **Lazy build (`ensure_built`).** The filter is built on first `process`/`flush` (it needs the ratio),
  and rebuilt when `set_ratio` moves the ratio. A rebuild clears the per-config state (cache, counters,
  flush flag).
- **Input buffering (`in_fifo`).** Arbitrary input slices are appended to a FIFO; `run_one_chunk` feeds
  rubato one whole `CHUNK_FRAMES` chunk at a time, popping what rubato consumed.
- **Output surplus (`out_cache`).** A chunk's output can overflow a small caller slice; the surplus is
  parked and drained first next call. `process` produces into the cache until it holds an output-slice's
  worth (`produce_until`), then drains (`drain_cache`).
- **Group-delay trim (`lead_trim` / `cache_produced`).** The first `output_delay` output frames are
  rubato's warm-up; they are discarded so frame 0 of the output is content frame 0 — matching the
  FFT-free backend, so a resampled deck stays aligned with a dry one.
- **Tail flush (`flush_tail`).** At EOF, the final partial chunk plus the delay line are drained against
  zero-padding, and delivery is **capped at `ceil(ratio · in_total)`** (`cache_produced_capped`) so the
  trailing zero-pad ringing past the time-aligned length is dropped rather than appended as a silent
  tail. This is what keeps the output length tight (vs rubato's natural one-chunk overshoot).

The `rub`/`in_buf`/`out_buf` borrows are kept disjoint by inlining field access (no `&mut self` helper
straddles the `rubato::process_into_buffer` call).

## Latency

`latency()` reports rubato's `output_delay` (`Latency::new(output_delay, 0, 0)`). The delay is trimmed
internally for content alignment, so this is informational — the stage's inherent buffering for the
engine to account for, not an uncompensated offset.

## Gotchas

- **`flush` until 0.** `flush_tail` runs once (guarded by `flushed`); subsequent `flush` calls drain the
  cache. Skipping flush drops the tail.
- **`set_ratio` rebuilds** the rubato filter on the next call and clears in-flight per-config state — a
  one-off cost. Holding ratio constant is free; mixrack rebuilds on a device-rate change and `reset`s on
  a seek.
- **Fixed channel count.** Built for one `channels`; `process`/`flush` return empty/`0` on a mismatch.
- **Feature-gated.** Absent unless `--features rubato`; the default build never compiles or links it.

## Tests

In-module (`#[cfg(test)]`): constructor arg validation, length-tracks-ratio, frequency preservation up
(16k→48k) and down (48k→32k, anti-aliased), content-aligned leading frame (DC stays at level, no warm-up
silence), stereo independence, finite/bounded output, and reset-equals-fresh. Run with
`cargo test --features rubato`.
