//! Proto-to-display conversion helpers.
//!
//! Converts proto messages into CLI-displayable structs and builds
//! proto request messages from CLI arguments.

use std::collections::HashMap;

use lattice_common::proto::lattice::v1 as pb;

use crate::commands::nodes::NodeInfo;
use crate::commands::status::AllocationInfo;

/// Format seconds as HH:MM:SS.
pub fn format_duration_hms(total_secs: u64) -> String {
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    format!("{h}:{m:02}:{s:02}")
}

/// Convert a proto `AllocationStatus` into a CLI-displayable `AllocationInfo`.
pub fn allocation_status_to_info(status: &pb::AllocationStatus) -> AllocationInfo {
    let spec = status.spec.as_ref();
    let resources = spec.and_then(|s| s.resources.as_ref());
    let lifecycle = spec.and_then(|s| s.lifecycle.as_ref());

    let walltime = lifecycle
        .and_then(|l| l.walltime.as_ref())
        .map(|d| format_duration_hms(d.seconds as u64))
        .unwrap_or_else(|| "-".to_string());

    let elapsed = match (status.started_at.as_ref(), status.completed_at.as_ref()) {
        (Some(start), Some(end)) => {
            let secs = (end.seconds - start.seconds).unsigned_abs();
            format_duration_hms(secs)
        }
        (Some(start), None) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let secs = (now - start.seconds).unsigned_abs();
            format_duration_hms(secs)
        }
        _ => "-".to_string(),
    };

    AllocationInfo {
        id: status.allocation_id.clone(),
        tenant: spec.map(|s| s.tenant.clone()).unwrap_or_default(),
        project: spec.map(|s| s.project.clone()).unwrap_or_default(),
        state: status.state.clone(),
        nodes: resources.map(|r| r.min_nodes).unwrap_or(0),
        walltime,
        elapsed,
        vcluster: spec.map(|s| s.vcluster.clone()).unwrap_or_default(),
        user: status.user.clone(),
        gpu_type: resources.map(|r| r.gpu_type.clone()).unwrap_or_default(),
    }
}

/// Convert a proto `NodeStatus` into a CLI-displayable `NodeInfo`.
pub fn node_status_to_info(status: &pb::NodeStatus) -> NodeInfo {
    NodeInfo {
        id: status.node_id.clone(),
        state: status.state.clone(),
        group: status.group,
        gpu_type: status.gpu_type.clone(),
        gpu_count: status.gpu_count,
        cpu_cores: status.cpu_cores,
        memory_gb: status.memory_gb,
        owner: if !status.owner_tenant.is_empty() {
            if !status.owner_allocation.is_empty() {
                format!("{}/{}", status.owner_tenant, status.owner_allocation)
            } else {
                status.owner_tenant.clone()
            }
        } else {
            "-".to_string()
        },
    }
}

/// Build a `SubmitRequest` from a `SubmitDescription`.
pub fn build_submit_request(
    desc: &crate::commands::submit::SubmitDescription,
    user: &str,
    vcluster: Option<&str>,
) -> pb::SubmitRequest {
    let spec = pb::AllocationSpec {
        tenant: desc.tenant.clone().unwrap_or_default(),
        project: desc.project.clone().unwrap_or_default(),
        vcluster: vcluster.unwrap_or_default().to_string(),
        entrypoint: desc.entrypoint.clone().unwrap_or_default(),
        environment: Some(pb::EnvironmentSpec {
            uenv: desc.uenv.clone().unwrap_or_default(),
            view: desc.view.clone().unwrap_or_default(),
            ..Default::default()
        }),
        resources: Some(pb::ResourceSpec {
            min_nodes: desc.nodes.unwrap_or(1),
            ..Default::default()
        }),
        lifecycle: Some(pb::LifecycleSpec {
            r#type: pb::lifecycle_spec::Type::Bounded as i32,
            walltime: desc.walltime.map(|d| prost_types::Duration {
                seconds: d.as_secs() as i64,
                nanos: 0,
            }),
            preemption_class: desc.priority_class.unwrap_or(5),
            ..Default::default()
        }),
        depends_on: desc
            .dependencies
            .iter()
            .map(|(ref_id, condition)| pb::DependencySpec {
                ref_id: ref_id.clone(),
                condition: condition.clone(),
            })
            .collect(),
        tags: {
            let mut tags = HashMap::new();
            tags.insert("user".to_string(), user.to_string());
            tags
        },
        ..Default::default()
    };

    if let Some((start, end, step, max_concurrent)) = desc.task_group {
        pb::SubmitRequest {
            submission: Some(pb::submit_request::Submission::TaskGroup(
                pb::TaskGroupSpec {
                    template: Some(spec),
                    range_start: start,
                    range_end: end,
                    step,
                    max_concurrent,
                },
            )),
        }
    } else {
        pb::SubmitRequest {
            submission: Some(pb::submit_request::Submission::Single(spec)),
        }
    }
}

