use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);

    // When built from a crates.io tarball, protos are vendored at {crate}/proto.
    // When built from the workspace, they are at {workspace_root}/proto.
    let local_proto = manifest_dir.join("proto");
    let workspace_proto = manifest_dir.join("../../proto");

    let proto_root = if local_proto.join("lattice/v1/allocations.proto").exists() {
        local_proto
    } else if workspace_proto
        .join("lattice/v1/allocations.proto")
        .exists()
    {
        workspace_proto
    } else {
        return Err("proto files not found in either ./proto or ../../proto".into());
    };

    let proto_root_str = proto_root.to_string_lossy();

    let protos: Vec<String> = [
        "lattice/v1/allocations.proto",
        "lattice/v1/nodes.proto",
        "lattice/v1/admin.proto",
        "lattice/v1/agent.proto",
        "lattice/v1/mpi.proto",
    ]
    .iter()
    .map(|p| format!("{proto_root_str}/{p}"))
    .collect();

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &protos.iter().map(String::as_str).collect::<Vec<_>>(),
            &[proto_root_str.as_ref()],
        )?;

    Ok(())
}
