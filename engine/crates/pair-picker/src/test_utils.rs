//! Shared test utilities — deterministic data generators for reproducible tests.

/// Deterministic LCG noise generator (no external dependency).
/// Same sequence for same seed — tests are fully reproducible.
pub struct Lcg {
    state: u64,
}

impl Lcg {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Generate a noise value in [-scale/2, +scale/2].
    pub fn next(&mut self, scale: f64) -> f64 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.state >> 33) as f64 / u32::MAX as f64 - 0.5) * scale
    }
}

/// Generate a stationary AR(1) series: y_t = phi * y_{t-1} + noise.
pub fn stationary_series(n: usize, phi: f64, noise_scale: f64, seed: u64) -> Vec<f64> {
    let mut lcg = Lcg::new(seed);
    let mut y = Vec::with_capacity(n);
    let mut val = 0.0;
    for _ in 0..n {
        val = phi * val + lcg.next(noise_scale);
        y.push(val);
    }
    y
}

/// Generate a random walk (unit root): y_t = y_{t-1} + noise.
pub fn random_walk(n: usize, noise_scale: f64, seed: u64) -> Vec<f64> {
    let mut lcg = Lcg::new(seed);
    let mut y = Vec::with_capacity(n);
    let mut val = 0.0;
    for _ in 0..n {
        val += lcg.next(noise_scale);
        y.push(val);
    }
    y
}

/// Generate an OU process with known half-life.
/// s_t = phi * s_{t-1} + noise, where phi = exp(-ln(2) / half_life).
pub fn ou_process(n: usize, half_life: f64, noise_scale: f64, seed: u64) -> Vec<f64> {
    let phi = (-f64::ln(2.0) / half_life).exp();
    stationary_series(n, phi, noise_scale, seed)
}

/// Generate a cointegrated pair: log_a = beta * log_b + alpha + OU_spread.
/// Returns (prices_a, prices_b) as raw prices (not log).
pub fn cointegrated_pair(n: usize, beta: f64, half_life: f64, seed: u64) -> (Vec<f64>, Vec<f64>) {
    let phi = (-f64::ln(2.0) / half_life).exp();
    let mut lcg = Lcg::new(seed);

    let mut log_b = Vec::with_capacity(n);
    let mut b_val = 4.0; // ln(~55)
    for _ in 0..n {
        b_val += lcg.next(0.02);
        log_b.push(b_val);
    }

    let mut spread = 0.0;
    let mut log_a = Vec::with_capacity(n);
    for lb in &log_b {
        spread = phi * spread + lcg.next(0.01);
        log_a.push(beta * lb + 1.0 + spread);
    }

    let prices_a: Vec<f64> = log_a.iter().map(|x| x.exp()).collect();
    let prices_b: Vec<f64> = log_b.iter().map(|x| x.exp()).collect();
    (prices_a, prices_b)
}

/// Generate two independent random walks (not cointegrated).
/// Returns (prices_a, prices_b) as raw prices.
pub fn independent_walks(n: usize, seed: u64) -> (Vec<f64>, Vec<f64>) {
    let mut lcg = Lcg::new(seed);
    let mut a = Vec::with_capacity(n);
    let mut b = Vec::with_capacity(n);
    let mut va = 4.0;
    let mut vb = 4.0;
    for _ in 0..n {
        va += lcg.next(0.02);
        vb += lcg.next(0.02);
        a.push(va.exp());
        b.push(vb.exp());
    }
    (a, b)
}
