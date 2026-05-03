//! Component-dominance diagnostics for basket spreads.

/// Maximum absolute component contribution share to spread variance.
///
/// Spread weights are `[+1, -1/n, -1/n, ...]` on log-prices. We compute the
/// covariance matrix of the weighted components over the fit window, then the
/// marginal variance contribution of each component:
///
/// `contrib_i = w_i * (Σw)_i / (w'Σw)`.
///
/// The returned score is `max_i |contrib_i|`. Values near 1 mean a single name
/// effectively dominates the spread's risk budget.
pub fn max_component_dominance(target: &[f64], peers: &[&[f64]]) -> Option<f64> {
    if peers.is_empty() {
        return None;
    }
    let n = target.len();
    if n < 10 {
        return None;
    }
    for peer in peers {
        if peer.len() != n {
            return None;
        }
    }

    let m = peers.len() + 1;
    let mut cols: Vec<Vec<f64>> = Vec::with_capacity(m);
    cols.push(target.iter().map(|p| p.ln()).collect());
    for peer in peers {
        cols.push(peer.iter().map(|p| p.ln()).collect());
    }

    let means: Vec<f64> = cols
        .iter()
        .map(|col| col.iter().sum::<f64>() / n as f64)
        .collect();

    let mut cov = vec![0.0; m * m];
    for i in 0..m {
        for j in i..m {
            let mut s = 0.0;
            for t in 0..n {
                s += (cols[i][t] - means[i]) * (cols[j][t] - means[j]);
            }
            let v = s / (n as f64 - 1.0);
            cov[i * m + j] = v;
            cov[j * m + i] = v;
        }
    }

    let mut w = vec![0.0; m];
    w[0] = 1.0;
    let peer_w = -1.0 / peers.len() as f64;
    for wi in w.iter_mut().skip(1) {
        *wi = peer_w;
    }

    let mut sigma_w = vec![0.0; m];
    for i in 0..m {
        let mut s = 0.0;
        for j in 0..m {
            s += cov[i * m + j] * w[j];
        }
        sigma_w[i] = s;
    }

    let total_var: f64 = w.iter().zip(sigma_w.iter()).map(|(wi, swi)| wi * swi).sum();
    if !total_var.is_finite() || total_var.abs() < 1e-15 {
        return None;
    }

    let max_abs = w
        .iter()
        .zip(sigma_w.iter())
        .map(|(wi, swi)| (wi * swi / total_var).abs())
        .fold(0.0_f64, f64::max);

    Some(max_abs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_single_name_dominance() {
        let target: Vec<f64> = (0..100).map(|i| 100.0 + i as f64 * 2.0).collect();
        let peer1: Vec<f64> = (0..100).map(|i| 50.0 + i as f64 * 0.1).collect();
        let peer2: Vec<f64> = (0..100).map(|i| 60.0 + i as f64 * 0.1).collect();
        let score = max_component_dominance(&target, &[&peer1, &peer2]).unwrap();
        assert!(score > 0.8, "{score}");
    }
}
