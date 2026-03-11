fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = "../../proto";

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[
                format!("{proto_root}/lattice/v1/allocations.proto"),
                format!("{proto_root}/lattice/v1/nodes.proto"),
                format!("{proto_root}/lattice/v1/admin.proto"),
                format!("{proto_root}/lattice/v1/agent.proto"),
                format!("{proto_root}/lattice/v1/mpi.proto"),
            ],
            &[proto_root.to_string()],
        )?;

    Ok(())
}
