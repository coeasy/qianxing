//! 确定性随机数（xorshift64*）。
//!
//! 不依赖 `rand` crate：外部 RNG 的实现细节可能随版本变化，
//! 一旦变化，"相同种子相同结果"的承诺就失效了。

pub struct DeterministicRng {
    s: u64,
}

impl DeterministicRng {
    pub fn new(seed: u64) -> Self {
        Self {
            s: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.s;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.s = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 返回 [0, SCALE) 的整数，用作定点概率（SCALE = 1e9）。
    pub fn next_prob(&mut self) -> i128 {
        (self.next_u64() % 1_000_000_000) as i128
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = DeterministicRng::new(42);
        let mut b = DeterministicRng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seed_differs() {
        let mut a = DeterministicRng::new(1);
        let mut b = DeterministicRng::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn prob_in_range() {
        let mut r = DeterministicRng::new(7);
        for _ in 0..1000 {
            let p = r.next_prob();
            assert!((0..1_000_000_000).contains(&p));
        }
    }
}
