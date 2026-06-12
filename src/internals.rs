//! Shared streaming machinery for the resampler backends: an interleaved input
//! history buffer addressed by *absolute* per-channel frame index, so a backend
//! can read a window around a fractional read position and trim what it has passed.

/// Interleaved input history with absolute per-channel frame addressing.
pub(crate) struct InputHistory {
    /// Pending input, interleaved. `buf[0]` is absolute per-channel frame `start`.
    buf: Vec<f32>,
    /// Absolute per-channel frame index of `buf[0]`.
    start: i64,
    channels: usize,
}

impl InputHistory {
    pub(crate) fn new(channels: usize) -> Self {
        Self {
            buf: Vec::new(),
            start: 0,
            channels,
        }
    }

    pub(crate) fn push(&mut self, input: &[f32]) {
        self.buf.extend_from_slice(input);
    }

    /// Per-channel frames currently buffered.
    pub(crate) fn frames(&self) -> usize {
        self.buf.len() / self.channels
    }

    /// Absolute per-channel frame just past the buffered input.
    pub(crate) fn end(&self) -> i64 {
        self.start + self.frames() as i64
    }

    /// One interleaved sample at absolute per-channel frame `abs`, channel `ch`;
    /// `0.0` outside the buffered range (so forward taps zero-pad at end-of-stream).
    #[inline]
    pub(crate) fn at(&self, abs: i64, ch: usize) -> f32 {
        let rel = abs - self.start;
        if rel < 0 || rel as usize >= self.frames() {
            return 0.0;
        }
        self.buf[rel as usize * self.channels + ch]
    }

    /// Drops buffered input before absolute frame `keep_from` (clamped to what is
    /// buffered) — input no future read can reach. `keep_from` is clamped to both
    /// `start` (no negative drop) and `end` (a read position past the buffer drops
    /// everything, never more — a heavy downsample step can overshoot the buffer).
    pub(crate) fn trim(&mut self, keep_from: i64) {
        let keep_from = keep_from.clamp(self.start, self.end());
        let drop = (keep_from - self.start) as usize;
        if drop > 0 {
            self.buf.drain(0..drop * self.channels);
            self.start += drop as i64;
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.clear();
        self.start = 0;
    }
}
