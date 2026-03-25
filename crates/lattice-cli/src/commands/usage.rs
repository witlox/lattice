//! `lattice usage` — show GPU-hours budget consumption.

use clap::Args;
use serde::Deserialize;

use crate::client::ClientConfig;
use crate::output::{OutputFormat, TableRow};

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

#[derive(Deserialize)]
struct TenantUsageResponse {
    tenant: String,
    gpu_hours_used: f64,
    gpu_hours_budget: Option<f64>,
    fraction_used: Option<f64>,
    period_start: String,
    period_end: String,
    #[allow(dead_code)]
    period_days: u32,
}

#[derive(Deserialize)]
struct UserUsageResponse {
    #[allow(dead_code)]
    user: String,
    tenants: Vec<UserTenantUsage>,
    total_gpu_hours: f64,
    period_start: String,
    period_end: String,
}

#[derive(Deserialize)]
struct UserTenantUsage {
    tenant: String,
    gpu_hours_used: f64,
    gpu_hours_budget: Option<f64>,
    fraction_used: Option<f64>,
}

fn rest_base_url(grpc_endpoint: &str) -> String {
    // The gRPC endpoint is typically on port 50051, REST on 8080.
    // Convention: replace port 50051 with 8080, or use the same host.
    let base = grpc_endpoint
        .replace(":50051", ":8080")
        .replace("grpc://", "http://");
    // Strip trailing slash
    base.trim_end_matches('/').to_string()
}

/// Execute the usage command.
pub async fn execute(
    args: &UsageArgs,
    config: &ClientConfig,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let base = rest_base_url(&config.api_endpoint);
    let client = reqwest::Client::new();

    if let Some(ref tenant) = args.tenant.as_ref().or(config.tenant.as_ref()) {
        // Tenant-specific usage
        let url = format!("{}/api/v1/tenants/{}/usage?days={}", base, tenant, args.days);
        let mut req = client.get(&url);
        if let Some(ref token) = config.token {
            req = req.bearer_auth(token);
        }
        let resp: TenantUsageResponse = req.send().await?.json().await?;

        match format {
            OutputFormat::Json => {
                let json = serde_json::json!({
                    "tenant": resp.tenant,
                    "gpu_hours_used": resp.gpu_hours_used,
                    "gpu_hours_budget": resp.gpu_hours_budget,
                    "fraction_used": resp.fraction_used,
                    "period_start": resp.period_start,
                    "period_end": resp.period_end,
                });
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
            _ => {
                println!("Tenant:     {}", resp.tenant);
                println!("Period:     {} to {}", &resp.period_start[..10], &resp.period_end[..10]);
                println!(
                    "GPU-hours:  {:.1} / {}",
                    resp.gpu_hours_used,
                    resp.gpu_hours_budget
                        .map(|b| format!("{:.1}", b))
                        .unwrap_or_else(|| "unlimited".into())
                );
                if let Some(frac) = resp.fraction_used {
                    let pct = frac * 100.0;
                    let bar = budget_bar(frac);
                    println!("Used:       {:.1}% {}", pct, bar);
                }
            }
        }
    } else {
        // Per-user usage across all tenants
        let url = format!(
            "{}/api/v1/usage?user={}&days={}",
            base, config.user, args.days
        );
        let mut req = client.get(&url);
        if let Some(ref token) = config.token {
            req = req.bearer_auth(token);
        }
        let resp: UserUsageResponse = req.send().await?.json().await?;

        match format {
            OutputFormat::Json => {
                let json = serde_json::json!({
                    "tenants": resp.tenants.iter().map(|t| serde_json::json!({
                        "tenant": t.tenant,
                        "gpu_hours_used": t.gpu_hours_used,
                        "gpu_hours_budget": t.gpu_hours_budget,
                        "fraction_used": t.fraction_used,
                    })).collect::<Vec<_>>(),
                    "total_gpu_hours": resp.total_gpu_hours,
                    "period_start": resp.period_start,
                    "period_end": resp.period_end,
                });
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
            _ => {
                let headers = vec!["TENANT", "GPU-HOURS", "BUDGET", "USED"];
                let rows: Vec<TableRow> = resp
                    .tenants
                    .iter()
                    .map(|t| TableRow {
                        cells: vec![
                            t.tenant.clone(),
                            format!("{:.1}", t.gpu_hours_used),
                            t.gpu_hours_budget
                                .map(|b| format!("{:.1}", b))
                                .unwrap_or_else(|| "-".into()),
                            t.fraction_used
                                .map(|f| format!("{:.1}%", f * 100.0))
                                .unwrap_or_else(|| "-".into()),
                        ],
                    })
                    .collect();
                let table = crate::output::render_table(&headers, &rows);
                println!("Period: {} to {}", &resp.period_start[..10], &resp.period_end[..10]);
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
