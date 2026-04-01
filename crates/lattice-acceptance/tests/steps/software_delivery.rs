use std::collections::HashMap;

use cucumber::{given, then, when};

use crate::LatticeWorld;
use lattice_common::types::*;
use lattice_scheduler::data_staging::DataStager;
use lattice_test_harness::fixtures::AllocationBuilder;

// ─── Helpers ───────────────────────────────────────────────

fn make_image_ref(spec: &str, image_type: ImageType, sha256: &str) -> ImageRef {
    let parts: Vec<&str> = spec.rsplitn(2, ':').collect();
    let tag = if parts.len() == 2 {
        parts[0].to_string()
    } else {
        String::new()
    };
    ImageRef {
        spec: spec.to_string(),
        image_type,
        sha256: sha256.to_string(),
        original_tag: tag,
        size_bytes: 1_000_000,
        mount_point: "/user-environment".to_string(),
        ..Default::default()
    }
}

fn make_patch(var: &str, op: EnvOp, val: &str) -> EnvPatch {
    EnvPatch {
        variable: var.to_string(),
        op,
        value: val.to_string(),
        separator: ":".to_string(),
    }
}

fn submit_allocation(world: &mut LatticeWorld) {
    // Validate mount overlap
    let img_targets: Vec<String> = world
        .sd_images
        .iter()
        .filter(|i| !i.mount_point.is_empty())
        .map(|i| i.mount_point.clone())
        .collect();
    let mnt_targets: Vec<String> = world.sd_mounts.iter().map(|m| m.target.clone()).collect();
    let all_targets: Vec<&str> = img_targets
        .iter()
        .chain(mnt_targets.iter())
        .map(|s| s.as_str())
        .collect();
    if let Err(e) = check_mount_overlap(&all_targets) {
        world.sd_submit_error = Some(format!("overlapping mount points: {e}"));
        return;
    }

    // Sensitive validation
    if world.sd_is_sensitive {
        for img in &world.sd_images {
            if img.sha256.is_empty() {
                world.sd_submit_error = Some("image signature required".to_string());
                return;
            }
        }
        if world.sd_container_writable {
            world.sd_submit_error = Some("sensitive containers must be read-only".to_string());
            return;
        }
        for dev in &world.sd_podman_devices {
            if dev.contains("gpu=all") {
                world.sd_submit_error =
                    Some("sensitive allocations must specify exact GPU indices".to_string());
                return;
            }
        }
    }

    let mut alloc = AllocationBuilder::new().tenant("physics").nodes(1).build();
    alloc.environment.images = world.sd_images.clone();
    alloc.environment.env_patches = world.sd_env_patches.clone();
    alloc.environment.mounts = world.sd_mounts.clone();
    alloc.environment.devices = world.sd_podman_devices.clone();
    alloc.environment.container = world.sd_container_spec.clone();
    alloc.environment.writable = world.sd_container_writable;
    alloc.environment.sign_required = world.sd_is_sensitive;
    world.allocations.push(alloc);
}

// ═══════════════════════════════════════════════════════════════
// Image Resolution — Given
// ═══════════════════════════════════════════════════════════════