/// Build a `ListAllocationsRequest` from status command args.
pub fn build_list_request(
    user: Option<&str>,
    tenant: Option<&str>,
    vcluster: Option<&str>,
    state: Option<&str>,
) -> pb::ListAllocationsRequest {
    pb::ListAllocationsRequest {
        tenant: tenant.unwrap_or_default().to_string(),
        user: user.unwrap_or_default().to_string(),
        vcluster: vcluster.unwrap_or_default().to_string(),
        state: state.unwrap_or_default().to_string(),
        limit: 100,
        cursor: String::new(),
    }
}

/// Build a `LogStreamRequest` from logs command args.
pub fn build_log_stream_request(args: &crate::commands::logs::LogsArgs) -> pb::LogStreamRequest {
    let stream = if args.stderr {
        pb::log_stream_request::Stream::Stderr as i32
    } else if args.follow {
        pb::log_stream_request::Stream::All as i32
    } else {
        pb::log_stream_request::Stream::Stdout as i32
    };

    pb::LogStreamRequest {
        allocation_id: args.alloc_id.clone(),
        stream,
        node_id: args.node.clone().unwrap_or_default(),
        follow: args.follow,
        tail_lines: args.tail.unwrap_or(0),
        since: None,
        until: None,
    }
}

/// Build a `WatchRequest` from watch command args.
pub fn build_watch_request(
    alloc_id: &str,
    tenant: Option<&str>,
    vcluster: Option<&str>,
) -> pb::WatchRequest {
    pb::WatchRequest {
        allocation_id: alloc_id.to_string(),
        tenant: tenant.unwrap_or_default().to_string(),
        vcluster: vcluster.unwrap_or_default().to_string(),
    }
}

/// Build a `DiagnosticsRequest` from diag command args.
pub fn build_diagnostics_request(args: &crate::commands::diag::DiagArgs) -> pb::DiagnosticsRequest {
    let scope = if args.show_all() {
        pb::diagnostics_request::Scope::Full as i32
    } else if args.network {
        pb::diagnostics_request::Scope::Network as i32
    } else {
        pb::diagnostics_request::Scope::Storage as i32
    };

    pb::DiagnosticsRequest {
        allocation_id: args.alloc_id.clone(),
        scope,
    }
}

/// Build a `QueryMetricsRequest` for the top command.
pub fn build_query_metrics_request(alloc_id: &str) -> pb::QueryMetricsRequest {
    pb::QueryMetricsRequest {
        allocation_id: alloc_id.to_string(),
        mode: pb::query_metrics_request::MetricsMode::PerNode as i32,
        duration: None,
    }
}

/// Build a `StreamMetricsRequest` for the top command (streaming mode).
pub fn build_stream_metrics_request(alloc_id: &str) -> pb::StreamMetricsRequest {
    pb::StreamMetricsRequest {
        allocation_id: alloc_id.to_string(),
        metrics: vec![],
        alerts_only: false,
    }
}

/// Build a `CreateTenantRequest` from admin tenant-create args.
pub fn build_create_tenant_request(
    args: &crate::commands::admin::TenantCreateArgs,
) -> pb::CreateTenantRequest {
    pb::CreateTenantRequest {
        name: args.name.clone(),
        quota: Some(pb::TenantQuotaSpec {
            max_nodes: args.max_nodes.unwrap_or(0),
            fair_share_target: args.fair_share.unwrap_or(0.0),
            gpu_hours_budget: None,
            max_concurrent_allocations: None,
        }),
        isolation_level: args.isolation.clone(),
    }
}

