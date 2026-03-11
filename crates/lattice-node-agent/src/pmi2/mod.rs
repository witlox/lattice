//! PMI-2 wire protocol implementation for MPI process management.
//!
//! Provides a native PMI-2 server that runs over Unix domain sockets,
//! enabling MPI rank discovery and key-value exchange without SSH.
//!
//! See docs/architecture/mpi-process-management.md for the full design.

pub mod fence;
pub mod kvs;
pub mod protocol;
pub mod server;

pub use fence::FenceCoordinator;
pub use kvs::LocalKvs;
pub use protocol::{Pmi2Command, Pmi2Response};
pub use server::Pmi2Server;