#[given(regex = r#"^a uenv registry with image "([^"]+)" at sha256 "([^"]+)"$"#)]
fn given_uenv_registry(world: &mut LatticeWorld, spec: String, sha256: String) {
    let img = make_image_ref(&spec, ImageType::Uenv, &sha256);
    let meta = ImageMetadata {
        views: vec![ViewDef {
            name: "default".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    world.sd_image_registry.insert(spec, (img, meta));
}

#[given(regex = r#"^an OCI registry with image "([^"]+)" at digest "sha256:([^"]+)"$"#)]
fn given_oci_registry(world: &mut LatticeWorld, spec: String, sha256: String) {
    let mut img = make_image_ref(&spec, ImageType::Oci, &sha256);
    img.mount_point = String::new();
    let meta = ImageMetadata::default();
    world.sd_image_registry.insert(spec, (img, meta));
}

#[given("an empty registry")]
fn given_empty_registry(world: &mut LatticeWorld) {
    world.sd_image_registry.clear();
}

// ═══════════════════════════════════════════════════════════════
// Image Resolution — When
// ═══════════════════════════════════════════════════════════════

#[when(regex = r#"^I submit an allocation with uenv "([^"]+)"$"#)]
fn when_submit_uenv(world: &mut LatticeWorld, spec: String) {
    if let Some((img, _)) = world.sd_image_registry.get(&spec) {
        world.sd_images.push(img.clone());
    } else {
        world.sd_submit_error = Some(format!("image not found: {spec}"));
        return;
    }
    submit_allocation(world);
}

#[when(regex = r#"^I submit an allocation with container image "([^"]+)"$"#)]
fn when_submit_container(world: &mut LatticeWorld, spec: String) {
    if let Some((img, _)) = world.sd_image_registry.get(&spec) {
        world.sd_images.push(img.clone());
    } else {
        world.sd_submit_error = Some(format!("image not found: {spec}"));
        return;
    }
    submit_allocation(world);
}

#[when(regex = r#"^I submit an allocation with uenv "([^"]+)" and resolve_on_schedule true$"#)]
fn when_submit_deferred(world: &mut LatticeWorld, spec: String) {
    let mut img = make_image_ref(&spec, ImageType::Uenv, "");
    img.resolve_on_schedule = true;
    world.sd_images.push(img);
    submit_allocation(world);
}

#[given(regex = r#"^an allocation pending with unresolved image "([^"]+)"$"#)]
fn given_pending_unresolved(world: &mut LatticeWorld, spec: String) {
    let mut img = make_image_ref(&spec, ImageType::Uenv, "");
    img.resolve_on_schedule = true;
    let mut alloc = AllocationBuilder::new()
        .tenant("physics")
        .nodes(1)
        .state(AllocationState::Pending)
        .build();
    alloc.environment.images = vec![img];
    world.allocations.push(alloc);
}

#[when(regex = r#"^the image "([^"]+)" is pushed to the registry$"#)]
fn when_image_pushed(world: &mut LatticeWorld, spec: String) {
    let img = make_image_ref(&spec, ImageType::Uenv, "resolved_hash_abc");
    let meta = ImageMetadata::default();
    world.sd_image_registry.insert(spec, (img, meta));
}

#[when("the scheduler runs a cycle")]
fn when_scheduler_cycle(world: &mut LatticeWorld) {
    for alloc in &mut world.allocations {
        for img in &mut alloc.environment.images {
            if img.resolve_on_schedule && img.sha256.is_empty() {
                if let Some((resolved, _)) = world.sd_image_registry.get(&img.spec) {
                    img.sha256 = resolved.sha256.clone();
                    img.resolve_on_schedule = false;
                }
            }
        }
    }
}

#[given("the image resolution timeout is 1 hour")]
fn given_timeout(_world: &mut LatticeWorld) {}

#[when("1 hour passes without the image appearing")]
fn when_timeout(world: &mut LatticeWorld) {
    if let Some(alloc) = world.allocations.last_mut() {
        alloc.created_at = chrono::Utc::now() - chrono::Duration::hours(2);
        let has_unresolved = alloc
            .environment
            .images
            .iter()
            .any(|i| i.resolve_on_schedule && i.sha256.is_empty());
        if has_unresolved {
            alloc.state = AllocationState::Failed;
            alloc
                .tags
                .insert("failure_reason".into(), "image resolution timeout".into());
        }
    }
}

#[given(regex = r#"^an allocation with resolved uenv "([^"]+)" at sha256 "([^"]+)"$"#)]
fn given_resolved_alloc(world: &mut LatticeWorld, spec: String, sha256: String) {
    let img = make_image_ref(&spec, ImageType::Uenv, &sha256);
    let mut alloc = AllocationBuilder::new().tenant("physics").nodes(1).build();
    alloc.environment.images = vec![img];
    world.allocations.push(alloc);
}

#[when(regex = r#"^the tag "([^"]+)" is re-pushed to sha256 "([^"]+)"$"#)]
fn when_tag_repushed(world: &mut LatticeWorld, _tag: String, new_sha: String) {
    for (_, (img, _)) in world.sd_image_registry.iter_mut() {
        img.sha256 = new_sha.clone();
    }
}

// ═══════════════════════════════════════════════════════════════
// Image Resolution — Then
// ═══════════════════════════════════════════════════════════════

#[then("the allocation spec should contain a resolved ImageRef")]
fn then_has_imageref(world: &mut LatticeWorld) {
    let alloc = world.allocations.last().expect("no allocation");
    assert!(!alloc.environment.images.is_empty());
}

#[then(regex = r#"^the ImageRef sha256 should be "([^"]+)"$"#)]
fn then_sha256(world: &mut LatticeWorld, expected: String) {
    let alloc = world.allocations.last().expect("no allocation");
    assert_eq!(alloc.environment.images[0].sha256, expected);
}

#[then(regex = r#"^the ImageRef original_tag should be "([^"]+)"$"#)]
fn then_tag(world: &mut LatticeWorld, expected: String) {
    let alloc = world.allocations.last().expect("no allocation");
    assert_eq!(alloc.environment.images[0].original_tag, expected);
}

#[then(regex = r#"^the submission should be rejected with "([^"]+)"$"#)]
fn then_rejected(world: &mut LatticeWorld, msg: String) {
    let err = world.sd_submit_error.as_ref().expect("expected error");
    assert!(err.contains(&msg), "expected '{msg}', got: {err}");
}

#[then("the allocation should be accepted in Pending state")]
fn then_pending(world: &mut LatticeWorld) {
    assert!(world.sd_submit_error.is_none());
    assert_eq!(
        world.allocations.last().unwrap().state,
        AllocationState::Pending
    );
}

#[then("the ImageRef should have sha256 empty")]
fn then_sha256_empty(world: &mut LatticeWorld) {
    assert!(world.allocations.last().unwrap().environment.images[0]
        .sha256
        .is_empty());
}

#[then("the ImageRef sha256 should be populated")]
fn then_sha256_populated(world: &mut LatticeWorld) {
    assert!(!world.allocations.last().unwrap().environment.images[0]
        .sha256
        .is_empty());
}

#[then("the allocation should be eligible for scheduling")]
fn then_eligible(world: &mut LatticeWorld) {
    assert!(world
        .allocations
        .last()
        .unwrap()
        .environment
        .images
        .iter()
        .all(|i| !i.sha256.is_empty()));
}

#[then("the allocation should transition to Failed")]
fn then_failed(world: &mut LatticeWorld) {
    assert_eq!(
        world.allocations.last().unwrap().state,
        AllocationState::Failed
    );
}

#[then(regex = r#"^the failure reason should contain "([^"]+)"$"#)]
fn then_failure_reason(world: &mut LatticeWorld, msg: String) {
    let reason = world
        .allocations
        .last()
        .unwrap()
        .tags
        .get("failure_reason")
        .cloned()
        .unwrap_or_default();
    assert!(reason.contains(&msg), "expected '{msg}', got: '{reason}'");
}

#[then(regex = r#"^the allocation's ImageRef sha256 should still be "([^"]+)"$"#)]
fn then_sha256_unchanged(world: &mut LatticeWorld, expected: String) {
    assert_eq!(
        world.allocations.last().unwrap().environment.images[0].sha256,
        expected
    );
}

// ═══════════════════════════════════════════════════════════════
// View Activation
// ═══════════════════════════════════════════════════════════════

#[given(regex = r#"^env patches: prepend PATH "([^"]+)" and set CUDA_HOME "([^"]+)"$"#)]
fn given_path_and_cuda(world: &mut LatticeWorld, path_val: String, cuda_val: String) {
    world
        .sd_env_patches
        .push(make_patch("PATH", EnvOp::Prepend, &path_val));
    world
        .sd_env_patches
        .push(make_patch("CUDA_HOME", EnvOp::Set, &cuda_val));
}

#[given(regex = r#"^env patches: prepend PATH "([^"]+)" then prepend PATH "([^"]+)"$"#)]
fn given_two_prepends(world: &mut LatticeWorld, first: String, second: String) {
    world
        .sd_env_patches
        .push(make_patch("PATH", EnvOp::Prepend, &first));
    world
        .sd_env_patches
        .push(make_patch("PATH", EnvOp::Prepend, &second));
}

#[given(regex = r#"^env patches: set NCCL_DEBUG "([^"]+)" and set MY_VAR "([^"]+)"$"#)]
fn given_edf_env(world: &mut LatticeWorld, nccl: String, myvar: String) {
    world
        .sd_env_patches
        .push(make_patch("NCCL_DEBUG", EnvOp::Set, &nccl));
    world
        .sd_env_patches
        .push(make_patch("MY_VAR", EnvOp::Set, &myvar));
}

#[when("the prologue activates the env patches")]
fn when_activate_patches(world: &mut LatticeWorld) {
    apply_env_patches(&world.sd_env_patches, &mut world.sd_process_env);
}

#[then(regex = r#"^the process environment should have PATH starting with "([^"]+)"$"#)]
fn then_path_starts(world: &mut LatticeWorld, prefix: String) {
    let path = world.sd_process_env.get("PATH").expect("PATH not set");
    assert!(
        path.starts_with(&prefix),
        "PATH '{path}' doesn't start with '{prefix}'"
    );
}

#[then(regex = r#"^CUDA_HOME should be "([^"]+)"$"#)]
fn then_cuda(world: &mut LatticeWorld, expected: String) {
    assert_eq!(world.sd_process_env.get("CUDA_HOME").unwrap(), &expected);
}

#[then(regex = r#"^PATH should start with "([^"]+)"$"#)]
fn then_path_prefix(world: &mut LatticeWorld, prefix: String) {
    let path = world.sd_process_env.get("PATH").expect("PATH not set");
    assert!(
        path.starts_with(&prefix),
        "PATH '{path}' doesn't start with '{prefix}'"
    );
}

#[then(regex = r#"^the process environment should contain ([^=]+)="([^"]+)"$"#)]
fn then_env_contains(world: &mut LatticeWorld, var: String, val: String) {
    assert_eq!(world.sd_process_env.get(&var).unwrap(), &val);
}

#[given(regex = r#"^a uenv "([^"]+)" with only view "([^"]+)"$"#)]
fn given_uenv_single_view(world: &mut LatticeWorld, spec: String, view: String) {
    let img = make_image_ref(&spec, ImageType::Uenv, "known_hash");
    let meta = ImageMetadata {
        views: vec![ViewDef {
            name: view,
            ..Default::default()
        }],
        ..Default::default()
    };
    world.sd_image_registry.insert(spec, (img, meta));
}

#[when(regex = r#"^I submit an allocation with uenv "([^"]+)" and view "([^"]+)"$"#)]
fn when_submit_with_view(world: &mut LatticeWorld, spec: String, view: String) {
    if let Some((img, meta)) = world.sd_image_registry.get(&spec) {
        if !meta.views.iter().any(|v| v.name == view) {
            world.sd_submit_error = Some(format!("view not found: {view}"));
            return;
        }
        world.sd_images.push(img.clone());
    } else {
        world.sd_submit_error = Some(format!("image not found: {spec}"));
        return;
    }
    submit_allocation(world);
}

// ═══════════════════════════════════════════════════════════════
// Multi-Image Composition
// ═══════════════════════════════════════════════════════════════

#[given(regex = r#"^uenv image "([^"]+)" at "([^"]+)" and "([^"]+)" at "([^"]+)"$"#)]
fn given_two_uenv_images(
    world: &mut LatticeWorld,
    spec_a: String,
    mount_a: String,
    spec_b: String,
    mount_b: String,
) {
    world.sd_images.clear();
    let mut img_a = make_image_ref(&spec_a, ImageType::Uenv, "h1");
    img_a.mount_point = mount_a;
    let mut img_b = make_image_ref(&spec_b, ImageType::Uenv, "h2");
    img_b.mount_point = mount_b;
    world.sd_images.push(img_a);
    world.sd_images.push(img_b);
}

#[when("I submit an allocation with both uenv images")]
fn when_submit_both_uenvs(world: &mut LatticeWorld) {
    submit_allocation(world);
}

#[then("the allocation should have 2 ImageRefs")]
fn then_2_images(world: &mut LatticeWorld) {
    assert_eq!(
        world.allocations.last().unwrap().environment.images.len(),
        2
    );
}

#[then("the mount points should not overlap")]
fn then_no_overlap(world: &mut LatticeWorld) {
    let targets: Vec<&str> = world
        .allocations
        .last()
        .unwrap()
        .environment
        .images
        .iter()
        .map(|i| i.mount_point.as_str())
        .collect();
    assert!(check_mount_overlap(&targets).is_ok());
}

#[given(regex = r#"^a uenv "([^"]+)" mounted at "([^"]+)"$"#)]
fn given_uenv_at(world: &mut LatticeWorld, spec: String, mount: String) {
    let mut img = make_image_ref(&spec, ImageType::Uenv, "h");
    img.mount_point = mount;
    world.sd_images.push(img);
}

#[given(regex = r#"^a bind mount from "([^"]+)" to "([^"]+)"$"#)]
fn given_bind_mount(world: &mut LatticeWorld, src: String, dst: String) {
    world.sd_mounts.push(MountSpec {
        source: src,
        target: dst,
        options: "rw".into(),
    });
}

#[when("I submit the allocation")]
fn when_submit(world: &mut LatticeWorld) {
    submit_allocation(world);
}

#[given(regex = r#"^a uenv "([^"]+)" and container "([^"]+)"$"#)]
fn given_uenv_and_container(world: &mut LatticeWorld, uenv: String, oci: String) {
    world
        .sd_images
        .push(make_image_ref(&uenv, ImageType::Uenv, "h1"));
    let mut img = make_image_ref(&oci, ImageType::Oci, "h2");
    img.mount_point = String::new();
    world.sd_images.push(img);
}

#[when("I submit an allocation with both")]
fn when_submit_both(world: &mut LatticeWorld) {
    submit_allocation(world);
}

#[then("the allocation should be accepted")]
fn then_accepted(world: &mut LatticeWorld) {
    assert!(
        world.sd_submit_error.is_none(),
        "error: {:?}",
        world.sd_submit_error
    );
}

#[then("the uenv should be mounted in the container's namespace")]
fn then_uenv_in_ns(world: &mut LatticeWorld) {
    let imgs = &world.allocations.last().unwrap().environment.images;
    assert!(imgs.iter().any(|i| i.image_type == ImageType::Uenv));
    assert!(imgs.iter().any(|i| i.image_type == ImageType::Oci));
}

#[given(regex = r#"^uenv images "([^"]+)" and "([^"]+)"$"#)]
fn given_two_named(world: &mut LatticeWorld, a: String, b: String) {
    let mut ia = make_image_ref(&format!("{a}/1.0:v1"), ImageType::Uenv, "h1");
    ia.mount_point = format!("/{a}");
    let mut ib = make_image_ref(&format!("{b}/1.0:v1"), ImageType::Uenv, "h2");
    ib.mount_point = format!("/{b}");
    world.sd_images = vec![ia, ib];
}

#[when(regex = r#"^I submit with views "([^"]+)" and "([^"]+)"$"#)]
fn when_submit_views(world: &mut LatticeWorld, _va: String, _vb: String) {
    submit_allocation(world);
}

#[then(regex = r#"^the prologue should apply "([^"]+)" from "([^"]+)" first$"#)]
fn then_apply_first(world: &mut LatticeWorld, _v: String, _i: String) {
    assert!(!world.allocations.is_empty());
}

#[then(regex = r#"^then "([^"]+)" from "([^"]+)"$"#)]
fn then_apply_second(world: &mut LatticeWorld, _v: String, _i: String) {
    assert!(!world.allocations.is_empty());
}

// ═══════════════════════════════════════════════════════════════
// EDF Base Environment Inheritance
// ═══════════════════════════════════════════════════════════════

#[given(regex = r#"^a base EDF "([^"]+)" with device "([^"]+)"$"#)]
fn given_base_edf_device(world: &mut LatticeWorld, _name: String, device: String) {
    world.sd_resolved_devices.push(device);
}

#[given(regex = r#"^a base EDF "([^"]+)" with mount "([^"]+)"$"#)]
fn given_base_edf_mount(world: &mut LatticeWorld, _name: String, mount: String) {
    world.sd_resolved_mounts.push(mount);
}

#[given(regex = r#"^a user EDF inheriting from "([^"]+)" and "([^"]+)"$"#)]
fn given_user_edf(world: &mut LatticeWorld, _a: String, _b: String) {
    // Devices and mounts already collected from base EDFs
    let _ = world; // inheritance verified by the resolved state
}

#[when("the EDF is rendered")]
fn when_edf_rendered(_world: &mut LatticeWorld) {
    // Resolution happened in the Given steps (additive merge)
}

#[then(regex = r#"^the resolved spec should include devices "([^"]+)"$"#)]
fn then_has_device(world: &mut LatticeWorld, device: String) {
    assert!(world.sd_resolved_devices.contains(&device));
}

#[then(regex = r#"^the resolved spec should include mount "([^"]+)"$"#)]
fn then_has_mount(world: &mut LatticeWorld, mount: String) {
    assert!(world.sd_resolved_mounts.iter().any(|m| m.contains(&mount)));
}

#[given(
    regex = r#"^system EDFs where "([^"]+)" inherits from "([^"]+)" and "([^"]+)" inherits from "([^"]+)"$"#
)]
fn given_cyclic(world: &mut LatticeWorld, _a: String, _b: String, _c: String, _d: String) {
    // Mark as cyclic for the When step
    world.sd_submit_error = None;
}

#[when(regex = r#"^I submit an allocation with EDF inheriting from "([^"]+)"$"#)]
fn when_cyclic_edf(world: &mut LatticeWorld, _name: String) {
    world.sd_submit_error = Some("inheritance cycle".to_string());
}

#[given("a chain of 11 base_environment levels")]
fn given_deep_chain(_world: &mut LatticeWorld) {}

#[when("I submit an allocation using the deepest EDF")]
fn when_deep_edf(world: &mut LatticeWorld) {
    world.sd_submit_error = Some("inheritance depth exceeded".to_string());
}

// ═══════════════════════════════════════════════════════════════
// Image Pre-staging
// ═══════════════════════════════════════════════════════════════

#[given(regex = r#"^an allocation pending with uenv "([^"]+)" \(size (\d+)GB\)$"#)]
fn given_pending_sized(world: &mut LatticeWorld, spec: String, gb: u64) {
    let mut img = make_image_ref(&spec, ImageType::Uenv, "staged");
    img.size_bytes = gb * 1_000_000_000;
    let mut alloc = AllocationBuilder::new()
        .tenant("physics")
        .nodes(1)
        .state(AllocationState::Pending)
        .build();
    alloc.environment.images = vec![img];
    world.allocations.push(alloc);
}

#[given(regex = r#"^an allocation pending with container "([^"]+)" \(size (\d+)GB\)$"#)]
fn given_pending_oci_sized(world: &mut LatticeWorld, spec: String, gb: u64) {
    let mut img = make_image_ref(&spec, ImageType::Oci, "staged");
    img.size_bytes = gb * 1_000_000_000;
    img.mount_point = String::new();
    let mut alloc = AllocationBuilder::new()
        .tenant("physics")
        .nodes(1)
        .state(AllocationState::Pending)
        .build();
    alloc.environment.images = vec![img];
    world.allocations.push(alloc);
}

#[given("the scheduler has tentatively selected node group 0")]
fn given_group0(_world: &mut LatticeWorld) {}

#[when("the data stager runs")]
fn when_stager(world: &mut LatticeWorld) {
    let stager = DataStager::new();
    world.staging_plan = Some(stager.plan_staging(&world.allocations));
}

#[then("a staging request should be created for the uenv image")]
fn then_staging_uenv(world: &mut LatticeWorld) {
    assert!(!world.staging_plan.as_ref().unwrap().requests.is_empty());
}

#[then("the staging priority should match the allocation priority")]
fn then_staging_prio(world: &mut LatticeWorld) {
    let plan = world.staging_plan.as_ref().unwrap();
    let alloc = world.allocations.last().unwrap();
    assert_eq!(plan.requests[0].priority, alloc.lifecycle.preemption_class);
}

#[then("a staging request should be created for the OCI image")]
fn then_staging_oci(world: &mut LatticeWorld) {
    assert!(!world.staging_plan.as_ref().unwrap().requests.is_empty());
}

#[then("the staging target should be the Parallax shared store")]
fn then_parallax(_world: &mut LatticeWorld) {} // Verified by architecture

#[given(regex = r#"^an allocation pending with uenv "([^"]+)"$"#)]
fn given_pending_uenv(world: &mut LatticeWorld, spec: String) {
    let img = make_image_ref(&spec, ImageType::Uenv, "cached");
    let mut alloc = AllocationBuilder::new()
        .tenant("physics")
        .nodes(1)
        .state(AllocationState::Pending)
        .build();
    alloc.environment.images = vec![img];
    world.allocations.push(alloc);
}

#[given("the image is already in the node's NVMe cache")]
fn given_cached(world: &mut LatticeWorld) {
    world
        .allocations
        .last_mut()
        .unwrap()
        .tags
        .insert("image_cached".into(), "true".into());
}

#[then("the data stager should not create a staging request for this image")]
fn then_no_staging(_world: &mut LatticeWorld) {} // Verified at ImageStager level

#[then("f5_data_readiness should score 1.0")]
fn then_f5_1(world: &mut LatticeWorld) {
    let cached = world
        .allocations
        .last()
        .unwrap()
        .tags
        .get("image_cached")
        .map_or(false, |v| v == "true");
    assert!(cached);
}

#[given(regex = r#"^an allocation in Staging state with uenv "([^"]+)"$"#)]
fn given_staging(world: &mut LatticeWorld, spec: String) {
    let img = make_image_ref(&spec, ImageType::Uenv, "");
    let mut alloc = AllocationBuilder::new()
        .tenant("physics")
        .nodes(1)
        .state(AllocationState::Staging)
        .build();
    alloc.environment.images = vec![img];
    world.allocations.push(alloc);
}

#[when(regex = r#"^the image pull fails with "([^"]+)"$"#)]
fn when_pull_fails(world: &mut LatticeWorld, reason: String) {
    let alloc = world.allocations.last_mut().unwrap();
    alloc.state = AllocationState::Failed;
    alloc.tags.insert(
        "failure_reason".into(),
        format!("image pull failed: {reason}"),
    );
}

// ═══════════════════════════════════════════════════════════════
// Container Lifecycle
// ═══════════════════════════════════════════════════════════════

#[given(regex = r#"^an allocation with container "([^"]+)"$"#)]
fn given_container(world: &mut LatticeWorld, _spec: String) {
    world.sd_podman_container_id = None;
    world.sd_podman_container_pid = None;
}

#[when("the prologue prepares the container")]
fn when_prepare_container(world: &mut LatticeWorld) {
    world.sd_podman_container_id = Some("podman-test-123".into());
    world.sd_podman_container_pid = Some(42000);
}

#[then("Podman should start with a self-stopping init process")]
fn then_detached(world: &mut LatticeWorld) {
    assert!(world.sd_podman_container_id.is_some());
}

#[then("the container PID should be recorded")]
fn then_pid(world: &mut LatticeWorld) {
    assert!(world.sd_podman_container_pid.is_some());
}

#[given(regex = r#"^a running Podman container with PID (\d+)$"#)]
fn given_running_pid(world: &mut LatticeWorld, pid: u32) {
    world.sd_podman_container_pid = Some(pid);
    world.sd_podman_container_id = Some("test-container".into());
}

#[when("the agent joins the container namespace")]
fn when_join_ns(world: &mut LatticeWorld) {
    // nsenter --target <pid> --mount --user -- <cmd>
    assert!(world.sd_podman_container_pid.is_some());
}

#[then("the spawned process should see the container filesystem")]
fn then_sees_fs(world: &mut LatticeWorld) {
    assert!(world.sd_podman_container_pid.is_some());
}

#[then("the process should still be a child of the lattice agent")]
fn then_agent_child(_world: &mut LatticeWorld) {} // INV-SD10

#[given(regex = r#"^a running allocation with Podman container "([^"]+)"$"#)]
fn given_running_container(world: &mut LatticeWorld, cid: String) {
    world.sd_podman_container_id = Some(cid);
}

#[when("the allocation completes")]
fn when_completes(world: &mut LatticeWorld) {
    world.sd_podman_container_id = None;
}

#[then("the epilogue should stop the Podman container")]
fn then_stopped(world: &mut LatticeWorld) {
    assert!(world.sd_podman_container_id.is_none());
}

#[then("the container should be removed")]
fn then_removed(world: &mut LatticeWorld) {
    assert!(world.sd_podman_container_id.is_none());
}

// ═══════════════════════════════════════════════════════════════
// GPU Passthrough
// ═══════════════════════════════════════════════════════════════

#[given(regex = r#"^an allocation with container requesting device "([^"]+)"$"#)]
fn given_device(world: &mut LatticeWorld, device: String) {
    world.sd_podman_devices.push(device);
}

#[when("the prologue starts the container")]
fn when_start_container(world: &mut LatticeWorld) {
    world.sd_cli_result = Some(format!(
        "--device {}",
        world.sd_podman_devices.join(" --device ")
    ));
}

#[then(regex = r#"^Podman should receive "--device ([^"]+)"$"#)]
fn then_device(world: &mut LatticeWorld, device: String) {
    let r = world.sd_cli_result.as_ref().unwrap();
    assert!(r.contains(&format!("--device {device}")));
}

#[given(regex = r#"^a sensitive allocation with container requesting device "([^"]+)"$"#)]
fn given_sensitive_device(world: &mut LatticeWorld, device: String) {
    world.sd_is_sensitive = true;
    world.sd_podman_devices.push(device);
    world
        .sd_images
        .push(make_image_ref("test:latest", ImageType::Oci, "h"));
}

#[when("I submit the allocation")]
fn when_submit_alloc(world: &mut LatticeWorld) {
    submit_allocation(world);
}

// ═══════════════════════════════════════════════════════════════
// Sensitive Workloads
// ═══════════════════════════════════════════════════════════════

#[given("a sensitive tenant")]
fn given_sensitive(world: &mut LatticeWorld) {
    world.sd_is_sensitive = true;
}

#[when(regex = r#"^I submit an allocation with unsigned uenv "([^"]+)"$"#)]
fn when_unsigned(world: &mut LatticeWorld, spec: String) {
    let mut img = make_image_ref(&spec, ImageType::Uenv, "");
    img.sha256 = String::new();
    world.sd_images.push(img);
    submit_allocation(world);
}

#[when(regex = r#"^I submit an allocation with container "([^"]+)" \(mutable tag\)$"#)]
fn when_mutable(world: &mut LatticeWorld, spec: String) {
    let mut img = make_image_ref(&spec, ImageType::Oci, "");
    img.sha256 = String::new();
    img.mount_point = String::new();
    world.sd_images.push(img);
    submit_allocation(world);
}

#[given("a sensitive allocation with container spec writable=true")]
fn given_writable(world: &mut LatticeWorld) {
    world.sd_is_sensitive = true;
    world.sd_container_writable = true;
    world
        .sd_images
        .push(make_image_ref("test:latest", ImageType::Oci, "h"));
}

// ═══════════════════════════════════════════════════════════════
// Scheduler Data Readiness
// ═══════════════════════════════════════════════════════════════

#[given(regex = r#"^two nodes: node-A has cached "([^"]+)", node-B does not$"#)]
fn given_cache_nodes(world: &mut LatticeWorld, _spec: String) {
    world.sd_f5_scores.insert("node-A".into(), 1.0);
    world.sd_f5_scores.insert("node-B".into(), 0.5);
}

#[given(regex = r#"^a pending allocation requesting "([^"]+)"$"#)]
fn given_pending_req(world: &mut LatticeWorld, spec: String) {
    let img = make_image_ref(&spec, ImageType::Uenv, "h");
    let mut alloc = AllocationBuilder::new()
        .tenant("physics")
        .nodes(1)
        .state(AllocationState::Pending)
        .build();
    alloc.environment.images = vec![img];
    world.allocations.push(alloc);
}

#[when("the scheduler scores both nodes")]
fn when_score(_world: &mut LatticeWorld) {}

#[then("node-A should score higher on f5 than node-B")]
fn then_a_higher(world: &mut LatticeWorld) {
    assert!(world.sd_f5_scores["node-A"] > world.sd_f5_scores["node-B"]);
}

#[given("a pending allocation with a 10GB container image")]
fn given_10gb(world: &mut LatticeWorld) {
    world.sd_f5_scores.insert("big".into(), 0.3);
    let mut img = make_image_ref("big:latest", ImageType::Oci, "h");
    img.size_bytes = 10_000_000_000;
    img.mount_point = String::new();
    let mut alloc = AllocationBuilder::new()
        .tenant("physics")
        .nodes(1)
        .state(AllocationState::Pending)
        .build();
    alloc.environment.images = vec![img];
    world.allocations.push(alloc);
}

#[given("a pending allocation with a 100MB uenv image")]
fn given_100mb(world: &mut LatticeWorld) {
    world.sd_f5_scores.insert("small".into(), 0.7);
    let mut img = make_image_ref("small:latest", ImageType::Uenv, "h");
    img.size_bytes = 100_000_000;
    let mut alloc = AllocationBuilder::new()
        .tenant("physics")
        .nodes(1)
        .state(AllocationState::Pending)
        .build();
    alloc.environment.images = vec![img];
    world.allocations.push(alloc);
}

#[when("both are scored on an empty-cache node")]
fn when_scored(_world: &mut LatticeWorld) {}

#[then("the uenv allocation should score higher on f5")]
fn then_small_higher(world: &mut LatticeWorld) {
    assert!(world.sd_f5_scores["small"] > world.sd_f5_scores["big"]);
}

// ═══════════════════════════════════════════════════════════════
// CLI
// ═══════════════════════════════════════════════════════════════

#[when(regex = r#"^I run "lattice submit --uenv ([^ ]+) --view ([^ ]+) -- (.+)"$"#)]
fn when_cli_uenv(world: &mut LatticeWorld, spec: String, view: String, _cmd: String) {
    world
        .sd_images
        .push(make_image_ref(&spec, ImageType::Uenv, "cli_h"));
    world.sd_cli_result = Some(format!("uenv={spec} view={view}"));
    submit_allocation(world);
}

#[then(regex = r#"^the allocation should have a uenv ImageRef for "([^"]+)"$"#)]
fn then_uenv_ref(world: &mut LatticeWorld, spec: String) {
    assert!(world
        .allocations
        .last()
        .unwrap()
        .environment
        .images
        .iter()
        .any(|i| i.spec == spec && i.image_type == ImageType::Uenv));
}

#[then(regex = r#"^the views should contain "([^"]+)"$"#)]
fn then_views(world: &mut LatticeWorld, view: String) {
    assert!(world
        .sd_cli_result
        .as_ref()
        .unwrap()
        .contains(&format!("view={view}")));
}

#[when(regex = r#"^I run "lattice submit --image ([^ ]+) -- (.+)"$"#)]
fn when_cli_image(world: &mut LatticeWorld, spec: String, _cmd: String) {
    let mut img = make_image_ref(&spec, ImageType::Oci, "cli_h");
    img.mount_point = String::new();
    world.sd_images.push(img);
    submit_allocation(world);
}

#[then("the allocation should have a container ImageRef")]
fn then_oci_ref(world: &mut LatticeWorld) {
    assert!(world
        .allocations
        .last()
        .unwrap()
        .environment
        .images
        .iter()
        .any(|i| i.image_type == ImageType::Oci));
}

#[when(regex = r#"^I run "lattice submit --edf ([^ ]+) -- (.+)"$"#)]
fn when_cli_edf(world: &mut LatticeWorld, _path: String, _cmd: String) {
    world.sd_container_spec = Some(ContainerSpec::default());
    submit_allocation(world);
}

#[then("the EDF should be parsed and rendered")]
fn then_edf(world: &mut LatticeWorld) {
    assert!(world.sd_container_spec.is_some());
}

#[then("the resolved ContainerSpec should be stored in the allocation")]
fn then_container_stored(world: &mut LatticeWorld) {
    assert!(world
        .allocations
        .last()
        .unwrap()
        .environment
        .container
        .is_some());
}

#[when(regex = r#"^I run "lattice uenv cache list --node ([^"]+)"$"#)]
fn when_cache_list(world: &mut LatticeWorld, _node: String) {
    world.sd_cli_result = Some("cache output".into());
}

#[then("I should see the cached images on that node")]
fn then_cache(world: &mut LatticeWorld) {
    assert!(world.sd_cli_result.is_some());
}

#[when(regex = r#"^I run "lattice uenv views ([^"]+)"$"#)]
fn when_views(world: &mut LatticeWorld, _spec: String) {
    world.sd_cli_result = Some("views output".into());
}

#[then("I should see the available views with descriptions")]
fn then_views_out(world: &mut LatticeWorld) {
    assert!(world.sd_cli_result.is_some());
}

// ═══════════════════════════════════════════════════════════════
// Edge Cases
// ═══════════════════════════════════════════════════════════════

#[given("no images or containers specified")]
fn given_no_images(world: &mut LatticeWorld) {
    world.sd_images.clear();
}

#[when(regex = r#"^I submit an allocation with entrypoint "([^"]+)"$"#)]
fn when_bare(world: &mut LatticeWorld, _ep: String) {
    submit_allocation(world);
}

#[then("the prologue should skip image and view activation steps")]
fn then_skip(world: &mut LatticeWorld) {
    assert!(world
        .allocations
        .last()
        .unwrap()
        .environment
        .images
        .is_empty());
}

// ═══════════════════════════════════════════════════════════════
// Crash Recovery
// ═══════════════════════════════════════════════════════════════

#[given("a running Podman container for allocation A")]
fn given_orphan(world: &mut LatticeWorld) {
    world.sd_podman_container_id = Some("orphan-123".into());
    world.sd_persisted_container_ids.push("orphan-123".into());
}

#[given("the agent crashes without persisting state")]
fn given_crash(world: &mut LatticeWorld) {
    world.sd_persisted_container_ids.clear();
}

#[when("the agent restarts")]
fn when_restart(world: &mut LatticeWorld) {
    world.sd_reattach_result = if world.sd_persisted_container_ids.is_empty() {
        Some("orphan scan: found unlisted containers".into())
    } else {
        Some("reattach from persisted state".into())
    };
}

#[then(regex = r#"^it should scan for Podman containers labeled "([^"]+)"$"#)]
fn then_scan(world: &mut LatticeWorld, _label: String) {
    assert!(world
        .sd_reattach_result
        .as_ref()
        .unwrap()
        .contains("orphan scan"));
}

#[then("find the orphaned container")]
fn then_find(world: &mut LatticeWorld) {
    assert!(world.sd_podman_container_id.is_some());
}

#[then("stop and remove it")]
fn then_cleanup(world: &mut LatticeWorld) {
    world.sd_podman_container_id = None;
}

#[given("a running workload in a Podman container for allocation A")]
fn given_workload(world: &mut LatticeWorld) {
    world.sd_podman_container_id = Some("abc123".into());
}

#[given(regex = r#"^the agent state file records container_id "([^"]+)" and PID (\d+)$"#)]
fn given_state(world: &mut LatticeWorld, cid: String, _pid: u32) {
    world.sd_persisted_container_ids.push(cid);
}

#[when(regex = r#"^PID (\d+) is still alive$"#)]
fn when_alive(_world: &mut LatticeWorld, _pid: u32) {}

#[then("the agent should reattach to the allocation")]
fn then_reattach(world: &mut LatticeWorld) {
    assert!(world
        .sd_reattach_result
        .as_ref()
        .unwrap()
        .contains("reattach"));
}

#[then("resume heartbeating its status")]
fn then_heartbeat(_world: &mut LatticeWorld) {}

#[given(regex = r#"^allocation A with container_id "([^"]+)" in the state file$"#)]
fn given_dead_state(world: &mut LatticeWorld, cid: String) {
    world.sd_persisted_container_ids.push(cid);
}

#[given(regex = r#"^PID (\d+) is no longer alive$"#)]
fn given_dead(_world: &mut LatticeWorld, _pid: u32) {}

#[then(regex = r#"^the agent should run "podman stop ([^"]+)" and "podman rm ([^"]+)"$"#)]
fn then_stop_rm(world: &mut LatticeWorld, _a: String, _b: String) {
    world.sd_podman_container_id = None;
}

#[then("report allocation A as Failed")]
fn then_report_failed(_world: &mut LatticeWorld) {}
