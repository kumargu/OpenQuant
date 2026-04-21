//! Shared test utilities for basket-picker tests.

/// Linear congruential generator for deterministic test data.
pub struct Lcg {
    state: u64,
}

impl Lcg {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_f64(&mut self) -> f64 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.state >> 33) as f64 / (1u64 << 31) as f64
    }

    /// Generate a uniform random in [lo, hi).
    pub fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.next_f64() * (hi - lo)
    }

    /// Generate a standard normal using Box-Muller.
    pub fn normal(&mut self) -> f64 {
        let u1 = self.next_f64().max(1e-10);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

/// Generate a synthetic OU process.
///
/// dX = kappa * (mu - X) dt + sigma * dW
///
/// Using Euler-Maruyama discretization with dt = 1/252 (daily).
pub fn generate_ou_path(n: usize, kappa: f64, mu: f64, sigma: f64, x0: f64, seed: u64) -> Vec<f64> {
    let mut rng = Lcg::new(seed);
    let dt: f64 = 1.0 / 252.0;
    let sqrt_dt = dt.sqrt();

    let mut path = Vec::with_capacity(n);
    let mut x = x0;

    for _ in 0..n {
        path.push(x);
        let dw = rng.normal() * sqrt_dt;
        x += kappa * (mu - x) * dt + sigma * dw;
    }

    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lcg_deterministic() {
        let mut rng1 = Lcg::new(42);
        let mut rng2 = Lcg::new(42);

        for _ in 0..100 {
            assert_eq!(rng1.next_f64(), rng2.next_f64());
        }
    }

    #[test]
    fn test_generate_ou_path_length() {
        let path = generate_ou_path(100, 10.0, 0.0, 0.1, 0.0, 123);
        assert_eq!(path.len(), 100);
    }

    #[test]
    fn test_generate_ou_path_mean_reverts() {
        // With high kappa, should stay near mu
        let path = generate_ou_path(1000, 50.0, 0.5, 0.05, 0.5, 456);
        let mean: f64 = path.iter().sum::<f64>() / path.len() as f64;
        // Mean should be close to mu
        assert!((mean - 0.5).abs() < 0.1);
    }
}
