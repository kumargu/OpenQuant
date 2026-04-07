//! ETF-component exclusion filter.
//!
//! Hard rule: reject any pair where one leg is an ETF and the other is a component.
//! These pairs show spurious cointegration driven by the mechanical ETF rebalancing,
//! not genuine economic relationships.

/// ETF-to-components lookup table as a flat array.
/// Linear scan over ~120 entries beats HashMap construction for our use case
/// (called once per pair validation, not in a hot loop).
const ETF_COMPONENTS: &[(&str, &[&str])] = &[
    (
        "XLF",
        &[
            "JPM", "BAC", "WFC", "GS", "MS", "C", "USB", "PNC", "SCHW", "BLK", "AXP", "BK", "TFC",
            "COF", "AIG",
        ],
    ),
    (
        "XLE",
        &[
            "XOM", "CVX", "COP", "SLB", "EOG", "PSX", "VLO", "MPC", "HAL", "OXY", "DVN", "HES",
            "FANG", "BKR",
        ],
    ),
    (
        "XLK",
        &[
            "AAPL", "MSFT", "NVDA", "AVGO", "ORCL", "CRM", "ADBE", "AMD", "INTC", "QCOM", "TXN",
            "NOW", "AMAT",
        ],
    ),
    (
        "XLV",
        &[
            "UNH", "JNJ", "LLY", "PFE", "ABBV", "MRK", "TMO", "ABT", "DHR", "BMY", "AMGN", "MDT",
        ],
    ),
    (
        "XLY",
        &[
            "AMZN", "TSLA", "HD", "MCD", "NKE", "LOW", "SBUX", "TJX", "BKNG", "CMG",
        ],
    ),
    (
        "XLP",
        &[
            "PG", "KO", "PEP", "COST", "WMT", "PM", "MO", "CL", "MDLZ", "KHC", "GIS",
        ],
    ),
    (
        "XLU",
        &[
            "NEE", "SO", "DUK", "SRE", "AEP", "D", "EXC", "XEL", "WEC", "ED",
        ],
    ),
    (
        "XLI",
        &[
            "UNP", "HON", "CAT", "BA", "RTX", "DE", "LMT", "GE", "MMM", "FDX", "UPS",
        ],
    ),
    (
        "XLB",
        &[
            "LIN", "SHW", "APD", "ECL", "FCX", "NEM", "DOW", "DD", "NUE", "VMC",
        ],
    ),
    (
        "XLRE",
        &[
            "PLD", "AMT", "CCI", "EQIX", "SPG", "PSA", "O", "DLR", "WELL", "AVB",
        ],
    ),
    ("GLD", &["GOLD", "NEM", "AEM", "RGLD", "FNV", "WPM", "KGC", "BTG"]),
    (
        "GDX",
        &["NEM", "GOLD", "AEM", "FNV", "WPM", "RGLD", "KGC", "AGI", "BTG", "HMY"],
    ),
    (
        "GDXJ",
        &["AGI", "BTG", "KGC", "HMY", "CDE", "MAG"],
    ),
    (
        "SIL",
        &["PAAS", "AG", "HL", "CDE", "MAG", "WPM"],
    ),
    (
        "SILJ",
        &["AG", "CDE", "MAG", "HL"],
    ),
    (
        "COPX",
        &["FCX", "SCCO", "TECK"],
    ),
    (
        "SMH",
        &[
            "NVDA", "AMD", "AVGO", "INTC", "QCOM", "TXN", "MU", "MRVL", "LRCX", "AMAT", "KLAC",
            "ON", "TSM",
        ],
    ),
    (
        "QQQ",
        &[
            "AAPL", "MSFT", "NVDA", "AMZN", "META", "GOOGL", "GOOG", "AVGO", "TSLA", "COST",
            "NFLX", "AMD", "ADBE", "PEP", "INTC", "QCOM",
        ],
    ),
];

/// Check if a pair is excluded by the ETF-component rule.
///
/// Returns `true` if the pair should be REJECTED (one is ETF, other is component).
pub fn is_etf_component_pair(sym_a: &str, sym_b: &str) -> bool {
    for &(etf, components) in ETF_COMPONENTS {
        if etf == sym_a && components.contains(&sym_b) {
            return true;
        }
        if etf == sym_b && components.contains(&sym_a) {
            return true;
        }
    }
    false
}

/// List all known ETF symbols.
pub fn known_etfs() -> &'static [&'static str] {
    &[
        "XLF", "XLE", "XLK", "XLV", "XLY", "XLP", "XLU", "XLI", "XLB", "XLRE", "GLD", "SLV",
        "IAU", "SGOL", "GDX", "GDXJ", "SIL", "SILJ", "COPX", "PPLT", "PALL",
        "URNM", "URNJ", "SLX", "XME", "PICK",
        "SMH", "QQQ", "SPY", "IWM", "DIA", "EEM", "TLT", "HYG", "LQD", "XBI",
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
