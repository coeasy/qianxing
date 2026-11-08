//! Seeded deterministic pseudo-random generator shared by experiment search modes.

#[derive(Clone, Debug)]
pub(super) struct SplitMix64(u64);

impl SplitMix64 {
    pub(super) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(super) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    pub(super) fn index(&mut self, len: usize) -> usize {
        (self.next() % len as u64) as usize
    }

    pub(super) fn index_u64(&mut self, len: u64) -> u64 {
        self.next() % len
    }

    pub(super) fn next_f64(&mut self) -> f64 {
        ((self.next() >> 11) as f64) / ((1_u64 << 53) as f64)
    }
}
