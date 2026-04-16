//! Proto ↔ domain type conversions.
//!
//! Converts between prost-generated protobuf types and the domain types
//! defined in `lattice_common::types`.

use chrono::{DateTime, TimeZone, Utc};
use uuid::Uuid;

use lattice_common::proto::lattice::v1 as pb;
use lattice_common::types::*;

// ─── AllocationSpec → Allocation ──────────────────────────────

/// Convert a protobuf AllocationSpec into a domain Allocation.
pub fn allocation_from_proto(spec: &pb::AllocationSpec, user: &str) -> Result<Allocation, String> {
    let id = if spec.id.is_empty() {
        Uuid::new_v4()
    } else {
        // If the ID is a valid UUID, use it; otherwise generate a new one
        // (user-provided names like "step1" are not UUIDs)
        Uuid::parse_str(&spec.id).unwrap_or_else(|_| Uuid::new_v4())
    };

    let environment = spec
        .environment
        .as_ref()
        .map(environment_from_proto)
        .unwrap_or_else(default_environment);

    let resources = spec
        .resources
        .as_ref()
        .map(resources_from_proto)
        .unwrap_or_else(default_resources);

    let lifecycle = spec
        .lifecycle
        .as_ref()
        .map(lifecycle_from_proto)
        .unwrap_or_else(default_lifecycle);

    let data = spec.data.as_ref().map(data_from_proto).unwrap_or_default();

    let connectivity = spec
        .connectivity
        .as_ref()
        .map(connectivity_from_proto)
        .unwrap_or_default();

    let depends_on: Vec<Dependency> = spec.depends_on.iter().map(dependency_from_proto).collect();

    let checkpoint = spec
        .checkpoint
        .as_ref()
        .map(checkpoint_from_proto)
        .unwrap_or(CheckpointStrategy::Auto);

    let telemetry_mode = spec
        .telemetry
        .as_ref()
        .map(telemetry_from_proto)
        .unwrap_or_default();

    let requeue_policy = requeue_policy_from_str(&spec.requeue_policy);

    // ── Input validation (ADV-10, ADV-11, ADV-12) ──────────────

    // ADV-11: Reject min_nodes=0 explicitly instead of silently clamping.
    if let Some(ref r) = spec.resources {
        if r.min_nodes == 0 {
            return Err("min_nodes must be >= 1".to_string());
        }
    }

    // ADV-10: Reject walltime <= 0 for bounded allocations.
    if let Some(ref l) = spec.lifecycle {
        let ltype = pb::lifecycle_spec::Type::try_from(l.r#type)
            .unwrap_or(pb::lifecycle_spec::Type::Bounded);
        if ltype == pb::lifecycle_spec::Type::Bounded {
            if let Some(ref wt) = l.walltime {
                if wt.seconds <= 0 {
                    return Err("walltime must be > 0 for bounded allocations".to_string());
                }
            }
        }
        // ADV-12: Validate preemption_class 0-10.
        if l.preemption_class > 10 {
            return Err(format!(
                "preemption_class must be 0-10, got {}",
                l.preemption_class
            ));
        }
    }

    // F15: Reject empty required fields.
    if spec.tenant.is_empty() {
        return Err("tenant is required".to_string());
    }
    if spec.entrypoint.is_empty() {
        return Err("entrypoint is required".to_string());
    }

    // F16: Validate reactive lifecycle min <= max.
    if let Some(ref l) = spec.lifecycle {
        if l.reactive_min_nodes > 0
            && l.reactive_max_nodes > 0
            && l.reactive_min_nodes > l.reactive_max_nodes
        {
            return Err(format!(
                "reactive min_nodes ({}) must be <= max_nodes ({})",
                l.reactive_min_nodes, l.reactive_max_nodes
            ));
        }
    }

    // F17: Validate service endpoint ports.
    if let Some(ref conn) = spec.connectivity {
        for ep in &conn.expose {
            if ep.port == 0 || ep.port > 65535 {
                return Err(format!(
                    "service endpoint port must be 1-65535, got {}",
                    ep.port
                ));
            }
        }
    }

    // F18: Cap max_requeue to a reasonable limit.
    if spec.max_requeue > 100 {
        return Err(format!(
            "max_requeue must be <= 100, got {}",
            spec.max_requeue
        ));
    }

    // INV-SD3: Validate mount point non-overlap.
    // Collect ALL mount targets: image mount_points + environment mounts.
    {
        let mut mount_targets: Vec<&str> = environment
            .images
            .iter()
            .filter(|i| !i.mount_point.is_empty())
            .map(|i| i.mount_point.as_str())
            .collect();
        for m in &environment.mounts {
            if !m.target.is_empty() {
                mount_targets.push(m.target.as_str());
            }
        }
        if mount_targets.len() > 1 {
            check_mount_overlap(&mount_targets)
                .map_err(|e| format!("overlapping mount points: {e}"))?;
        }
    }

    // INV-SD5: Sensitive workload image validation.
    if spec.sensitive || environment.sign_required || environment.scan_required {
        for img in &environment.images {
            // sha256 must be non-empty (no deferred resolution for sensitive)
            if img.sha256.is_empty() && !img.resolve_on_schedule {
                return Err(format!(
                    "sensitive allocation requires pinned sha256 for image '{}'",
                    img.spec
                ));
            }
            if img.sha256.is_empty() && img.resolve_on_schedule {
                return Err(format!(
                    "sensitive allocation does not allow deferred resolution for image '{}'",
                    img.spec
                ));
            }
        }
        // writable must be false
        if environment.writable {
            return Err("sensitive allocation requires writable=false".to_string());
        }
        // devices must not contain "gpu=all" — must be specific indices
        for dev in &environment.devices {
            if dev.contains("gpu=all") {
                return Err(format!(
                    "sensitive allocation requires specific GPU indices, not '{}'; \
                     use e.g. 'nvidia.com/gpu=0,1'",
                    dev
                ));
            }
        }
    }

    Ok(Allocation {
        id,
        tenant: spec.tenant.clone(),
        project: spec.project.clone(),
        vcluster: spec.vcluster.clone(),
        user: user.to_string(),
        tags: spec.tags.clone(),
        allocation_type: AllocationType::Single,
        environment,
        entrypoint: spec.entrypoint.clone(),
        resources,
        lifecycle,
        requeue_policy,
        max_requeue: spec.max_requeue,
        data,
        connectivity,
        depends_on,
        checkpoint,
        telemetry_mode,
        state: AllocationState::Pending,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        assigned_nodes: Vec::new(),
        dag_id: None,
        exit_code: None,
        message: None,
        requeue_count: 0,
        preempted_count: 0,
        resume_from_checkpoint: false,
        sensitive: spec.sensitive,
        liveness_probe: liveness_probe_from_proto(spec),
        state_version: 0,
        dispatch_retry_count: 0,
        last_completion_report_at: None,
    })
}

// ─── Allocation → AllocationStatus ────────────────────────────

/// Convert a domain Allocation into a protobuf AllocationStatus.
pub fn allocation_to_status(alloc: &Allocation) -> pb::AllocationStatus {
    pb::AllocationStatus {
        allocation_id: alloc.id.to_string(),
        state: allocation_state_to_str(&alloc.state).to_string(),
        spec: Some(allocation_to_spec(alloc)),
        assigned_nodes: alloc.assigned_nodes.clone(),
        created_at: Some(datetime_to_prost_timestamp(alloc.created_at)),
        started_at: alloc.started_at.map(datetime_to_prost_timestamp),
        completed_at: alloc.completed_at.map(datetime_to_prost_timestamp),
        exit_code: alloc.exit_code.unwrap_or(-1),
        message: alloc.message.clone().unwrap_or_default(),
        user: alloc.user.clone(),
    }
}

/// Convert a domain Allocation back to a protobuf AllocationSpec.
pub fn allocation_to_spec(alloc: &Allocation) -> pb::AllocationSpec {
    pb::AllocationSpec {
        id: alloc.id.to_string(),
        tenant: alloc.tenant.clone(),
        project: alloc.project.clone(),
        vcluster: alloc.vcluster.clone(),
        tags: alloc.tags.clone(),
        environment: Some(environment_to_proto(&alloc.environment)),
        entrypoint: alloc.entrypoint.clone(),
        resources: Some(resources_to_proto(&alloc.resources)),
        lifecycle: Some(lifecycle_to_proto(&alloc.lifecycle)),
        data: Some(data_to_proto(&alloc.data)),
        connectivity: Some(connectivity_to_proto(&alloc.connectivity)),
        depends_on: alloc.depends_on.iter().map(dependency_to_proto).collect(),
        checkpoint: Some(checkpoint_to_proto(&alloc.checkpoint)),
        telemetry: Some(telemetry_to_proto(&alloc.telemetry_mode)),
        requeue_policy: requeue_policy_to_str(&alloc.requeue_policy).to_string(),
        max_requeue: alloc.max_requeue,
        sensitive: alloc.sensitive,
        liveness_probe: liveness_probe_to_proto(&alloc.liveness_probe),
    }
}

// ─── Node → NodeStatus ────────────────────────────────────────

/// Convert a domain Node into a protobuf NodeStatus.
pub fn node_to_status(node: &Node) -> pb::NodeStatus {
    let (state_str, state_reason) = node_state_to_str(&node.state);
    pb::NodeStatus {
        node_id: node.id.clone(),
        state: state_str.to_string(),
        state_reason,
        group: node.group,
        gpu_type: node.capabilities.gpu_type.clone().unwrap_or_default(),
        gpu_count: node.capabilities.gpu_count,
        cpu_cores: node.capabilities.cpu_cores,
        memory_gb: node.capabilities.memory_gb,
        features: node.capabilities.features.clone(),
        owner_tenant: node
            .owner
            .as_ref()
            .map(|o| o.tenant.clone())
            .unwrap_or_default(),
        owner_vcluster: node
            .owner
            .as_ref()
            .map(|o| o.vcluster.clone())
            .unwrap_or_default(),
        owner_allocation: node
            .owner
            .as_ref()
            .map(|o| o.allocation.to_string())
            .unwrap_or_default(),
        claimed_by: node
            .owner
            .as_ref()
            .and_then(|o| o.claimed_by.clone())
            .unwrap_or_default(),
        is_borrowed: node.owner.as_ref().is_some_and(|o| o.is_borrowed),
        conformance_fingerprint: node.conformance_fingerprint.clone().unwrap_or_default(),
        last_heartbeat: node.last_heartbeat.map(datetime_to_prost_timestamp),
        allocation_ids: Vec::new(), // populated separately
        memory_domains: node
            .capabilities
            .memory_topology
            .as_ref()
            .map(|t| t.domains.iter().map(memory_domain_to_proto).collect())
            .unwrap_or_default(),
        memory_interconnects: node
            .capabilities
            .memory_topology
            .as_ref()
            .map(|t| {
                t.interconnects
                    .iter()
                    .map(memory_interconnect_to_proto)
                    .collect()
            })
            .unwrap_or_default(),
        total_memory_capacity_bytes: node
            .capabilities
            .memory_topology
            .as_ref()
            .map(|t| t.total_capacity_bytes)
            .unwrap_or(0),
        agent_address: node.agent_address.clone(),
        consecutive_dispatch_failures: node.consecutive_dispatch_failures,
        reattach_in_progress: node.reattach_in_progress,
    }
}

// ─── Tenant conversions ───────────────────────────────────────

/// Convert a protobuf CreateTenantRequest into a domain Tenant.
pub fn tenant_from_create(req: &pb::CreateTenantRequest) -> Tenant {
    Tenant {
        id: req.name.clone(),
        name: req.name.clone(),
        quota: req
            .quota
            .as_ref()
            .map(quota_from_proto)
            .unwrap_or_else(default_quota),
        isolation_level: isolation_from_str(&req.isolation_level),
    }
}

/// Convert a domain Tenant into a protobuf TenantResponse.
pub fn tenant_to_response(tenant: &Tenant) -> pb::TenantResponse {
    pb::TenantResponse {
        tenant_id: tenant.id.clone(),
        name: tenant.name.clone(),
        quota: Some(quota_to_proto(&tenant.quota)),
        isolation_level: isolation_to_str(&tenant.isolation_level).to_string(),
    }
}

// ─── VCluster conversions ─────────────────────────────────────

/// Convert a protobuf CreateVClusterRequest into a domain VCluster.
pub fn vcluster_from_create(req: &pb::CreateVClusterRequest) -> VCluster {
    VCluster {
        id: format!("{}/{}", req.tenant_id, req.name),
        name: req.name.clone(),
        tenant: req.tenant_id.clone(),
        scheduler_type: scheduler_type_from_str(&req.scheduler_type),
        cost_weights: req
            .cost_weights
            .as_ref()
            .map(cost_weights_from_proto)
            .unwrap_or_default(),
        dedicated_nodes: req.dedicated_nodes.clone(),
        allow_borrowing: req.allow_borrowing,
        allow_lending: req.allow_lending,
    }
}

/// Convert a domain VCluster into a protobuf VClusterResponse.
pub fn vcluster_to_response(vc: &VCluster) -> pb::VClusterResponse {
    pb::VClusterResponse {
        vcluster_id: vc.id.clone(),
        tenant_id: vc.tenant.clone(),
        name: vc.name.clone(),
        scheduler_type: scheduler_type_to_str(&vc.scheduler_type).to_string(),
        cost_weights: Some(cost_weights_to_proto(&vc.cost_weights)),
        allow_borrowing: vc.allow_borrowing,
        allow_lending: vc.allow_lending,
    }
}

// ─── Sub-type conversions ─────────────────────────────────────

fn image_ref_from_proto(p: &pb::ImageRefProto) -> lattice_common::types::ImageRef {
    let image_type = match p.image_type.as_str() {
        "oci" => lattice_common::types::ImageType::Oci,
        _ => lattice_common::types::ImageType::Uenv,
    };
    lattice_common::types::ImageRef {
        spec: p.spec.clone(),
        image_type,
        registry: p.registry.clone(),
        name: p.name.clone(),
        version: p.version.clone(),
        original_tag: p.original_tag.clone(),
        sha256: p.sha256.clone(),
        size_bytes: p.size_bytes,
        mount_point: p.mount_point.clone(),
        resolve_on_schedule: p.resolve_on_schedule,
    }
}

fn image_ref_to_proto(r: &lattice_common::types::ImageRef) -> pb::ImageRefProto {
    pb::ImageRefProto {
        spec: r.spec.clone(),
        image_type: match r.image_type {
            lattice_common::types::ImageType::Uenv => "uenv".to_string(),
            lattice_common::types::ImageType::Oci => "oci".to_string(),
        },
        registry: r.registry.clone(),
        name: r.name.clone(),
        version: r.version.clone(),
        original_tag: r.original_tag.clone(),
        sha256: r.sha256.clone(),
        size_bytes: r.size_bytes,
        mount_point: r.mount_point.clone(),
        resolve_on_schedule: r.resolve_on_schedule,
    }
}

fn env_patch_from_proto(p: &pb::EnvPatchProto) -> lattice_common::types::EnvPatch {
    let op = match p.op.as_str() {
        "prepend" => lattice_common::types::EnvOp::Prepend,
        "append" => lattice_common::types::EnvOp::Append,
        "unset" => lattice_common::types::EnvOp::Unset,
        _ => lattice_common::types::EnvOp::Set,
    };
    lattice_common::types::EnvPatch {
        variable: p.variable.clone(),
        op,
        value: p.value.clone(),
        separator: if p.separator.is_empty() {
            ":".to_string()
        } else {
            p.separator.clone()
        },
    }
}

fn env_patch_to_proto(p: &lattice_common::types::EnvPatch) -> pb::EnvPatchProto {
    pb::EnvPatchProto {
        variable: p.variable.clone(),
        op: match p.op {
            lattice_common::types::EnvOp::Prepend => "prepend".to_string(),
            lattice_common::types::EnvOp::Append => "append".to_string(),
            lattice_common::types::EnvOp::Set => "set".to_string(),
            lattice_common::types::EnvOp::Unset => "unset".to_string(),
        },
        value: p.value.clone(),
        separator: p.separator.clone(),
    }
}

fn mount_spec_from_proto(p: &pb::MountSpecProto) -> lattice_common::types::MountSpec {
    lattice_common::types::MountSpec {
        source: p.source.clone(),
        target: p.target.clone(),
        options: p.options.clone(),
    }
}

fn mount_spec_to_proto(m: &lattice_common::types::MountSpec) -> pb::MountSpecProto {
    pb::MountSpecProto {
        source: m.source.clone(),
        target: m.target.clone(),
        options: m.options.clone(),
    }
}

fn container_spec_from_proto(p: &pb::ContainerSpecProto) -> lattice_common::types::ContainerSpec {
    lattice_common::types::ContainerSpec {
        base_environments: p.base_environments.clone(),
        mounts: p.mounts.iter().map(mount_spec_from_proto).collect(),
        devices: p.devices.clone(),
        workdir: p.workdir.clone(),
        writable: p.writable,
        env: p.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        annotations: p
            .annotations
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    }
}

fn container_spec_to_proto(c: &lattice_common::types::ContainerSpec) -> pb::ContainerSpecProto {
    pb::ContainerSpecProto {
        base_environments: c.base_environments.clone(),
        mounts: c.mounts.iter().map(mount_spec_to_proto).collect(),
        devices: c.devices.clone(),
        workdir: c.workdir.clone(),
        writable: c.writable,
        env: c.env.iter().cloned().collect(),
        annotations: c.annotations.iter().cloned().collect(),
    }
}

fn environment_from_proto(e: &pb::EnvironmentSpec) -> Environment {
    Environment {
        images: e.images.iter().map(image_ref_from_proto).collect(),
        env_patches: e.env_patches.iter().map(env_patch_from_proto).collect(),
        devices: e.devices.clone(),
        mounts: e.env_mounts.iter().map(mount_spec_from_proto).collect(),
        container: e.container.as_ref().map(container_spec_from_proto),
        writable: e.writable,
        sign_required: e.sign_required,
        scan_required: e.scan_required,
        approved_bases_only: e.approved_bases_only,
    }
}

fn environment_to_proto(e: &Environment) -> pb::EnvironmentSpec {
    pb::EnvironmentSpec {
        images: e.images.iter().map(image_ref_to_proto).collect(),
        env_patches: e.env_patches.iter().map(env_patch_to_proto).collect(),
        devices: e.devices.clone(),
        env_mounts: e.mounts.iter().map(mount_spec_to_proto).collect(),
        container: e.container.as_ref().map(container_spec_to_proto),
        writable: e.writable,
        sign_required: e.sign_required,
        scan_required: e.scan_required,
        approved_bases_only: e.approved_bases_only,
    }
}

fn default_environment() -> Environment {
    Environment::default()
}

fn resources_from_proto(r: &pb::ResourceSpec) -> ResourceRequest {
    // ADV-11: min_nodes validated upstream; no silent clamping.
    let nodes = if r.max_nodes > 0 && r.max_nodes != r.min_nodes {
        NodeCount::Range {
            min: r.min_nodes,
            max: r.max_nodes,
        }
    } else {
        NodeCount::Exact(r.min_nodes)
    };

    ResourceRequest {
        nodes,
        constraints: ResourceConstraints {
            gpu_type: non_empty(&r.gpu_type),
            features: r.features.clone(),
            topology: topology_hint_from_str(&r.topology_hint),
            feature_counts: r.feature_counts.clone(),
            ..Default::default()
        },
    }
}

fn resources_to_proto(r: &ResourceRequest) -> pb::ResourceSpec {
    let (min_nodes, max_nodes) = match &r.nodes {
        NodeCount::Exact(n) => (*n, 0),
        NodeCount::Range { min, max } => (*min, *max),
    };

    pb::ResourceSpec {
        min_nodes,
        max_nodes,
        gpu_type: r.constraints.gpu_type.clone().unwrap_or_default(),
        features: r.constraints.features.clone(),
        topology_hint: r
            .constraints
            .topology
            .as_ref()
            .map(topology_hint_to_str)
            .unwrap_or("")
            .to_string(),
        feature_counts: r.constraints.feature_counts.clone(),
    }
}

fn default_resources() -> ResourceRequest {
    ResourceRequest {
        nodes: NodeCount::Exact(1),
        constraints: ResourceConstraints::default(),
    }
}

fn lifecycle_from_proto(l: &pb::LifecycleSpec) -> Lifecycle {
    let lifecycle_type = match pb::lifecycle_spec::Type::try_from(l.r#type)
        .unwrap_or(pb::lifecycle_spec::Type::Bounded)
    {
        pb::lifecycle_spec::Type::Bounded => {
            let walltime = l
                .walltime
                .as_ref()
                .map(|d| chrono::Duration::seconds(d.seconds))
                .unwrap_or_else(|| chrono::Duration::hours(1));
            LifecycleType::Bounded { walltime }
        }
        pb::lifecycle_spec::Type::Unbounded => LifecycleType::Unbounded,
        pb::lifecycle_spec::Type::Reactive => LifecycleType::Reactive {
            min_nodes: l.reactive_min_nodes,
            max_nodes: l.reactive_max_nodes,
            metric: l.reactive_metric.clone(),
            target: l.reactive_target.clone(),
        },
    };

    Lifecycle {
        lifecycle_type,
        preemption_class: l.preemption_class as u8,
    }
}

fn lifecycle_to_proto(l: &Lifecycle) -> pb::LifecycleSpec {
    match &l.lifecycle_type {
        LifecycleType::Bounded { walltime } => pb::LifecycleSpec {
            r#type: pb::lifecycle_spec::Type::Bounded as i32,
            walltime: Some(prost_types::Duration {
                seconds: walltime.num_seconds(),
                nanos: 0,
            }),
            preemption_class: l.preemption_class as u32,
            reactive_min_nodes: 0,
            reactive_max_nodes: 0,
            reactive_metric: String::new(),
            reactive_target: String::new(),
        },
        LifecycleType::Unbounded => pb::LifecycleSpec {
            r#type: pb::lifecycle_spec::Type::Unbounded as i32,
            walltime: None,
            preemption_class: l.preemption_class as u32,
            reactive_min_nodes: 0,
            reactive_max_nodes: 0,
            reactive_metric: String::new(),
            reactive_target: String::new(),
        },
        LifecycleType::Reactive {
            min_nodes,
            max_nodes,
            metric,
            target,
        } => pb::LifecycleSpec {
            r#type: pb::lifecycle_spec::Type::Reactive as i32,
            walltime: None,
            preemption_class: l.preemption_class as u32,
            reactive_min_nodes: *min_nodes,
            reactive_max_nodes: *max_nodes,
            reactive_metric: metric.clone(),
            reactive_target: target.clone(),
        },
    }
}

fn default_lifecycle() -> Lifecycle {
    Lifecycle {
        lifecycle_type: LifecycleType::Bounded {
            walltime: chrono::Duration::hours(1),
        },
        preemption_class: 0,
    }
}

fn data_from_proto(d: &pb::DataSpec) -> DataRequirements {
    DataRequirements {
        mounts: d.mounts.iter().map(mount_from_proto).collect(),
        use_defaults: d.use_defaults,
        scratch_per_node: non_empty(&d.scratch_per_node),
    }
}

fn data_to_proto(d: &DataRequirements) -> pb::DataSpec {
    pb::DataSpec {
        mounts: d.mounts.iter().map(mount_to_proto).collect(),
        use_defaults: d.use_defaults,
        scratch_per_node: d.scratch_per_node.clone().unwrap_or_default(),
    }
}

fn mount_from_proto(m: &pb::MountSpec) -> DataMount {
    DataMount {
        source: m.source.clone(),
        target: m.target.clone(),
        access: match m.access.as_str() {
            "read-write" | "rw" => DataAccess::ReadWrite,
            _ => DataAccess::ReadOnly,
        },
        tier_hint: match m.tier_hint.as_str() {
            "hot" => Some(StorageTier::Hot),
            "warm" => Some(StorageTier::Warm),
            "cold" => Some(StorageTier::Cold),
            _ => None,
        },
    }
}

fn mount_to_proto(m: &DataMount) -> pb::MountSpec {
    pb::MountSpec {
        source: m.source.clone(),
        target: m.target.clone(),
        access: match m.access {
            DataAccess::ReadOnly => "read-only".to_string(),
            DataAccess::ReadWrite => "read-write".to_string(),
        },
        tier_hint: m
            .tier_hint
            .as_ref()
            .map(|t| match t {
                StorageTier::Hot => "hot",
                StorageTier::Warm => "warm",
                StorageTier::Cold => "cold",
            })
            .unwrap_or("")
            .to_string(),
    }
}

fn connectivity_from_proto(c: &pb::ConnectivitySpec) -> Connectivity {
    Connectivity {
        network_domain: non_empty(&c.network_domain),
        expose: c
            .expose
            .iter()
            .map(|e| ServiceEndpoint {
                name: e.name.clone(),
                port: e.port as u16,
                protocol: non_empty(&e.protocol),
            })
            .collect(),
    }
}

fn connectivity_to_proto(c: &Connectivity) -> pb::ConnectivitySpec {
    pb::ConnectivitySpec {
        network_domain: c.network_domain.clone().unwrap_or_default(),
        expose: c
            .expose
            .iter()
            .map(|e| pb::EndpointSpec {
                name: e.name.clone(),
                port: e.port as u32,
                protocol: e.protocol.clone().unwrap_or_default(),
            })
            .collect(),
    }
}

fn dependency_from_proto(d: &pb::DependencySpec) -> Dependency {
    Dependency {
        ref_id: d.ref_id.clone(),
        condition: match d.condition.as_str() {
            "failure" | "afternotok" => DependencyCondition::Failure,
            "any" | "afterany" => DependencyCondition::Any,
            "corresponding" | "aftercorr" => DependencyCondition::Corresponding,
            "mutex" | "singleton" => DependencyCondition::Mutex,
            _ => DependencyCondition::Success,
        },
    }
}

fn dependency_to_proto(d: &Dependency) -> pb::DependencySpec {
    pb::DependencySpec {
        ref_id: d.ref_id.clone(),
        condition: match d.condition {
            DependencyCondition::Success => "success".to_string(),
            DependencyCondition::Failure => "failure".to_string(),
            DependencyCondition::Any => "any".to_string(),
            DependencyCondition::Corresponding => "corresponding".to_string(),
            DependencyCondition::Mutex => "mutex".to_string(),
        },
    }
}

fn checkpoint_from_proto(c: &pb::CheckpointSpec) -> CheckpointStrategy {
    match c.strategy.as_str() {
        "manual" => CheckpointStrategy::Manual,
        "none" => CheckpointStrategy::None,
        _ => CheckpointStrategy::Auto,
    }
}

fn checkpoint_to_proto(c: &CheckpointStrategy) -> pb::CheckpointSpec {
    pb::CheckpointSpec {
        strategy: match c {
            CheckpointStrategy::Auto => "auto".to_string(),
            CheckpointStrategy::Manual => "manual".to_string(),
            CheckpointStrategy::None => "none".to_string(),
        },
    }
}

fn telemetry_from_proto(t: &pb::TelemetrySpec) -> TelemetryMode {
    match t.mode.as_str() {
        "debug" => TelemetryMode::Debug {
            duration_seconds: t.duration_seconds,
        },
        "audit" => TelemetryMode::Audit,
        _ => TelemetryMode::Prod,
    }
}

fn telemetry_to_proto(t: &TelemetryMode) -> pb::TelemetrySpec {
    match t {
        TelemetryMode::Prod => pb::TelemetrySpec {
            mode: "prod".to_string(),
            duration_seconds: 0,
        },
        TelemetryMode::Debug { duration_seconds } => pb::TelemetrySpec {
            mode: "debug".to_string(),
            duration_seconds: *duration_seconds,
        },
        TelemetryMode::Audit => pb::TelemetrySpec {
            mode: "audit".to_string(),
            duration_seconds: 0,
        },
    }
}

fn quota_from_proto(q: &pb::TenantQuotaSpec) -> TenantQuota {
    TenantQuota {
        max_nodes: q.max_nodes,
        fair_share_target: q.fair_share_target,
        gpu_hours_budget: q.gpu_hours_budget,
        node_hours_budget: q.node_hours_budget,
        max_concurrent_allocations: q.max_concurrent_allocations,
        burst_allowance: q.burst_allowance,
    }
}

fn quota_to_proto(q: &TenantQuota) -> pb::TenantQuotaSpec {
    pb::TenantQuotaSpec {
        max_nodes: q.max_nodes,
        fair_share_target: q.fair_share_target,
        gpu_hours_budget: q.gpu_hours_budget,
        max_concurrent_allocations: q.max_concurrent_allocations,
        node_hours_budget: q.node_hours_budget,
        burst_allowance: q.burst_allowance,
    }
}

fn default_quota() -> TenantQuota {
    TenantQuota {
        max_nodes: 100,
        fair_share_target: 0.0,
        gpu_hours_budget: None,
        node_hours_budget: None,
        max_concurrent_allocations: None,
        burst_allowance: None,
    }
}

fn cost_weights_from_proto(w: &pb::CostWeightsSpec) -> CostWeights {
    CostWeights {
        priority: w.priority,
        wait_time: w.wait_time,
        fair_share: w.fair_share,
        topology: w.topology,
        data_readiness: w.data_readiness,
        backlog: w.backlog,
        energy: w.energy,
        checkpoint_efficiency: w.checkpoint_efficiency,
        conformance: w.conformance,
    }
}

fn cost_weights_to_proto(w: &CostWeights) -> pb::CostWeightsSpec {
    pb::CostWeightsSpec {
        priority: w.priority,
        wait_time: w.wait_time,
        fair_share: w.fair_share,
        topology: w.topology,
        data_readiness: w.data_readiness,
        backlog: w.backlog,
        energy: w.energy,
        checkpoint_efficiency: w.checkpoint_efficiency,
        conformance: w.conformance,
    }
}

// ─── String ↔ enum helpers ──────────────────────────────────

pub fn allocation_state_to_str(state: &AllocationState) -> &'static str {
    match state {
        AllocationState::Pending => "pending",
        AllocationState::Staging => "staging",
        AllocationState::Running => "running",
        AllocationState::Checkpointing => "checkpointing",
        AllocationState::Suspended => "suspended",
        AllocationState::Completed => "completed",
        AllocationState::Failed => "failed",
        AllocationState::Cancelled => "cancelled",
    }
}

pub fn allocation_state_from_str(s: &str) -> Option<AllocationState> {
    match s {
        "pending" => Some(AllocationState::Pending),
        "staging" => Some(AllocationState::Staging),
        "running" => Some(AllocationState::Running),
        "checkpointing" => Some(AllocationState::Checkpointing),
        "suspended" => Some(AllocationState::Suspended),
        "completed" => Some(AllocationState::Completed),
        "failed" => Some(AllocationState::Failed),
        "cancelled" => Some(AllocationState::Cancelled),
        _ => None,
    }
}

pub fn node_state_to_str_pub(state: &NodeState) -> (&'static str, String) {
    node_state_to_str(state)
}

fn node_state_to_str(state: &NodeState) -> (&'static str, String) {
    match state {
        NodeState::Unknown => ("unknown", String::new()),
        NodeState::Booting => ("booting", String::new()),
        NodeState::Ready => ("ready", String::new()),
        NodeState::Degraded { reason } => ("degraded", reason.clone()),
        NodeState::Down { reason } => ("down", reason.clone()),
        NodeState::Draining => ("draining", String::new()),
        NodeState::Drained => ("drained", String::new()),
        NodeState::Failed { reason } => ("failed", reason.clone()),
    }
}

pub fn node_state_from_str(s: &str) -> NodeState {
    match s {
        "booting" => NodeState::Booting,
        "ready" => NodeState::Ready,
        "degraded" => NodeState::Degraded {
            reason: String::new(),
        },
        "down" => NodeState::Down {
            reason: String::new(),
        },
        "draining" => NodeState::Draining,
        "drained" => NodeState::Drained,
        "failed" => NodeState::Failed {
            reason: String::new(),
        },
        _ => NodeState::Unknown,
    }
}

fn topology_hint_from_str(s: &str) -> Option<TopologyHint> {
    match s {
        "tight" => Some(TopologyHint::Tight),
        "spread" => Some(TopologyHint::Spread),
        "any" => Some(TopologyHint::Any),
        _ => None,
    }
}

fn topology_hint_to_str(t: &TopologyHint) -> &'static str {
    match t {
        TopologyHint::Tight => "tight",
        TopologyHint::Spread => "spread",
        TopologyHint::Any => "any",
    }
}

