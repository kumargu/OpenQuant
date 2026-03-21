//! Stock relationship graph for pair universe filtering.
//!
//! Only screen pairs between graph-connected stocks. Two stocks with no
//! graph edge don't get screened, regardless of statistical properties.
//! This eliminates spurious correlations from blind N-choose-2 screening.
//!
//! The graph is a curated adjacency list (~200 lines of JSON) capturing:
//! - Competitor relationships (GS/MS, V/MA, HD/LOW)
//! - Supply chain links (AAPL/TSM, NVDA/TSM)
//! - Same-regulator pairs (Fed-supervised banks)
//! - Same-factor exposure (oil-sensitive, rate-sensitive)

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use tracing::{info, warn};

/// A relationship edge between two stocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub a: String,
    pub b: String,
    #[serde(rename = "type")]
    pub edge_type: String,
    #[serde(default)]
    pub sector: String,
    #[serde(default)]
    pub note: String,
}

/// The stock relationship graph file format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipGraphFile {
    pub nodes: Vec<String>,
    pub edges: Vec<Edge>,
}

/// In-memory relationship graph for fast pair lookups.
pub struct RelationshipGraph {
    /// All nodes (symbols).
    pub nodes: HashSet<String>,
    /// Adjacency set: for each node, the set of connected nodes.
    adjacency: HashMap<String, HashSet<String>>,
    /// Edge metadata by canonical pair key.
    edge_info: HashMap<(String, String), Edge>,
    /// Total edge count.
    pub edge_count: usize,
}

impl RelationshipGraph {
    /// Load from JSON file.
    pub fn load(path: &Path) -> Option<Self> {
        let contents = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to read relationship graph");
                return None;
            }
        };

        let file: RelationshipGraphFile = match serde_json::from_str(&contents) {
            Ok(f) => f,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to parse relationship graph");
                return None;
            }
        };

        Some(Self::from_file(file))
    }

    /// Build from parsed file.
    pub fn from_file(file: RelationshipGraphFile) -> Self {
        let nodes: HashSet<String> = file.nodes.into_iter().collect();
        let mut adjacency: HashMap<String, HashSet<String>> = HashMap::new();
        let mut edge_info: HashMap<(String, String), Edge> = HashMap::new();

        for edge in &file.edges {
            adjacency
                .entry(edge.a.clone())
                .or_default()
                .insert(edge.b.clone());
            adjacency
                .entry(edge.b.clone())
                .or_default()
                .insert(edge.a.clone());

            let key = canonical_key(&edge.a, &edge.b);
            edge_info.insert(key, edge.clone());
        }

        let edge_count = file.edges.len();

        info!(
            nodes = nodes.len(),
            edges = edge_count,
            "Loaded relationship graph"
        );

        Self {
            nodes,
            adjacency,
            edge_info,
            edge_count,
        }
    }

    /// Check if two stocks are connected in the graph.
    pub fn are_connected(&self, a: &str, b: &str) -> bool {
        self.adjacency
            .get(a)
            .is_some_and(|neighbors| neighbors.contains(b))
    }

    /// Get edge info for a pair (if connected).
    pub fn edge(&self, a: &str, b: &str) -> Option<&Edge> {
        let key = canonical_key(a, b);
        self.edge_info.get(&key)
    }

    /// Get all neighbors of a node.
    pub fn neighbors(&self, symbol: &str) -> Option<&HashSet<String>> {
        self.adjacency.get(symbol)
    }

    /// Generate all connected pairs (for screening).
    /// Returns canonical (a, b) pairs where a < b.
    pub fn connected_pairs(&self) -> Vec<(String, String)> {
        self.edge_info.keys().cloned().collect()
    }

    /// Filter a list of candidate pairs to only graph-connected ones.
    pub fn filter_connected(&self, pairs: &[(String, String)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .filter(|(a, b)| self.are_connected(a, b))
            .cloned()
            .collect()
    }
}

