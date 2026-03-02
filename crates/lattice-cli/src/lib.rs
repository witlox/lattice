//! # lattice-cli
//!
//! CLI binary for the Lattice distributed workload scheduler.
//! Provides `lattice submit`, `lattice status`, `lattice cancel`,
//! `lattice session`, `lattice nodes`, and admin subcommands.
//!
//! Supports Slurm compatibility via `#SBATCH` directive parsing.

pub mod client;
pub mod commands;
pub mod compat;
pub mod completions;
pub mod config_file;
pub mod output;
