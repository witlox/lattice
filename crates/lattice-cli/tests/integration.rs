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

// ═══════════════════════════════════════════════════════════════
// Slurm Compat Integration Tests
// ═══════════════════════════════════════════════════════════════

use lattice_cli::compat::*;
use lattice_cli::config_file::*;
use lattice_cli::convert::*;

// ─── Test 6: Parse basic SBATCH directives ───────────────────
// Verify nodes, walltime, partition, and entrypoint from a typical script.

#[test]
fn parse_sbatch_basic_directives() {
    let script = r#"#!/bin/bash
#SBATCH --nodes=4
#SBATCH --time=01:00:00
#SBATCH --partition=gpu
#SBATCH --account=research
#SBATCH --job-name=inference

srun ./model_serve
"#;
    let d = parse_sbatch_script(script);
    assert_eq!(d.nodes, Some(4));
    assert_eq!(d.walltime, Some(Duration::from_secs(3600)));
    assert_eq!(d.partition.as_deref(), Some("gpu"));
    assert_eq!(d.account.as_deref(), Some("research"));
    assert_eq!(d.job_name.as_deref(), Some("inference"));
    assert_eq!(d.entrypoint, "srun ./model_serve");
}

// ─── Test 7: Parse GRES and array directives ────────────────
// Verify gpu count parsing and array spec extraction.

#[test]
fn parse_sbatch_gres_and_array() {
    let script = r#"#!/bin/bash
#SBATCH --gres=gpu:4
#SBATCH --array=0-99%20
#SBATCH --nodes=2

./train.sh
"#;
    let d = parse_sbatch_script(script);
    assert_eq!(d.gres.as_deref(), Some("gpu:4"));
    assert_eq!(parse_gres_gpu(d.gres.as_deref().unwrap()), Some(4));
    assert_eq!(d.array.as_deref(), Some("0-99%20"));
    let (start, end, step, max_conc) = parse_array_spec(d.array.as_deref().unwrap()).unwrap();
    assert_eq!(start, 0);
    assert_eq!(end, 99);
    assert_eq!(step, 1);
    assert_eq!(max_conc, 20);
}

// ─── Test 8: Parse dependency directives ─────────────────────
// Verify dependency parsing with multiple conditions.

#[test]
fn parse_sbatch_dependency() {
    let script = r#"#!/bin/bash
#SBATCH --dependency=afterok:123,afternotok:456

./eval.sh
"#;
    let d = parse_sbatch_script(script);
    assert_eq!(d.dependency.as_deref(), Some("afterok:123,afternotok:456"));
    let deps = parse_dependency(d.dependency.as_deref().unwrap());
    assert_eq!(deps.len(), 2);
    assert_eq!(deps[0], ("123".to_string(), "success".to_string()));
    assert_eq!(deps[1], ("456".to_string(), "failure".to_string()));
}

// ─── Test 9: Slurm time format parsing ──────────────────────
// Test all four time formats: D-HH:MM:SS, HH:MM:SS, HH:MM, minutes.

#[test]
fn parse_slurm_time_formats() {
    // D-HH:MM:SS
    assert_eq!(
        parse_slurm_time("2-12:30:45"),
        Some(Duration::from_secs(2 * 86400 + 12 * 3600 + 30 * 60 + 45))
    );
    // HH:MM:SS
    assert_eq!(
        parse_slurm_time("01:00:00"),
        Some(Duration::from_secs(3600))
    );
    // HH:MM (treated as HH:MM for Slurm compat)
    assert_eq!(
        parse_slurm_time("02:30"),
        Some(Duration::from_secs(2 * 3600 + 30 * 60))
    );
    // Minutes only
    assert_eq!(parse_slurm_time("90"), Some(Duration::from_secs(5400)));
    // Edge case: zero
    assert_eq!(parse_slurm_time("0"), Some(Duration::from_secs(0)));
    // Edge case: large day count
    assert_eq!(
        parse_slurm_time("7-00:00:00"),
        Some(Duration::from_secs(7 * 86400))
    );
}

// ═══════════════════════════════════════════════════════════════
// Convert Module Integration Tests
// ═══════════════════════════════════════════════════════════════

// ─── Test 10: Duration formatting for various values ─────────

#[test]
fn format_duration_hms_various() {
    assert_eq!(format_duration_hms(0), "0:00:00");
    assert_eq!(format_duration_hms(59), "0:00:59");
    assert_eq!(format_duration_hms(60), "0:01:00");
    assert_eq!(format_duration_hms(3600), "1:00:00");
    assert_eq!(format_duration_hms(3661), "1:01:01");
    assert_eq!(format_duration_hms(86399), "23:59:59");
    assert_eq!(format_duration_hms(90061), "25:01:01");
}

// ─── Test 11: Build submit request populates fields ──────────

