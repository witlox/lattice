//! Integration tests for the lattice-cli crate.
//!
//! Tests client configuration, output formatting, and error handling
//! at the crate boundary level.

use std::time::Duration;

use lattice_cli::client::{ClientConfig, LatticeGrpcClient};
use lattice_cli::output::{render_json, render_table, OutputFormat, TableRow};

// ─── Test 1: ClientConfig defaults ───────────────────────────
// Verify sensible defaults for endpoint, timeout, and user detection.

#[test]
fn client_config_defaults() {
    let config = ClientConfig::default();
    assert_eq!(config.api_endpoint, "http://localhost:50051");
    assert_eq!(config.timeout_secs, 30);
    assert_eq!(config.timeout(), Duration::from_secs(30));
    // User should be detected from environment or fallback to "anonymous"
    assert!(
        !config.user.is_empty(),
        "user should be detected or fallback"
    );
    assert!(config.tenant.is_none());
    assert!(config.vcluster.is_none());
}

// ─── Test 2: Output format parsing ───────────────────────────
// Parse various format strings, including case variations.

#[test]
fn output_format_parsing() {
    // Exact matches
    assert_eq!(OutputFormat::parse("json"), OutputFormat::Json);
    assert_eq!(OutputFormat::parse("wide"), OutputFormat::Wide);
    assert_eq!(OutputFormat::parse("table"), OutputFormat::Table);

    // Case insensitivity
    assert_eq!(OutputFormat::parse("JSON"), OutputFormat::Json);
    assert_eq!(OutputFormat::parse("WIDE"), OutputFormat::Wide);
    assert_eq!(OutputFormat::parse("Table"), OutputFormat::Table);

    // Unknown defaults to Table
    assert_eq!(OutputFormat::parse("unknown"), OutputFormat::Table);
    assert_eq!(OutputFormat::parse("csv"), OutputFormat::Table);
    assert_eq!(OutputFormat::parse(""), OutputFormat::Table);

    // Default trait
    assert_eq!(OutputFormat::default(), OutputFormat::Table);
}

// ─── Test 3: Table rendering with headers and alignment ──────
// Verify correct header line, separator, and column alignment.

#[test]
fn table_rendering_headers_and_alignment() {
    let rows = vec![
        TableRow {
            cells: vec![
                "abc-123-long-id".to_string(),
                "running".to_string(),
                "physics".to_string(),
            ],
        },
        TableRow {
            cells: vec![
                "def-456".to_string(),
                "pending".to_string(),
                "biology".to_string(),
            ],
        },
    ];

    let result = render_table(&["ID", "STATE", "TENANT"], &rows);
    let lines: Vec<&str> = result.lines().collect();

    // Should have header + separator + 2 rows = 4 lines
    assert_eq!(lines.len(), 4, "should have header + separator + 2 rows");

    // Header should contain all column names
    assert!(lines[0].contains("ID"));
    assert!(lines[0].contains("STATE"));
    assert!(lines[0].contains("TENANT"));

    // Separator line should consist of dashes
    assert!(lines[1].contains("---"));

    // Data rows should contain cell values
    assert!(lines[2].contains("abc-123-long-id"));
    assert!(lines[2].contains("running"));
    assert!(lines[3].contains("def-456"));
    assert!(lines[3].contains("pending"));
    assert!(lines[3].contains("biology"));

    // Column widths should accommodate the longest value
    // "abc-123-long-id" (15 chars) > "ID" (2 chars) → column should be 15+ wide
    let id_col_width = lines[0].find("STATE").unwrap();
    assert!(
        id_col_width >= 15,
        "ID column should be wide enough for longest value"
    );

    // Empty table should show "No results."
    let empty = render_table(&["ID", "STATE"], &[]);
    assert_eq!(empty, "No results.");
}

// ─── Test 4: JSON rendering produces valid output ────────────
// Render items and verify the output parses back as valid JSON.

#[test]
fn json_rendering_valid_output() {
    let items = vec![
        serde_json::json!({"id": "abc-123", "state": "running", "tenant": "physics"}),
        serde_json::json!({"id": "def-456", "state": "pending", "tenant": "biology"}),
    ];

    let result = render_json(&items);

    // Should be valid JSON
    let parsed: serde_json::Value = serde_json::from_str(&result).expect("should be valid JSON");

    // Should be an array
    assert!(parsed.is_array());
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 2);

    // Verify fields
    assert_eq!(arr[0]["id"], "abc-123");
    assert_eq!(arr[0]["state"], "running");
    assert_eq!(arr[1]["id"], "def-456");
    assert_eq!(arr[1]["tenant"], "biology");

    // Empty array should produce valid JSON
    let empty_result = render_json::<serde_json::Value>(&[]);
    let parsed_empty: serde_json::Value =
        serde_json::from_str(&empty_result).expect("empty should be valid JSON");
    assert!(parsed_empty.is_array());
    assert_eq!(parsed_empty.as_array().unwrap().len(), 0);
}

// ─── Test 5: Connect to non-existent server returns error ────
// Attempting to connect to an unreachable endpoint should return
// an error, not panic.

#[tokio::test]
async fn connect_to_nonexistent_server_returns_error() {
    let config = ClientConfig {
        api_endpoint: "http://127.0.0.1:1".to_string(), // port 1: unlikely to be listening
        timeout_secs: 1,
        ..Default::default()
    };

    let result = LatticeGrpcClient::connect(&config).await;
    assert!(
        result.is_err(),
        "connecting to a non-existent server should return an error"
    );
}
