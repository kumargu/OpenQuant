//! Spread computation for basket strategies.

/// Build spread = log(target) - mean(log(peers)).
///
/// All input slices must have the same length (aligned bars).
/// Returns `None` if inputs are misaligned or contain non-positive prices.
pub fn build_spread(target: &[f64], peers: &[&[f64]]) -> Option<Vec<f64>> {
    if peers.is_empty() {
        return None;
    }

    let n = target.len();
    for peer in peers {
        if peer.len() != n {
            return None;
        }
    }

    let num_peers = peers.len() as f64;
    let mut spread = Vec::with_capacity(n);

    for i in 0..n {
        let target_price = target[i];
        if target_price <= 0.0 || !target_price.is_finite() {
            return None;
        }
        let log_target = target_price.ln();

        let mut log_peer_sum = 0.0;
        for peer in peers {
            let peer_price = peer[i];
            if peer_price <= 0.0 || !peer_price.is_finite() {
                return None;
            }
            log_peer_sum += peer_price.ln();
        }

        let log_peer_mean = log_peer_sum / num_peers;
        spread.push(log_target - log_peer_mean);
    }

    Some(spread)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_spread_basic() {
        let target = vec![100.0, 101.0, 102.0];
        let peer1 = vec![50.0, 51.0, 52.0];
        let peer2 = vec![75.0, 76.0, 77.0];
        let peers: Vec<&[f64]> = vec![&peer1, &peer2];

        let spread = build_spread(&target, &peers).unwrap();
        assert_eq!(spread.len(), 3);

        // Manual check for first element
        let expected = (100.0_f64).ln() - ((50.0_f64).ln() + (75.0_f64).ln()) / 2.0;
        assert!((spread[0] - expected).abs() < 1e-10);
    }

    #[test]
    fn test_build_spread_misaligned() {
        let target = vec![100.0, 101.0];
        let peer1 = vec![50.0, 51.0, 52.0]; // Different length
        let peers: Vec<&[f64]> = vec![&peer1];

        assert!(build_spread(&target, &peers).is_none());
    }

    #[test]
    fn test_build_spread_negative_price() {
        let target = vec![100.0, -1.0, 102.0];
        let peer1 = vec![50.0, 51.0, 52.0];
        let peers: Vec<&[f64]> = vec![&peer1];

        assert!(build_spread(&target, &peers).is_none());
    }

    #[test]
    fn test_build_spread_empty_peers() {
        let target = vec![100.0];
        let peers: Vec<&[f64]> = vec![];
        assert!(build_spread(&target, &peers).is_none());
    }
}