fn requeue_policy_from_str(s: &str) -> RequeuePolicy {
    match s {
        "on_node_failure" => RequeuePolicy::OnNodeFailure,
        "always" => RequeuePolicy::Always,
        _ => RequeuePolicy::Never,
    }
}

fn requeue_policy_to_str(p: &RequeuePolicy) -> &'static str {
    match p {
        RequeuePolicy::Never => "never",
        RequeuePolicy::OnNodeFailure => "on_node_failure",
        RequeuePolicy::Always => "always",
    }
}

// ─── Liveness probe conversions ──────────────────────────────

fn liveness_probe_from_proto(spec: &pb::AllocationSpec) -> Option<LivenessProbe> {
    let p = spec.liveness_probe.as_ref()?;
    let probe_type = match p.probe_type.as_str() {
        "http" => ProbeType::Http {
            port: p.port as u16,
            path: p.path.clone(),
        },
        _ => ProbeType::Tcp {
            port: p.port as u16,
        },
    };
    Some(LivenessProbe {
        probe_type,
        period_secs: if p.period_secs > 0 { p.period_secs } else { 30 },
        initial_delay_secs: p.initial_delay_secs,
        failure_threshold: if p.failure_threshold > 0 {
            p.failure_threshold
        } else {
            3
        },
        timeout_secs: if p.timeout_secs > 0 {
            p.timeout_secs
        } else {
            5
        },
    })
}

