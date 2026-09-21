//! 固定种子、跨平台可重放的确定性随机源（xoshiro256** + Box-Muller）。
//!
//! 不使用 `rand` / `getrandom`，因此重放结果只取决于种子与逻辑版本，
//! 与操作系统、线程调度无关。

#[derive(Clone, Debug)]
pub struct Rng {
    state: [u64; 4],
}

#[inline]
fn rotl(x: u64, k: u32) -> u64 {
    x.rotate_left(k)
}

#[inline]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl Rng {
    /// 从一个 64 位种子填充 256 位状态。
    pub fn from_seed(seed: u64) -> Self {
        let mut sm = seed;
        let mut state = [0u64; 4];
        for word in state.iter_mut() {
            *word = splitmix64(&mut sm).max(1);
        }
        Rng { state }
    }

    /// 由父种子与字符串键派生一条独立子流；相同 (seed, key) 永远相同。
    pub fn derive(parent_seed: u64, key: &str) -> Self {
        // FNV-1a 混合父种子字节与键字节，再用 splitmix64 雪崩。
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix_byte = |b: u8| {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        };
        for &b in parent_seed.to_le_bytes().iter() {
            mix_byte(b);
        }
        mix_byte(b'|');
        for &b in key.as_bytes() {
            mix_byte(b);
        }
        let mut sm = hash ^ 0x6a09_e667_f3bc_c908;
        let mut state = [0u64; 4];
        for word in state.iter_mut() {
            *word = splitmix64(&mut sm).max(1);
        }
        Rng { state }
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.state;
        let result = rotl(s[1].wrapping_mul(5), 7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = rotl(s[3], 45);
        result
    }

    /// [0, 1) 上的均匀浮点（53 位尾数）。
    #[inline]
    pub fn uniform01(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// 标准正态，Box-Muller（每次消耗两个均匀数，保证确定、无隐藏状态差异）。
    #[inline]
    pub fn standard_normal(&mut self) -> f64 {
        let u1 = self.uniform01().max(f64::MIN_POSITIVE);
        let u2 = self.uniform01();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// 截断正态样本：均值 mean、标准差 sd，并截断到 [lo, hi]。
    /// 采用拒绝采样（边界外重抽，流位置仍然确定）。
    pub fn truncated_normal(&mut self, mean: f64, sd: f64, lo: f64, hi: f64) -> f64 {
        if sd <= 0.0 {
            return mean.clamp(lo, hi);
        }
        let lo_z = (lo - mean) / sd;
        let hi_z = (hi - mean) / sd;
        if lo_z <= -8.0 && hi_z >= 8.0 {
            return mean + sd * self.standard_normal();
        }
        for _ in 0..10_000 {
            let z = self.standard_normal();
            if z >= lo_z && z <= hi_z {
                return mean + sd * z;
            }
        }
        // 极端情况下（区间极窄）退化为区间中点，仍然确定。
        (lo + hi) / 2.0
    }

    /// 在 0..n 中等概率挑一个下标。
    pub fn choice(&mut self, n: usize) -> usize {
        debug_assert!(n > 0);
        ((self.uniform01() * n as f64) as usize).min(n - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_replay() {
        let mut a = Rng::from_seed(20260921);
        let mut b = Rng::from_seed(20260921);
        let va: Vec<u64> = (0..8).map(|_| a.next_u64()).collect();
        let vb: Vec<u64> = (0..8).map(|_| b.next_u64()).collect();
        assert_eq!(va, vb);
    }

    #[test]
    fn derived_streams_stable() {
        let mut a = Rng::derive(77, "v15:3");
        let mut b = Rng::derive(77, "v15:3");
        assert_eq!(a.next_u64(), b.next_u64());
        let mut c = Rng::derive(77, "v20:3");
        assert_ne!(a.next_u64(), c.next_u64());
    }

    #[test]
    fn truncation_holds() {
        let mut r = Rng::from_seed(1);
        for _ in 0..500 {
            let x = r.truncated_normal(100.0, 30.0, 120.0, 130.0);
            assert!((120.0..=130.0).contains(&x));
        }
    }
}
