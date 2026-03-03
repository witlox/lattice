//! `lattice nodes` — list and inspect cluster nodes.

use clap::Args;

use crate::client::LatticeGrpcClient;
use crate::convert::{build_list_nodes_request, node_status_to_info};
use crate::output::{OutputFormat, TableRow};

/// Arguments for the nodes command.
#[derive(Args, Debug)]
pub struct NodesArgs {
    /// Node ID to inspect (omit for list)
    pub id: Option<String>,

    /// Filter by state
    #[arg(long)]
    pub state: Option<String>,

    /// Filter by dragonfly group
    #[arg(long)]
    pub group: Option<u32>,
}

/// Node info for display.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub id: String,
    pub state: String,
    pub group: u32,
    pub gpu_type: String,
    pub gpu_count: u32,
    pub cpu_cores: u32,
    pub memory_gb: u64,
    pub owner: String,
}

impl NodeInfo {
    pub fn to_table_row(&self) -> TableRow {
        TableRow {
            cells: vec![
                self.id.clone(),
                self.state.clone(),
                self.group.to_string(),
                self.gpu_type.clone(),
                self.gpu_count.to_string(),
            ],
        }
    }

    pub fn to_wide_row(&self) -> TableRow {
        TableRow {
            cells: vec![
                self.id.clone(),
                self.state.clone(),
                self.group.to_string(),
                self.gpu_type.clone(),
                self.gpu_count.to_string(),
                self.cpu_cores.to_string(),
                format!("{}GB", self.memory_gb),
                self.owner.clone(),
            ],
        }
    }
}

pub fn table_headers() -> Vec<&'static str> {
    vec!["ID", "STATE", "GROUP", "GPU_TYPE", "GPUS"]
}

pub fn wide_headers() -> Vec<&'static str> {
    vec![
        "ID", "STATE", "GROUP", "GPU_TYPE", "GPUS", "CPUS", "MEMORY", "OWNER",
    ]
}

pub fn format_nodes(nodes: &[NodeInfo], format: OutputFormat) -> String {
    match format {
        OutputFormat::Table => {
            let rows: Vec<TableRow> = nodes.iter().map(|n| n.to_table_row()).collect();
            crate::output::render_table(&table_headers(), &rows)
        }
        OutputFormat::Wide => {
            let rows: Vec<TableRow> = nodes.iter().map(|n| n.to_wide_row()).collect();
            crate::output::render_table(&wide_headers(), &rows)
        }
        OutputFormat::Json => {
            let items: Vec<serde_json::Value> = nodes
                .iter()
                .map(|n| {
                    serde_json::json!({
                        "id": n.id,
                        "state": n.state,
                        "group": n.group,
                        "gpu_type": n.gpu_type,
                        "gpu_count": n.gpu_count,
                        "cpu_cores": n.cpu_cores,
                        "memory_gb": n.memory_gb,
                        "owner": n.owner,
                    })
                })
                .collect();
            crate::output::render_json(&items)
        }
    }
}

/// Execute the nodes command: list or inspect nodes via gRPC.
pub async fn execute(
    args: &NodesArgs,
    client: &mut LatticeGrpcClient,
    format: OutputFormat,
) -> anyhow::Result<()> {
    if let Some(ref id) = args.id {
        let status = client.get_node(id).await?;
        let info = node_status_to_info(&status);
        let output = format_nodes(&[info], format);
        print!("{output}");
    } else {
        let req = build_list_nodes_request(args);
        let resp = client.list_nodes(req).await?;
        let infos: Vec<NodeInfo> = resp.nodes.iter().map(node_status_to_info).collect();
        let output = format_nodes(&infos, format);
        print!("{output}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_nodes() -> Vec<NodeInfo> {
        vec![
            NodeInfo {
                id: "x1000c0s0b0n0".to_string(),
                state: "ready".to_string(),
                group: 0,
                gpu_type: "GH200".to_string(),
                gpu_count: 4,
                cpu_cores: 72,
                memory_gb: 512,
                owner: "-".to_string(),
            },
            NodeInfo {
                id: "x1000c0s0b0n1".to_string(),
                state: "draining".to_string(),
                group: 0,
                gpu_type: "GH200".to_string(),
                gpu_count: 4,
                cpu_cores: 72,
                memory_gb: 512,
                owner: "physics/training".to_string(),
            },
        ]
    }

    #[test]
    fn nodes_table_format() {
        let output = format_nodes(&sample_nodes(), OutputFormat::Table);
        assert!(output.contains("ID"));
        assert!(output.contains("x1000c0s0b0n0"));
        assert!(output.contains("ready"));
    }

    #[test]
    fn nodes_wide_has_owner() {
        let output = format_nodes(&sample_nodes(), OutputFormat::Wide);
        assert!(output.contains("OWNER"));
        assert!(output.contains("MEMORY"));
        assert!(output.contains("physics/training"));
    }

    #[test]
    fn nodes_json() {
        let output = format_nodes(&sample_nodes(), OutputFormat::Json);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["gpu_count"], 4);
    }
}