fn liveness_probe_to_proto(probe: &Option<LivenessProbe>) -> Option<pb::LivenessProbeSpec> {
    let p = probe.as_ref()?;
    let (probe_type, port, path) = match &p.probe_type {
        ProbeType::Tcp { port } => ("tcp".to_string(), *port as u32, String::new()),
        ProbeType::Http { port, path } => ("http".to_string(), *port as u32, path.clone()),
    };
    Some(pb::LivenessProbeSpec {
        probe_type,
        port,
        path,
        period_secs: p.period_secs,
        initial_delay_secs: p.initial_delay_secs,
        failure_threshold: p.failure_threshold,
        timeout_secs: p.timeout_secs,
    })
}

fn isolation_from_str(s: &str) -> IsolationLevel {
    match s {
        "strict" => IsolationLevel::Strict,
        _ => IsolationLevel::Standard,
    }
}

fn isolation_to_str(l: &IsolationLevel) -> &'static str {
    match l {
        IsolationLevel::Standard => "standard",
        IsolationLevel::Strict => "strict",
    }
}

fn scheduler_type_from_str(s: &str) -> SchedulerType {
    match s {
        "service_bin_pack" => SchedulerType::ServiceBinPack,
        "sensitive_reservation" => SchedulerType::SensitiveReservation,
        "interactive_fifo" => SchedulerType::InteractiveFifo,
        _ => SchedulerType::HpcBackfill,
    }
}