/// Build a `ListNodesRequest` from nodes command args.
pub fn build_list_nodes_request(args: &crate::commands::nodes::NodesArgs) -> pb::ListNodesRequest {
    pb::ListNodesRequest {
        state: args.state.clone().unwrap_or_default(),
        group: args.group.map(|g| g.to_string()).unwrap_or_default(),
        tenant: String::new(),
        vcluster: String::new(),
        limit: 200,
        cursor: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::submit::SubmitDescription;

    #[test]
    fn build_submit_request_single() {
        let desc = SubmitDescription {
            tenant: Some("physics".to_string()),
            project: Some("training".to_string()),
            entrypoint: Some("python train.py".to_string()),
            nodes: Some(4),
            walltime: Some(std::time::Duration::from_secs(7200)),
            uenv: Some("pytorch:latest".to_string()),
            priority_class: Some(8),
            ..Default::default()
        };

        let req = build_submit_request(&desc, "alice", Some("hpc-batch"));
        match req.submission {
            Some(pb::submit_request::Submission::Single(spec)) => {
                assert_eq!(spec.tenant, "physics");
                assert_eq!(spec.project, "training");
                assert_eq!(spec.entrypoint, "python train.py");
                assert_eq!(spec.vcluster, "hpc-batch");
                let res = spec.resources.unwrap();
                assert_eq!(res.min_nodes, 4);
                let env = spec.environment.unwrap();
                assert_eq!(env.uenv, "pytorch:latest");
                let lc = spec.lifecycle.unwrap();
                assert_eq!(lc.preemption_class, 8);
                assert_eq!(lc.walltime.unwrap().seconds, 7200);
                assert_eq!(spec.tags.get("user").unwrap(), "alice");
            }
            _ => panic!("expected Single submission"),
        }
    }

    #[test]
    fn build_submit_request_task_group() {
        let desc = SubmitDescription {
            tenant: Some("ml".to_string()),
            entrypoint: Some("./run.sh".to_string()),
            nodes: Some(1),
            task_group: Some((0, 99, 1, 20)),
            ..Default::default()
        };

        let req = build_submit_request(&desc, "bob", None);
        match req.submission {
            Some(pb::submit_request::Submission::TaskGroup(tg)) => {
                assert_eq!(tg.range_start, 0);
                assert_eq!(tg.range_end, 99);
                assert_eq!(tg.step, 1);
                assert_eq!(tg.max_concurrent, 20);
                let template = tg.template.unwrap();
                assert_eq!(template.tenant, "ml");
            }
            _ => panic!("expected TaskGroup submission"),
        }
    }

    #[test]
    fn build_list_request_with_filters() {
        let req = build_list_request(
            Some("alice"),
            Some("physics"),
            Some("hpc-batch"),
            Some("running"),
        );
        assert_eq!(req.user, "alice");
        assert_eq!(req.tenant, "physics");
        assert_eq!(req.vcluster, "hpc-batch");
        assert_eq!(req.state, "running");
        assert_eq!(req.limit, 100);
    }

    #[test]
    fn build_list_request_defaults() {
        let req = build_list_request(None, None, None, None);
        assert_eq!(req.user, "");
        assert_eq!(req.tenant, "");
        assert_eq!(req.state, "");
    }

    #[test]
    fn allocation_status_to_info_conversion() {
        let status = pb::AllocationStatus {
            allocation_id: "alloc-1".to_string(),
            state: "running".to_string(),
            spec: Some(pb::AllocationSpec {
                tenant: "physics".to_string(),
                project: "training".to_string(),
                vcluster: "hpc-batch".to_string(),
                resources: Some(pb::ResourceSpec {
                    min_nodes: 4,
                    gpu_type: "GH200".to_string(),
                    ..Default::default()
                }),
                lifecycle: Some(pb::LifecycleSpec {
                    walltime: Some(prost_types::Duration {
                        seconds: 7200,
                        nanos: 0,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            user: "alice".to_string(),
            ..Default::default()
        };

        let info = allocation_status_to_info(&status);
        assert_eq!(info.id, "alloc-1");
        assert_eq!(info.state, "running");
        assert_eq!(info.tenant, "physics");
        assert_eq!(info.project, "training");
        assert_eq!(info.nodes, 4);
        assert_eq!(info.walltime, "2:00:00");
        assert_eq!(info.user, "alice");
        assert_eq!(info.gpu_type, "GH200");
    }

    #[test]
    fn node_status_to_info_conversion() {
        let status = pb::NodeStatus {
            node_id: "x1000c0s0b0n0".to_string(),
            state: "ready".to_string(),
            group: 2,
            gpu_type: "GH200".to_string(),
            gpu_count: 4,
            cpu_cores: 72,
            memory_gb: 512,
            owner_tenant: "physics".to_string(),
            owner_allocation: "alloc-1".to_string(),
            ..Default::default()
        };

        let info = node_status_to_info(&status);
        assert_eq!(info.id, "x1000c0s0b0n0");
        assert_eq!(info.state, "ready");
        assert_eq!(info.group, 2);
        assert_eq!(info.gpu_type, "GH200");
        assert_eq!(info.gpu_count, 4);
        assert_eq!(info.cpu_cores, 72);
        assert_eq!(info.memory_gb, 512);
        assert_eq!(info.owner, "physics/alloc-1");
    }

    #[test]
    fn node_status_to_info_no_owner() {
        let status = pb::NodeStatus {
            node_id: "x1000c0s0b0n1".to_string(),
            state: "ready".to_string(),
            ..Default::default()
        };
        let info = node_status_to_info(&status);
        assert_eq!(info.owner, "-");
    }

    #[test]
    fn build_diagnostics_request_full() {
        let args = crate::commands::diag::DiagArgs {
            alloc_id: "alloc-1".to_string(),
            network: false,
            storage: false,
            gpu: false,
            all: false,
        };
        let req = build_diagnostics_request(&args);
        assert_eq!(req.allocation_id, "alloc-1");
        assert_eq!(req.scope, pb::diagnostics_request::Scope::Full as i32);
    }

    #[test]
    fn build_diagnostics_request_network_only() {
        let args = crate::commands::diag::DiagArgs {
            alloc_id: "alloc-1".to_string(),
            network: true,
            storage: false,
            gpu: false,
            all: false,
        };
        let req = build_diagnostics_request(&args);
        assert_eq!(req.scope, pb::diagnostics_request::Scope::Network as i32);
    }

    #[test]
    fn build_log_stream_request_defaults() {
        let args = crate::commands::logs::LogsArgs {
            alloc_id: "alloc-1".to_string(),
            follow: false,
            stderr: false,
            node: None,
            tail: None,
            timestamps: false,
        };
        let req = build_log_stream_request(&args);
        assert_eq!(req.allocation_id, "alloc-1");
        assert_eq!(req.stream, pb::log_stream_request::Stream::Stdout as i32);
        assert!(!req.follow);
        assert_eq!(req.tail_lines, 0);
    }

    #[test]
    fn build_log_stream_request_stderr_follow() {
        let args = crate::commands::logs::LogsArgs {
            alloc_id: "alloc-2".to_string(),
            follow: true,
            stderr: true,
            node: Some("node-0".to_string()),
            tail: Some(50),
            timestamps: true,
        };
        let req = build_log_stream_request(&args);
        assert_eq!(req.allocation_id, "alloc-2");
        assert_eq!(req.stream, pb::log_stream_request::Stream::Stderr as i32);
        assert!(req.follow);
        assert_eq!(req.tail_lines, 50);
        assert_eq!(req.node_id, "node-0");
    }

    #[test]
    fn build_create_tenant_request_from_args() {
        let args = crate::commands::admin::TenantCreateArgs {
            name: "physics".to_string(),
            max_nodes: Some(100),
            fair_share: Some(0.3),
            isolation: "strict".to_string(),
        };
        let req = build_create_tenant_request(&args);
        assert_eq!(req.name, "physics");
        assert_eq!(req.isolation_level, "strict");
        let quota = req.quota.unwrap();
        assert_eq!(quota.max_nodes, 100);
        assert!((quota.fair_share_target - 0.3).abs() < 0.001);
    }

    #[test]
    fn build_list_nodes_request_with_filters() {
        let args = crate::commands::nodes::NodesArgs {
            id: None,
            state: Some("ready".to_string()),
            group: Some(3),
        };
        let req = build_list_nodes_request(&args);
        assert_eq!(req.state, "ready");
        assert_eq!(req.group, "3");
        assert_eq!(req.limit, 200);
    }

    #[test]
    fn build_query_metrics_request_builds_correctly() {
        let req = build_query_metrics_request("alloc-99");
        assert_eq!(req.allocation_id, "alloc-99");
        assert_eq!(
            req.mode,
            pb::query_metrics_request::MetricsMode::PerNode as i32
        );
    }

    #[test]
    fn build_watch_request_builds_correctly() {
        let req = build_watch_request("alloc-1", Some("physics"), None);
        assert_eq!(req.allocation_id, "alloc-1");
        assert_eq!(req.tenant, "physics");
        assert_eq!(req.vcluster, "");
    }
}
