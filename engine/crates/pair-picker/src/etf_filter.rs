//! ETF-component exclusion filter.
//!
//! Hard rule: reject any pair where one leg is an ETF and the other is a component.
//! These pairs show spurious cointegration driven by the mechanical ETF rebalancing,
//! not genuine economic relationships.

use std::collections::HashMap;

/// Lookup table of ETF → component symbols.
///
/// Covers major sector ETFs used in the current universe.
/// Extend as the universe grows.
fn etf_components() -> HashMap<&'static str, &'static [&'static str]> {
    let mut map = HashMap::new();

    map.insert(
        "XLF",
        &[
            "JPM", "BAC", "WFC", "GS", "MS", "C", "USB", "PNC", "SCHW", "BLK", "AXP", "BK", "TFC",
            "COF", "AIG",
        ][..],
    );

    map.insert(
        "XLE",
        &[
            "XOM", "CVX", "COP", "SLB", "EOG", "PSX", "VLO", "MPC", "HAL", "OXY", "DVN", "HES",
            "FANG", "BKR",
        ][..],
    );

    map.insert(
        "XLK",
        &[
            "AAPL", "MSFT", "NVDA", "AVGO", "ORCL", "CRM", "ADBE", "AMD", "INTC", "QCOM", "TXN",
            "NOW", "AMAT",
        ][..],
    );

    map.insert(
        "XLV",
        &[
            "UNH", "JNJ", "LLY", "PFE", "ABBV", "MRK", "TMO", "ABT", "DHR", "BMY", "AMGN", "MDT",
        ][..],
    );

    map.insert(
        "XLY",
        &[
            "AMZN", "TSLA", "HD", "MCD", "NKE", "LOW", "SBUX", "TJX", "BKNG", "CMG",
        ][..],
    );

    map.insert(
        "XLP",
        &[
            "PG", "KO", "PEP", "COST", "WMT", "PM", "MO", "CL", "MDLZ", "KHC", "GIS",
        ][..],
    );

    map.insert(
        "XLU",
        &[
            "NEE", "SO", "DUK", "SRE", "AEP", "D", "EXC", "XEL", "WEC", "ED",
        ][..],
    );

    map.insert(
        "XLI",
        &[
            "UNP", "HON", "CAT", "BA", "RTX", "DE", "LMT", "GE", "MMM", "FDX", "UPS",
        ][..],
    );

    map.insert(
        "XLB",
        &[
            "LIN", "SHW", "APD", "ECL", "FCX", "NEM", "DOW", "DD", "NUE", "VMC",
        ][..],
    );

    map.insert(
        "XLRE",
        &[
            "PLD", "AMT", "CCI", "EQIX", "SPG", "PSA", "O", "DLR", "WELL", "AVB",
        ][..],
    );

    map.insert("GLD", &["GOLD", "NEM", "AEM", "RGLD"][..]);

    map.insert(
        "SMH",
        &[
            "NVDA", "AMD", "AVGO", "INTC", "QCOM", "TXN", "MU", "MRVL", "LRCX", "AMAT", "KLAC",
            "ON", "TSM",
        ][..],
    );

    map.insert(
        "QQQ",
        &[
            "AAPL", "MSFT", "NVDA", "AMZN", "META", "GOOGL", "GOOG", "AVGO", "TSLA", "COST",
            "NFLX", "AMD", "ADBE", "PEP", "INTC", "QCOM",
        ][..],
    );

    map
}

/// Check if a pair is excluded by the ETF-component rule.
///
/// Returns `true` if the pair should be REJECTED (one is ETF, other is component).
pub fn is_etf_component_pair(sym_a: &str, sym_b: &str) -> bool {
    let table = etf_components();

    // Check if A is ETF and B is component
    if let Some(components) = table.get(sym_a) {
        if components.contains(&sym_b) {
            return true;
        }
    }

    // Check if B is ETF and A is component
    if let Some(components) = table.get(sym_b) {
        if components.contains(&sym_a) {
            return true;
        }
    }

    false
}

/// List all known ETF symbols.
pub fn known_etfs() -> Vec<&'static str> {
    vec![
        "XLF", "XLE", "XLK", "XLV", "XLY", "XLP", "XLU", "XLI", "XLB", "XLRE", "GLD", "SLV", "SMH",
        "QQQ", "SPY", "IWM", "DIA", "EEM", "TLT", "HYG", "LQD", "XBI",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_etf_component_rejected() {
        assert!(is_etf_component_pair("XLF", "JPM"));
        assert!(is_etf_component_pair("JPM", "XLF")); // order doesn't matter
        assert!(is_etf_component_pair("XLE", "XOM"));
        assert!(is_etf_component_pair("SMH", "NVDA"));
        assert!(is_etf_component_pair("QQQ", "AAPL"));
    }

    #[test]
    fn test_non_etf_pair_allowed() {
        assert!(!is_etf_component_pair("GS", "MS"));
        assert!(!is_etf_component_pair("JPM", "BAC"));
        assert!(!is_etf_component_pair("AAPL", "MSFT"));
    }

    #[test]
    fn test_etf_etf_pair_allowed() {
        // Two ETFs are fine (not ETF-component)
        assert!(!is_etf_component_pair("XLF", "XLE"));
        assert!(!is_etf_component_pair("GLD", "SLV"));
    }

    #[test]
    fn test_component_component_allowed() {
        // Two components of the same ETF are fine
        assert!(!is_etf_component_pair("JPM", "GS"));
        assert!(!is_etf_component_pair("XOM", "CVX"));
    }
}
