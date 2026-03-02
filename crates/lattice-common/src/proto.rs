//! Generated protobuf types and gRPC service definitions.
//!
//! This module re-exports all types generated from the proto files:
//! - `allocations.proto` → `AllocationService` (18 RPCs)
//! - `nodes.proto` → `NodeService` (5 RPCs)
//! - `admin.proto` → `AdminService` (6 RPCs)

pub mod lattice {
    pub mod v1 {
        tonic::include_proto!("lattice.v1");
    }
}

pub use lattice::v1::*;
