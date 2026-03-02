fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = "../../proto";

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[
                format!("{proto_root}/lattice/v1/allocations.proto"),
                format!("{proto_root}/lattice/v1/nodes.proto"),
                format!("{proto_root}/lattice/v1/admin.proto"),
                format!("{proto_root}/lattice/v1/agent.proto"),
            ],
            &[proto_root],
        )?;

    Ok(())
}
