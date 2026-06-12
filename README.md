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

By **default** the crate is dependency-free beyond `sinerack` and **FFT-free** (no
`rustfft`), so it stays light and is a candidate for a future `no_std` build — its
in-house sinc backend was distilled from `rubato` precisely to drop that dependency
surface. For builds where SRC fidelity outweighs a light dependency set, the optional
`rubato` feature adds a **std, high-fidelity** backend that wraps `rubato` directly.
Both sinc backends are content-time-aligned and implement the same contract, so they
are drop-in interchangeable — the consumer picks per build.

## Implementations

- **`NoopResampler`** — pass-through; the default and a test baseline (valid at
  `ratio == 1.0`).
- **`LinearResampler`** — linear interpolation; cheapest, ~1 frame latency, no
  anti-aliasing (aliases when downsampling).
- **`SincResampler`** — FFT-free windowed-sinc polyphase interpolation (32-tap);
  high quality, dependency-free, with the cutoff scaled to the output Nyquist so
  downsampling is anti-aliased. The default for a light/`no_std`-candidate build.
- **`RubatoResampler`** *(feature `rubato`)* — wraps `rubato`'s asynchronous sinc
  resampler (128-tap, std, pulls `rustfft`) for the sharpest anti-aliasing. Content-
  aligned and length-matched so it swaps in for `SincResampler` unchanged.

## License

Licensed under the GNU Affero General Public License v3.0 or later
([AGPL-3.0-or-later](LICENSE)).