/// Canonical key for edge lookup (alphabetical order).
fn canonical_key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_graph() -> RelationshipGraphFile {
        RelationshipGraphFile {
            nodes: vec![
                "GS".into(),
                "MS".into(),
                "JPM".into(),
                "C".into(),
                "NVDA".into(),
                "TSM".into(),
                "AAPL".into(),
            ],
            edges: vec![
                Edge {
                    a: "GS".into(),
                    b: "MS".into(),
                    edge_type: "competitor".into(),
                    sector: "financials".into(),
                    note: "Investment banks".into(),
                },
                Edge {
                    a: "JPM".into(),
                    b: "C".into(),
                    edge_type: "competitor".into(),
                    sector: "financials".into(),
                    note: "Universal banks".into(),
                },
                Edge {
                    a: "NVDA".into(),
                    b: "TSM".into(),
                    edge_type: "supply_chain".into(),
                    sector: "semis".into(),
                    note: "TSM fabs NVDA chips".into(),
                },
            ],
        }
    }

    #[test]
    fn test_connected_pairs() {
        let graph = RelationshipGraph::from_file(sample_graph());
        assert!(graph.are_connected("GS", "MS"));
        assert!(graph.are_connected("MS", "GS")); // bidirectional
        assert!(graph.are_connected("NVDA", "TSM"));
        assert!(!graph.are_connected("GS", "NVDA")); // no edge
        assert!(!graph.are_connected("AAPL", "MS")); // no edge
    }

    #[test]
    fn test_edge_info() {
        let graph = RelationshipGraph::from_file(sample_graph());
        let edge = graph.edge("GS", "MS").unwrap();
        assert_eq!(edge.edge_type, "competitor");
        assert_eq!(edge.sector, "financials");

        // Reversed order should also work
        let edge = graph.edge("MS", "GS").unwrap();
        assert_eq!(edge.edge_type, "competitor");
    }

    #[test]
    fn test_neighbors() {
        let graph = RelationshipGraph::from_file(sample_graph());
        let gs_neighbors = graph.neighbors("GS").unwrap();
        assert!(gs_neighbors.contains("MS"));
        assert!(!gs_neighbors.contains("NVDA"));
    }

    #[test]
    fn test_connected_pairs_list() {
        let graph = RelationshipGraph::from_file(sample_graph());
        let pairs = graph.connected_pairs();
        assert_eq!(pairs.len(), 3);
    }

    #[test]
    fn test_filter_connected() {
        let graph = RelationshipGraph::from_file(sample_graph());
        let candidates = vec![
            ("GS".into(), "MS".into()),    // connected
            ("GS".into(), "NVDA".into()),  // not connected
            ("NVDA".into(), "TSM".into()), // connected
        ];
        let filtered = graph.filter_connected(&candidates);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_load_from_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("relationships.json");

        let file = sample_graph();
        let json = serde_json::to_string_pretty(&file).unwrap();
        fs::write(&path, json).unwrap();

        let graph = RelationshipGraph::load(&path).unwrap();
        assert_eq!(graph.nodes.len(), 7);
        assert_eq!(graph.edge_count, 3);
        assert!(graph.are_connected("GS", "MS"));
    }

    #[test]
    fn test_load_missing_file() {
        let result = RelationshipGraph::load(Path::new("/nonexistent.json"));
        assert!(result.is_none());
    }

    #[test]
    fn test_proven_pairs_are_connected() {
        // Verify that our proven hardcoded pairs appear in the full graph
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("data/stock_relationships.json");

        if !path.exists() {
            return; // skip if running outside repo
        }

        let graph = RelationshipGraph::load(&path).unwrap();
        // These are the validated OOS pairs from pairs/mod.rs
        assert!(graph.are_connected("GS", "MS"), "GS/MS should be connected");
        assert!(graph.are_connected("C", "JPM"), "C/JPM should be connected");
        assert!(
            graph.are_connected("GLD", "SLV"),
            "GLD/SLV should be connected"
        );
        assert!(
            graph.are_connected("COIN", "PLTR"),
            "COIN/PLTR should be connected"
        );
    }
}