fn scheduler_type_to_str(t: &SchedulerType) -> &'static str {
    match t {
        SchedulerType::HpcBackfill => "hpc_backfill",
        SchedulerType::ServiceBinPack => "service_bin_pack",
        SchedulerType::SensitiveReservation => "sensitive_reservation",
        SchedulerType::InteractiveFifo => "interactive_fifo",
    }
}

// ─── Memory Topology conversions ────────────────────────────

/// Convert a domain MemoryTopology into proto messages.
pub fn memory_topology_to_proto(
    topo: &MemoryTopology,
) -> (
    Vec<pb::MemoryDomainProto>,
    Vec<pb::MemoryInterconnectProto>,
    u64,
) {
    let domains = topo.domains.iter().map(memory_domain_to_proto).collect();
    let interconnects = topo
        .interconnects
        .iter()
        .map(memory_interconnect_to_proto)
        .collect();
    (domains, interconnects, topo.total_capacity_bytes)
}

/// Convert proto messages back to a domain MemoryTopology.
pub fn memory_topology_from_proto(
    domains: &[pb::MemoryDomainProto],
    interconnects: &[pb::MemoryInterconnectProto],
    total_bytes: u64,
) -> Option<MemoryTopology> {
    if domains.is_empty() {
        return None;
    }
    Some(MemoryTopology {
        domains: domains.iter().map(memory_domain_from_proto).collect(),
        interconnects: interconnects
            .iter()
            .map(memory_interconnect_from_proto)
            .collect(),
        total_capacity_bytes: total_bytes,
    })
}

