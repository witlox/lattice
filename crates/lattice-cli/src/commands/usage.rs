//! `lattice usage` — show GPU-hours and node-hours budget consumption.

use clap::Args;

use crate::client::{ClientConfig, LatticeGrpcClient};
use crate::output::OutputFormat;

/// Arguments for the usage command.
#[derive(Args, Debug)]
pub struct UsageArgs {
    /// Tenant to query (omit for per-user summary across all tenants)
    #[arg(long)]
    pub tenant: Option<String>,

    /// Number of days to look back (default: 90)
    #[arg(long, default_value = "90")]
    pub days: u32,
}

/// Execute the usage command via gRPC.
pub async fn execute(
    args: &UsageArgs,
    client: &mut LatticeGrpcClient,
    config: &ClientConfig,
    format: OutputFormat,
) -> anyhow::Result<()> {
    if let Some(tenant) = args.tenant.as_ref().or(config.tenant.as_ref()) {
        // Tenant-specific usage
        let resp = client.tenant_usage(tenant, args.days).await?;

        match format {
            OutputFormat::Json => {
                let json = serde_json::json!({
                    "tenant": resp.tenant,
                    "gpu_hours_used": resp.gpu_hours_used,
                    "gpu_hours_budget": resp.gpu_hours_budget,
                    "gpu_fraction_used": resp.gpu_fraction_used,
                    "node_hours_used": resp.node_hours_used,
                    "node_hours_budget": resp.node_hours_budget,
                    "node_fraction_used": resp.node_fraction_used,
                    "period_start": resp.period_start,
                    "period_end": resp.period_end,
                });
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
            _ => {
                println!("Tenant:      {}", resp.tenant);
                println!(
                    "Period:      {} to {}",
                    &resp.period_start[..10],
                    &resp.period_end[..10]
                );
                println!(
                    "Node-hours:  {:.1} / {}",
                    resp.node_hours_used,
                    resp.node_hours_budget
                        .map(|b| format!("{:.1}", b))
                        .unwrap_or_else(|| "unlimited".into())
                );
                if let Some(frac) = resp.node_fraction_used {
                    println!("  Used:      {:.1}% {}", frac * 100.0, budget_bar(frac));
                }
                println!(
                    "GPU-hours:   {:.1} / {}",
                    resp.gpu_hours_used,
                    resp.gpu_hours_budget
                        .map(|b| format!("{:.1}", b))
                        .unwrap_or_else(|| "unlimited".into())
                );
                if let Some(frac) = resp.gpu_fraction_used {
                    println!("  Used:      {:.1}% {}", frac * 100.0, budget_bar(frac));
                }
            }
        }
    } else {
        // Per-user usage across all tenants
        let resp = client.user_usage(&config.user, args.days).await?;

        match format {
            OutputFormat::Json => {
                let json = serde_json::json!({
                    "tenants": resp.tenants.iter().map(|t| serde_json::json!({
                        "tenant": t.tenant,
                        "gpu_hours_used": t.gpu_hours_used,
                        "gpu_hours_budget": t.gpu_hours_budget,
                        "node_hours_used": t.node_hours_used,
                        "node_hours_budget": t.node_hours_budget,
                    })).collect::<Vec<_>>(),
                    "total_gpu_hours": resp.total_gpu_hours,
                    "period_start": resp.period_start,
                    "period_end": resp.period_end,
                });
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
            _ => {
                let headers = vec!["TENANT", "NODE-HRS", "NODE-BUDGET", "GPU-HRS", "GPU-BUDGET"];
                let rows: Vec<crate::output::TableRow> = resp
                    .tenants
                    .iter()
                    .map(|t| crate::output::TableRow {
                        cells: vec![
                            t.tenant.clone(),
                            format!("{:.1}", t.node_hours_used),
                            t.node_hours_budget
                                .map(|b| format!("{:.1}", b))
                                .unwrap_or_else(|| "-".into()),
                            format!("{:.1}", t.gpu_hours_used),
                            t.gpu_hours_budget
                                .map(|b| format!("{:.1}", b))
                                .unwrap_or_else(|| "-".into()),
                        ],
                    })
                    .collect();
                let table = crate::output::render_table(&headers, &rows);
                println!(
                    "Period: {} to {}",
                    &resp.period_start[..10],
                    &resp.period_end[..10]
                );
                print!("{table}");
                println!("Total:  {:.1} GPU-hours", resp.total_gpu_hours);
            }
        }
    }

    Ok(())
}

fn budget_bar(fraction: f64) -> String {
    let width: usize = 20;
    let filled = ((fraction.min(1.0)) * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);
    let indicator = if fraction > 1.0 { "!" } else { "" };
    format!("[{}{}]{}", "#".repeat(filled), ".".repeat(empty), indicator)
}
