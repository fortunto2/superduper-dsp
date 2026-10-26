/// Fixed integer-sample delay line — pushes a sample and returns the one from
/// `delay` samples ago, via a `delay+1` ring. Alloc-free/RT-safe after `new()`.
/// Used to latency-match a dry path to a processed path, or to pad a processor's
/// intrinsic latency up to a reported PDC value. The read-before-write order
/// matches the hand-rolled rings it replaces, so swapping to it is bit-identical.
#[derive(Clone)]
pub struct LatencyDelay {
    buf: Box<[f32]>,
    w: usize,
}

impl LatencyDelay {
    pub fn new(delay: usize) -> Self {
        Self {
            buf: vec![0.0; delay + 1].into_boxed_slice(),
            w: 0,
        }
    }

    /// Push `x`, return the sample from `delay` samples ago.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        // Wrap by compare, not `%`: this runs per sample on every dry path.
        let next = if self.w + 1 == self.buf.len() { 0 } else { self.w + 1 };
        let out = self.buf[next];
        self.buf[self.w] = x;
        self.w = next;
        out
    }

    pub fn reset(&mut self) {
        self.buf.fill(0.0);
        self.w = 0;
    }
}