fn memory_domain_to_proto(d: &MemoryDomain) -> pb::MemoryDomainProto {
    pb::MemoryDomainProto {
        id: d.id,
        domain_type: memory_domain_type_to_str(&d.domain_type).to_string(),
        capacity_bytes: d.capacity_bytes,
        numa_node: d.numa_node,
        attached_cpus: d.attached_cpus.clone(),
        attached_gpus: d.attached_gpus.clone(),
    }
}

fn memory_domain_from_proto(d: &pb::MemoryDomainProto) -> MemoryDomain {
    MemoryDomain {
        id: d.id,
        domain_type: memory_domain_type_from_str(&d.domain_type),
        capacity_bytes: d.capacity_bytes,
        numa_node: d.numa_node,
        attached_cpus: d.attached_cpus.clone(),
        attached_gpus: d.attached_gpus.clone(),
    }
}

fn memory_interconnect_to_proto(i: &MemoryInterconnect) -> pb::MemoryInterconnectProto {
    pb::MemoryInterconnectProto {
        domain_a: i.domain_a,
        domain_b: i.domain_b,
        link_type: memory_link_type_to_str(&i.link_type).to_string(),
        bandwidth_gbps: i.bandwidth_gbps,
        latency_ns: i.latency_ns,
    }
}

fn memory_interconnect_from_proto(i: &pb::MemoryInterconnectProto) -> MemoryInterconnect {
    MemoryInterconnect {
        domain_a: i.domain_a,
        domain_b: i.domain_b,
        link_type: memory_link_type_from_str(&i.link_type),
        bandwidth_gbps: i.bandwidth_gbps,
        latency_ns: i.latency_ns,
    }
}

