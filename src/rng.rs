//! Deterministic RNG (xoshiro256**) and the small distribution helpers used by
//! the sampler. Every stream is derived from the run seed plus a stable tag,
//! so replaying a run yields bit-identical quantiles.

pub struct Rng {
    s: [u64; 4],
    spare_normal: Option<f64>,
}

fn fnv1a(tag: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in tag.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

impl Rng {
    pub fn derived(seed: u64, tag: &str) -> Rng {
        let base = seed ^ fnv1a(tag);
        let mut s = [0u64; 4];
        for (i, slot) in s.iter_mut().enumerate() {
            *slot = splitmix(base.wrapping_add((i as u64).wrapping_mul(0x9e3779b97f4a7c15)));
        }
        Rng {
            s,
            spare_normal: None,
        }
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    #[inline]
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / 9007199254740992.0
    }

    pub fn uniform_range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.uniform()
    }

    pub fn index(&mut self, n: usize) -> usize {
        (self.uniform() * n as f64) as usize
    }

    pub fn exponential(&mut self, mean: f64) -> f64 {
        let u = self.uniform().max(1e-15);
        -mean * u.ln()
    }

    /// Standard normal via the Marsaglia polar method.
    pub fn normal(&mut self) -> f64 {
        if let Some(z) = self.spare_normal.take() {
            return z;
        }
        loop {
            let u = self.uniform_range(-1.0, 1.0);
            let v = self.uniform_range(-1.0, 1.0);
            let s = u * u + v * v;
            if s > 0.0 && s < 1.0 {
                let m = (-2.0 * s.ln() / s).sqrt();
                self.spare_normal = Some(v * m);
                return u * m;
            }
        }
    }

    /// Gamma(shape, scale). Marsaglia-Tsang for shape >= 1, Ahrens-Dieter
    /// boost trick for shape < 1.
    pub fn gamma(&mut self, shape: f64, scale: f64) -> f64 {
        let g = if shape >= 1.0 {
            let d = shape - 1.0 / 3.0;
            let c = 1.0 / (9.0 * d).sqrt();
            loop {
                let x = self.normal();
                let v = (1.0 + c * x).powi(3);
                if v <= 0.0 {
                    continue;
                }
                let u = self.uniform();
                if u < 1.0 - 0.036 * x.powi(4) {
                    break d * v;
                }
                if u.ln() < 0.5 * x * x + d * (1.0 - v + v.ln()) {
                    break d * v;
                }
            }
        } else {
            let u = self.uniform().max(1e-15);
            self.gamma(shape + 1.0, 1.0) * u.powf(1.0 / shape)
        };
        g * scale
    }

    /// Normal(mu, sigma) truncated to [lo, hi]. Falls back to a uniform draw
    /// inside the box when rejection sampling cannot reach the target.
    pub fn truncated_normal(&mut self, mu: f64, sigma: f64, lo: f64, hi: f64) -> f64 {
        if hi <= lo {
            return lo;
        }
        if sigma <= 0.0 {
            return mu.clamp(lo, hi);
        }
        for _ in 0..64 {
            let x = mu + sigma * self.normal();
            if x >= lo && x <= hi {
                return x;
            }
        }
        self.uniform_range(lo, hi)
    }

    /// Gamma with the given mean / one-sigma, truncated to [lo, hi].
    pub fn truncated_gamma(&mut self, mean: f64, sigma: f64, lo: f64, hi: f64) -> f64 {
        if hi <= lo {
            return lo;
        }
        let sigma = sigma.max(mean * 0.05);
        let shape = (mean / sigma).powi(2);
        let scale = mean / shape;
        for _ in 0..64 {
            let x = self.gamma(shape, scale);
            if x >= lo && x <= hi {
                return x;
            }
        }
        self.uniform_range(lo, hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_streams() {
        let a = Rng::derived(42, "edge-1").next_u64();
        let b = Rng::derived(42, "edge-1").next_u64();
        let c = Rng::derived(42, "edge-2").next_u64();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
