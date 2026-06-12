# SampleRack

**Sample-rate conversion (resampling)** for the q-lib audio engine — a leaf DSP
crate in the audio split, built on the shared
[`sinerack`](https://github.com/QsKue/sinerack) core.

SampleRack owns the `Resampler` contract (change a signal's sample rate by a
`ratio = output_rate / input_rate`) and its implementations. It is the counterpart
to [`phaserack`](https://github.com/QsKue/phaserack)'s time-stretcher: a stretcher
changes length while preserving pitch; a resampler changes both together. That makes
it the engine's **sample-rate-conversion** primitive (e.g. a 48 kHz line-in into a
44.1 kHz pipeline) **and** the second half of time-domain **pitch shifting**
(time-stretch then resample). Backends report their delay as a `sinerack::Latency`
so the engine can sum it across the pipeline.

Backends are **per-feature opt-in** (like `pitchrack`'s detectors): the default build
gives only the `Resampler` trait and `NoopResampler` (pass-through) — if you need no
conversion you pull no backend. Each real backend is its own feature. The two FFT-free
backends (`linear`, `sinc`) keep the crate dependency-free beyond `sinerack` and a
candidate for a future `no_std` build; `rubato` adds a std, high-fidelity backend.

## Implementations

- **`NoopResampler`** — pass-through; the default and a test baseline (valid at
  `ratio == 1.0`). Always available, no feature needed.
- **`LinearResampler`** *(feature `linear`)* — linear interpolation; cheapest, ~1 frame
  latency, no anti-aliasing (aliases when downsampling). FFT-free, dependency-free.
- **`SincResampler`** *(feature `sinc`)* — FFT-free windowed-sinc polyphase interpolation
  (32-tap); high quality, dependency-free, with the cutoff scaled to the output Nyquist so
  downsampling is anti-aliased. The light/`no_std`-candidate high-quality backend.
- **`RubatoResampler`** *(feature `rubato`)* — wraps `rubato`'s asynchronous sinc
  resampler (128-tap, std, pulls `rustfft`) for the sharpest anti-aliasing. Content-
  aligned and length-matched so it swaps in for `SincResampler` unchanged.

## License

Licensed under the GNU Affero General Public License v3.0 or later
([AGPL-3.0-or-later](LICENSE)).