fn memory_domain_type_to_str(t: &MemoryDomainType) -> &'static str {
    match t {
        MemoryDomainType::Dram => "dram",
        MemoryDomainType::Hbm => "hbm",
        MemoryDomainType::CxlAttached => "cxl_attached",
        MemoryDomainType::Unified => "unified",
    }
}

fn memory_domain_type_from_str(s: &str) -> MemoryDomainType {
    match s {
        "hbm" => MemoryDomainType::Hbm,
        "cxl_attached" => MemoryDomainType::CxlAttached,
        "unified" => MemoryDomainType::Unified,
        _ => MemoryDomainType::Dram,
    }
}

fn memory_link_type_to_str(t: &MemoryLinkType) -> &'static str {
    match t {
        MemoryLinkType::NumaLink => "numa_link",
        MemoryLinkType::CxlSwitch => "cxl_switch",
        MemoryLinkType::CoherentFabric => "coherent_fabric",
    }
}

fn memory_link_type_from_str(s: &str) -> MemoryLinkType {
    match s {
        "cxl_switch" => MemoryLinkType::CxlSwitch,
        "coherent_fabric" => MemoryLinkType::CoherentFabric,
        _ => MemoryLinkType::NumaLink,
    }
}

// ─── Timestamp helpers ──────────────────────────────────────

fn datetime_to_prost_timestamp(dt: DateTime<Utc>) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: dt.timestamp(),
        nanos: dt.timestamp_subsec_nanos() as i32,
    }
}

pub fn prost_timestamp_to_datetime(ts: &prost_types::Timestamp) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(ts.seconds, ts.nanos as u32).single()
}

// ─── Utility ─────────────────────────────────────────────────

fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

// ─── AllocationFilter from ListAllocationsRequest ─────────────

pub fn filter_from_list_request(
    req: &pb::ListAllocationsRequest,
) -> lattice_common::traits::AllocationFilter {
    lattice_common::traits::AllocationFilter {
        user: non_empty(&req.user),
        tenant: non_empty(&req.tenant),
        state: allocation_state_from_str(&req.state),
        vcluster: non_empty(&req.vcluster),
    }
}

