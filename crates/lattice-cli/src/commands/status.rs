//! `lattice status` — query allocation status.

use clap::Args;

use crate::client::{ClientConfig, LatticeGrpcClient};
use crate::convert::{allocation_status_to_info, build_list_request};
use crate::output::{OutputFormat, TableRow};

/// Arguments for the status command.
#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Allocation ID to query (omit for list)
    pub id: Option<String>,

    /// Filter by state
    #[arg(long)]
    pub state: Option<String>,

    /// Filter by user
    #[arg(long)]
    pub user: Option<String>,

    /// Show only own allocations
    #[arg(long)]
    pub mine: bool,
}

/// Allocation info for display.
#[derive(Debug, Clone)]
pub struct AllocationInfo {
    pub id: String,
    pub tenant: String,
    pub project: String,
    pub state: String,
    pub nodes: u32,
    pub walltime: String,
    pub elapsed: String,
    pub vcluster: String,
    pub user: String,
    pub gpu_type: String,
}

impl AllocationInfo {
    /// Convert to a standard table row.
    pub fn to_table_row(&self) -> TableRow {
        TableRow {
            cells: vec![
                self.id.clone(),
                self.project.clone(),
                self.state.clone(),
                self.nodes.to_string(),
                self.walltime.clone(),
                self.elapsed.clone(),
                self.vcluster.clone(),
            ],
        }
    }

    /// Convert to a wide table row (with extra columns).
    pub fn to_wide_row(&self) -> TableRow {
        TableRow {
            cells: vec![
                self.id.clone(),
                self.project.clone(),
                self.state.clone(),
                self.nodes.to_string(),
                self.walltime.clone(),
                self.elapsed.clone(),
                self.vcluster.clone(),
                self.tenant.clone(),
                self.user.clone(),
                self.gpu_type.clone(),
            ],
        }
    }
}

/// Standard table headers for allocation listing.
pub fn table_headers() -> Vec<&'static str> {
    vec![
        "ID", "NAME", "STATE", "NODES", "WALLTIME", "ELAPSED", "VCLUSTER",
    ]
}

/// Wide table headers.
pub fn wide_headers() -> Vec<&'static str> {
    vec![
        "ID", "NAME", "STATE", "NODES", "WALLTIME", "ELAPSED", "VCLUSTER", "TENANT", "USER",
        "GPU_TYPE",
    ]
}

/// Format allocation list for display.
pub fn format_allocations(allocations: &[AllocationInfo], format: OutputFormat) -> String {
    match format {
        OutputFormat::Table => {
            let rows: Vec<TableRow> = allocations.iter().map(|a| a.to_table_row()).collect();
            crate::output::render_table(&table_headers(), &rows)
        }
        OutputFormat::Wide => {
            let rows: Vec<TableRow> = allocations.iter().map(|a| a.to_wide_row()).collect();
            crate::output::render_table(&wide_headers(), &rows)
        }
        OutputFormat::Json => {
            let items: Vec<serde_json::Value> = allocations
                .iter()
                .map(|a| {
                    serde_json::json!({
                        "id": a.id,
                        "tenant": a.tenant,
                        "project": a.project,
                        "state": a.state,
                        "nodes": a.nodes,
                        "walltime": a.walltime,
                        "elapsed": a.elapsed,
                        "vcluster": a.vcluster,
                        "user": a.user,
                    })
                })
                .collect();
            crate::output::render_json(&items)
        }
    }
}

/// Execute the status command: either get a single allocation or list.
pub async fn execute(
    args: &StatusArgs,
    client: &mut LatticeGrpcClient,
    config: &ClientConfig,
    format: OutputFormat,
) -> anyhow::Result<()> {
    if let Some(ref id) = args.id {
        let status = client.get_allocation(id).await?;
        let info = allocation_status_to_info(&status);
        let output = format_allocations(&[info], format);
        print!("{output}");
    } else {
        let user_filter = if args.mine {
            Some(config.user.as_str())
        } else {
            args.user.as_deref()
        };
        let req = build_list_request(
            user_filter,
            config.tenant.as_deref(),
            config.vcluster.as_deref(),
            args.state.as_deref(),
        );
        let resp = client.list_allocations(req).await?;
        let infos: Vec<AllocationInfo> = resp
            .allocations
            .iter()
            .map(allocation_status_to_info)
            .collect();
        let output = format_allocations(&infos, format);
        print!("{output}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_allocations() -> Vec<AllocationInfo> {
        vec![
            AllocationInfo {
                id: "abc-123".to_string(),
                tenant: "physics".to_string(),
                project: "training".to_string(),
                state: "running".to_string(),
                nodes: 4,
                walltime: "2:00:00".to_string(),
                elapsed: "0:45:12".to_string(),
                vcluster: "hpc-batch".to_string(),
                user: "alice".to_string(),
                gpu_type: "GH200".to_string(),
            },
            AllocationInfo {
                id: "def-456".to_string(),
                tenant: "ml".to_string(),
                project: "inference".to_string(),
                state: "pending".to_string(),
                nodes: 8,
                walltime: "24:00:00".to_string(),
                elapsed: "-".to_string(),
                vcluster: "service".to_string(),
                user: "bob".to_string(),
                gpu_type: "MI300X".to_string(),
            },
        ]
    }

    #[test]
    fn table_format() {
        let output = format_allocations(&sample_allocations(), OutputFormat::Table);
        assert!(output.contains("ID"));
        assert!(output.contains("abc-123"));
        assert!(output.contains("running"));
        assert!(output.contains("pending"));
    }

    #[test]
    fn wide_format_has_extra_columns() {
        let output = format_allocations(&sample_allocations(), OutputFormat::Wide);
        assert!(output.contains("TENANT"));
        assert!(output.contains("USER"));
        assert!(output.contains("GPU_TYPE"));
        assert!(output.contains("physics"));
        assert!(output.contains("alice"));
    }

    #[test]
    fn json_format() {
        let output = format_allocations(&sample_allocations(), OutputFormat::Json);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["id"], "abc-123");
    }

    #[test]
    fn empty_list() {
        let output = format_allocations(&[], OutputFormat::Table);
        assert_eq!(output, "No results.");
    }
}