#[test]
fn build_submit_request_populates_fields() {
    use lattice_cli::commands::submit::SubmitDescription;

    let desc = SubmitDescription {
        tenant: Some("bio-lab".to_string()),
        project: Some("genome-seq".to_string()),
        entrypoint: Some("./align.sh".to_string()),
        nodes: Some(8),
        walltime: Some(Duration::from_secs(14400)),
        uenvs: vec!["biotools:2.1".to_string()],
        views: vec!["develop".to_string()],
        priority_class: Some(3),
        dependencies: vec![
            ("job-100".to_string(), "success".to_string()),
            ("job-101".to_string(), "any".to_string()),
        ],
        task_group: None,
        ..Default::default()
    };

    let req = build_submit_request(&desc, "researcher", Some("hpc-batch"));
    match req.submission {
        Some(lattice_common::proto::lattice::v1::submit_request::Submission::Single(spec)) => {
            assert_eq!(spec.tenant, "bio-lab");
            assert_eq!(spec.project, "genome-seq");
            assert_eq!(spec.entrypoint, "./align.sh");
            assert_eq!(spec.vcluster, "hpc-batch");

            let res = spec.resources.unwrap();
            assert_eq!(res.min_nodes, 8);

            let env = spec.environment.unwrap();
            assert_eq!(env.images.len(), 1);
            assert_eq!(env.images[0].spec, "biotools:2.1");

            let lc = spec.lifecycle.unwrap();
            assert_eq!(lc.preemption_class, 3);
            assert_eq!(lc.walltime.unwrap().seconds, 14400);

            assert_eq!(spec.tags.get("user").unwrap(), "researcher");

            assert_eq!(spec.depends_on.len(), 2);
            assert_eq!(spec.depends_on[0].ref_id, "job-100");
            assert_eq!(spec.depends_on[0].condition, "success");
            assert_eq!(spec.depends_on[1].ref_id, "job-101");
            assert_eq!(spec.depends_on[1].condition, "any");
        }
        _ => panic!("expected Single submission"),
    }
}

// ─── Test 12: Build list request with filters ────────────────

#[test]
fn build_list_request_with_filters() {
    let req = build_list_request(
        Some("bob"),
        Some("chemistry"),
        Some("service-bin-pack"),
        Some("pending"),
    );
    assert_eq!(req.user, "bob");
    assert_eq!(req.tenant, "chemistry");
    assert_eq!(req.vcluster, "service-bin-pack");
    assert_eq!(req.state, "pending");
    assert_eq!(req.limit, 100);
    assert!(req.cursor.is_empty());

    // Also verify None produces empty strings
    let req_empty = build_list_request(None, None, None, None);
    assert_eq!(req_empty.user, "");
    assert_eq!(req_empty.tenant, "");
    assert_eq!(req_empty.vcluster, "");
    assert_eq!(req_empty.state, "");
}

// ═══════════════════════════════════════════════════════════════
// Config File Integration Tests
// ═══════════════════════════════════════════════════════════════

// ─── Test 13: Default CliConfig has sensible defaults ────────

#[test]
fn cli_config_default_values() {
    let config = CliConfig::default();
    assert!(config.server_addr.is_none());
    assert!(config.default_tenant.is_none());
    assert!(config.default_vcluster.is_none());
    assert!(config.output_format.is_none());
    // server_addr() should return the fallback
    assert_eq!(config.server_addr(), "localhost:50051");
}

// ─── Test 14: Merge CLI overrides replaces config values ─────

#[test]
fn merge_cli_overrides_replaces() {
    let config = CliConfig {
        server_addr: Some("remote-server:50051".to_string()),
        default_tenant: Some("physics".to_string()),
        default_vcluster: Some("hpc-backfill".to_string()),
        output_format: Some("table".to_string()),
    };

    let merged = config.merge_cli_overrides(
        Some("biology"),     // override tenant
        Some("gpu-cluster"), // override vcluster
        Some("json"),        // override output
    );

    // CLI values win
    assert_eq!(merged.default_tenant.as_deref(), Some("biology"));
    assert_eq!(merged.default_vcluster.as_deref(), Some("gpu-cluster"));
    assert_eq!(merged.output_format.as_deref(), Some("json"));
    // server_addr is not overridden by merge_cli_overrides
    assert_eq!(merged.server_addr.as_deref(), Some("remote-server:50051"));
}

// ─── Test 15: Server addr fallback ──────────────────────────

#[test]
fn server_addr_fallback() {
    // Empty config (no server_addr set) returns localhost:50051
    let config = CliConfig::default();
    assert_eq!(config.server_addr(), "localhost:50051");

    // Config with server_addr set returns that value
    let config_with_addr = CliConfig {
        server_addr: Some("lattice.example.com:50051".to_string()),
        ..Default::default()
    };
    assert_eq!(config_with_addr.server_addr(), "lattice.example.com:50051");

    // Loading from nonexistent file returns default (fallback)
    let loaded = CliConfig::load_from_path("/nonexistent/no/such/file.toml");
    assert_eq!(loaded.server_addr(), "localhost:50051");
}