/// Convert ListNodesRequest to NodeFilter.
pub fn node_filter_from_list_request(
    req: &pb::ListNodesRequest,
) -> lattice_common::traits::NodeFilter {
    lattice_common::traits::NodeFilter {
        state: if req.state.is_empty() {
            None
        } else {
            Some(node_state_from_str(&req.state))
        },
        group: req.group.parse::<u32>().ok(),
        tenant: non_empty(&req.tenant),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::AllocationBuilder;

    #[test]
    fn roundtrip_allocation_spec() {
        let alloc = AllocationBuilder::new()
            .tenant("physics")
            .project("sim")
            .build();

        let proto = allocation_to_spec(&alloc);
        let back = allocation_from_proto(&proto, "test-user").unwrap();

        assert_eq!(back.tenant, alloc.tenant);
        assert_eq!(back.project, alloc.project);
        assert_eq!(back.entrypoint, alloc.entrypoint);
    }

    #[test]
    fn roundtrip_allocation_status() {
        let alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();

        let status = allocation_to_status(&alloc);
        assert_eq!(status.state, "running");
        assert_eq!(status.allocation_id, alloc.id.to_string());
    }

    #[test]
    fn roundtrip_node_status() {
        use lattice_test_harness::fixtures::NodeBuilder;
        let node = NodeBuilder::new().state(NodeState::Ready).build();
        let status = node_to_status(&node);
        assert_eq!(status.node_id, node.id);
        assert_eq!(status.state, "ready");
    }

    #[test]
    fn roundtrip_tenant() {
        let req = pb::CreateTenantRequest {
            name: "physics".to_string(),
            quota: Some(pb::TenantQuotaSpec {
                max_nodes: 50,
                fair_share_target: 0.3,
                gpu_hours_budget: Some(1000.0),
                max_concurrent_allocations: Some(10),
                node_hours_budget: None,
                burst_allowance: None,
            }),
            isolation_level: "standard".to_string(),
        };
        let tenant = tenant_from_create(&req);
        assert_eq!(tenant.name, "physics");
        assert_eq!(tenant.quota.max_nodes, 50);

        let resp = tenant_to_response(&tenant);
        assert_eq!(resp.name, "physics");
        assert_eq!(resp.quota.unwrap().max_nodes, 50);
    }

    #[test]
    fn roundtrip_tenant_with_node_hours_budget() {
        let req = pb::CreateTenantRequest {
            name: "ml-team".to_string(),
            quota: Some(pb::TenantQuotaSpec {
                max_nodes: 20,
                fair_share_target: 0.2,
                gpu_hours_budget: Some(5000.0),
                max_concurrent_allocations: Some(5),
                node_hours_budget: Some(10000.0),
                burst_allowance: Some(1.5),
            }),
            isolation_level: "standard".to_string(),
        };
        let tenant = tenant_from_create(&req);
        assert_eq!(tenant.quota.node_hours_budget, Some(10000.0));
        assert_eq!(tenant.quota.burst_allowance, Some(1.5));
        assert_eq!(tenant.quota.gpu_hours_budget, Some(5000.0));

        // Round-trip through proto
        let resp = tenant_to_response(&tenant);
        let quota = resp.quota.unwrap();
        assert_eq!(quota.node_hours_budget, Some(10000.0));
        assert_eq!(quota.burst_allowance, Some(1.5));
        assert_eq!(quota.gpu_hours_budget, Some(5000.0));

        // And back from proto
        let restored = quota_from_proto(&quota);
        assert_eq!(restored.node_hours_budget, Some(10000.0));
        assert_eq!(restored.burst_allowance, Some(1.5));
    }

    #[test]
    fn roundtrip_vcluster() {
        let req = pb::CreateVClusterRequest {
            tenant_id: "physics".to_string(),
            name: "hpc-batch".to_string(),
            scheduler_type: "hpc_backfill".to_string(),
            cost_weights: Some(pb::CostWeightsSpec {
                priority: 0.2,
                wait_time: 0.2,
                fair_share: 0.2,
                topology: 0.15,
                data_readiness: 0.1,
                backlog: 0.05,
                energy: 0.0,
                checkpoint_efficiency: 0.0,
                conformance: 0.1,
            }),
            dedicated_nodes: vec!["n0".to_string()],
            allow_borrowing: true,
            allow_lending: false,
        };
        let vc = vcluster_from_create(&req);
        assert_eq!(vc.name, "hpc-batch");
        assert_eq!(vc.scheduler_type, SchedulerType::HpcBackfill);
        assert!(vc.allow_borrowing);

        let resp = vcluster_to_response(&vc);
        assert_eq!(resp.scheduler_type, "hpc_backfill");
    }

    #[test]
    fn allocation_state_roundtrip() {
        let states = [
            AllocationState::Pending,
            AllocationState::Staging,
            AllocationState::Running,
            AllocationState::Checkpointing,
            AllocationState::Suspended,
            AllocationState::Completed,
            AllocationState::Failed,
            AllocationState::Cancelled,
        ];
        for state in &states {
            let s = allocation_state_to_str(state);
            let back = allocation_state_from_str(s).unwrap();
            assert_eq!(&back, state);
        }
    }

    #[test]
    fn node_state_to_string_roundtrip() {
        let (s, _) = node_state_to_str(&NodeState::Ready);
        assert_eq!(s, "ready");
        let back = node_state_from_str(s);
        assert_eq!(back, NodeState::Ready);
    }

    #[test]
    fn lifecycle_bounded_roundtrip() {
        let lifecycle = Lifecycle {
            lifecycle_type: LifecycleType::Bounded {
                walltime: chrono::Duration::hours(2),
            },
            preemption_class: 3,
        };
        let proto = lifecycle_to_proto(&lifecycle);
        let back = lifecycle_from_proto(&proto);
        assert_eq!(back.preemption_class, 3);
        match back.lifecycle_type {
            LifecycleType::Bounded { walltime } => {
                assert_eq!(walltime.num_hours(), 2);
            }
            _ => panic!("Expected Bounded"),
        }
    }

    #[test]
    fn lifecycle_reactive_roundtrip() {
        let lifecycle = Lifecycle {
            lifecycle_type: LifecycleType::Reactive {
                min_nodes: 1,
                max_nodes: 8,
                metric: "gpu_util".to_string(),
                target: "0.8".to_string(),
            },
            preemption_class: 2,
        };
        let proto = lifecycle_to_proto(&lifecycle);
        let back = lifecycle_from_proto(&proto);
        match back.lifecycle_type {
            LifecycleType::Reactive {
                min_nodes,
                max_nodes,
                ..
            } => {
                assert_eq!(min_nodes, 1);
                assert_eq!(max_nodes, 8);
            }
            _ => panic!("Expected Reactive"),
        }
    }

    #[test]
    fn dependency_condition_roundtrip() {
        let deps = vec![
            ("success", DependencyCondition::Success),
            ("failure", DependencyCondition::Failure),
            ("any", DependencyCondition::Any),
            ("corresponding", DependencyCondition::Corresponding),
            ("mutex", DependencyCondition::Mutex),
        ];
        for (s, cond) in deps {
            let proto = pb::DependencySpec {
                ref_id: "test".to_string(),
                condition: s.to_string(),
            };
            let dep = dependency_from_proto(&proto);
            assert_eq!(
                std::mem::discriminant(&dep.condition),
                std::mem::discriminant(&cond)
            );

            let back = dependency_to_proto(&dep);
            assert_eq!(back.condition, s);
        }
    }

    #[test]
    fn checkpoint_strategy_roundtrip() {
        for (s, strat) in [
            ("auto", CheckpointStrategy::Auto),
            ("manual", CheckpointStrategy::Manual),
            ("none", CheckpointStrategy::None),
        ] {
            let proto = pb::CheckpointSpec {
                strategy: s.to_string(),
            };
            let back = checkpoint_from_proto(&proto);
            assert_eq!(
                std::mem::discriminant(&back),
                std::mem::discriminant(&strat)
            );
        }
    }

    #[test]
    fn filter_from_list_request_parses_state() {
        let req = pb::ListAllocationsRequest {
            tenant: "physics".to_string(),
            user: String::new(),
            vcluster: String::new(),
            state: "running".to_string(),
            limit: 10,
            cursor: String::new(),
        };
        let filter = filter_from_list_request(&req);
        assert_eq!(filter.tenant, Some("physics".to_string()));
        assert_eq!(filter.state, Some(AllocationState::Running));
    }

    #[test]
    fn timestamp_roundtrip() {
        let now = Utc::now();
        let ts = datetime_to_prost_timestamp(now);
        let back = prost_timestamp_to_datetime(&ts).unwrap();
        assert_eq!(back.timestamp(), now.timestamp());
    }

    #[test]
    fn timestamp_zero_epoch() {
        let ts = prost_types::Timestamp {
            seconds: 0,
            nanos: 0,
        };
        let dt = prost_timestamp_to_datetime(&ts).unwrap();
        assert_eq!(dt.timestamp(), 0);
    }

    // ─── G-NEW-7: Input validation edge cases ────────────────────

    #[test]
    fn reject_min_nodes_zero() {
        let spec = pb::AllocationSpec {
            tenant: "physics".to_string(),
            entrypoint: "./run.sh".to_string(),
            resources: Some(pb::ResourceSpec {
                min_nodes: 0,
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = allocation_from_proto(&spec, "alice");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("min_nodes"),
            "error should mention min_nodes"
        );
    }

    #[test]
    fn reject_walltime_zero_for_bounded() {
        let spec = pb::AllocationSpec {
            tenant: "physics".to_string(),
            entrypoint: "./run.sh".to_string(),
            resources: Some(pb::ResourceSpec {
                min_nodes: 1,
                ..Default::default()
            }),
            lifecycle: Some(pb::LifecycleSpec {
                r#type: pb::lifecycle_spec::Type::Bounded as i32,
                walltime: Some(prost_types::Duration {
                    seconds: 0,
                    nanos: 0,
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = allocation_from_proto(&spec, "alice");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("walltime"),
            "error should mention walltime"
        );
    }

    #[test]
    fn reject_negative_walltime_for_bounded() {
        let spec = pb::AllocationSpec {
            tenant: "physics".to_string(),
            entrypoint: "./run.sh".to_string(),
            resources: Some(pb::ResourceSpec {
                min_nodes: 1,
                ..Default::default()
            }),
            lifecycle: Some(pb::LifecycleSpec {
                r#type: pb::lifecycle_spec::Type::Bounded as i32,
                walltime: Some(prost_types::Duration {
                    seconds: -10,
                    nanos: 0,
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = allocation_from_proto(&spec, "alice");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("walltime"),
            "error should mention walltime"
        );
    }

    #[test]
    fn reject_preemption_class_above_10() {
        let spec = pb::AllocationSpec {
            tenant: "physics".to_string(),
            entrypoint: "./run.sh".to_string(),
            resources: Some(pb::ResourceSpec {
                min_nodes: 1,
                ..Default::default()
            }),
            lifecycle: Some(pb::LifecycleSpec {
                preemption_class: 11,
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = allocation_from_proto(&spec, "alice");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("preemption_class"),
            "error should mention preemption_class"
        );
    }

    #[test]
    fn reject_reactive_min_nodes_greater_than_max_nodes() {
        let spec = pb::AllocationSpec {
            tenant: "physics".to_string(),
            entrypoint: "./run.sh".to_string(),
            resources: Some(pb::ResourceSpec {
                min_nodes: 1,
                ..Default::default()
            }),
            lifecycle: Some(pb::LifecycleSpec {
                r#type: pb::lifecycle_spec::Type::Reactive as i32,
                reactive_min_nodes: 5,
                reactive_max_nodes: 2,
                preemption_class: 3,
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = allocation_from_proto(&spec, "alice");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("reactive") && err.contains("min_nodes"),
            "error should mention reactive min_nodes, got: {err}"
        );
    }

    #[test]
    fn reject_service_endpoint_port_zero() {
        let spec = pb::AllocationSpec {
            tenant: "physics".to_string(),
            entrypoint: "./run.sh".to_string(),
            resources: Some(pb::ResourceSpec {
                min_nodes: 1,
                ..Default::default()
            }),
            connectivity: Some(pb::ConnectivitySpec {
                expose: vec![pb::EndpointSpec {
                    name: "http".to_string(),
                    port: 0,
                    protocol: "tcp".to_string(),
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = allocation_from_proto(&spec, "alice");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("port"),
            "error should mention port"
        );
    }

    #[test]
    fn reject_service_endpoint_port_above_65535() {
        let spec = pb::AllocationSpec {
            tenant: "physics".to_string(),
            entrypoint: "./run.sh".to_string(),
            resources: Some(pb::ResourceSpec {
                min_nodes: 1,
                ..Default::default()
            }),
            connectivity: Some(pb::ConnectivitySpec {
                expose: vec![pb::EndpointSpec {
                    name: "http".to_string(),
                    port: 70000,
                    protocol: "tcp".to_string(),
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = allocation_from_proto(&spec, "alice");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("port"),
            "error should mention port"
        );
    }

    #[test]
    fn reject_max_requeue_above_100() {
        let spec = pb::AllocationSpec {
            tenant: "physics".to_string(),
            entrypoint: "./run.sh".to_string(),
            resources: Some(pb::ResourceSpec {
                min_nodes: 1,
                ..Default::default()
            }),
            max_requeue: 101,
            ..Default::default()
        };
        let result = allocation_from_proto(&spec, "alice");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("max_requeue"),
            "error should mention max_requeue"
        );
    }
}
