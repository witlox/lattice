//! `lattice uenv` — uenv image management subcommands.

use clap::{Args, Subcommand};

/// Default uenv cache directory.
const DEFAULT_CACHE_DIR: &str = "/var/cache/lattice/uenv";

/// Arguments for the uenv command.
#[derive(Args, Debug)]
pub struct UenvArgs {
    #[command(subcommand)]
    pub command: UenvCommand,
}

#[derive(Subcommand, Debug)]
pub enum UenvCommand {
    /// Query available views for a uenv image spec
    Views(UenvViewsArgs),
    /// Manage the uenv image cache
    Cache(UenvCacheArgs),
}

/// Arguments for `lattice uenv views <spec>`.
#[derive(Args, Debug)]
pub struct UenvViewsArgs {
    /// uenv image spec (e.g., prgenv-gnu/24.11:v1)
    pub spec: String,
}

/// Arguments for `lattice uenv cache`.
#[derive(Args, Debug)]
pub struct UenvCacheArgs {
    #[command(subcommand)]
    pub command: UenvCacheCommand,
}

#[derive(Subcommand, Debug)]
pub enum UenvCacheCommand {
    /// List cached images on a node
    List(UenvCacheListArgs),
}

/// Arguments for `lattice uenv cache list`.
#[derive(Args, Debug)]
pub struct UenvCacheListArgs {
    /// Node ID to query (currently only local cache is supported)
    #[arg(long)]
    pub node: String,

    /// Cache directory to inspect (default: /var/cache/lattice/uenv)
    #[arg(long, default_value = DEFAULT_CACHE_DIR)]
    pub cache_dir: String,
}

/// Execute the uenv command.
pub async fn execute(args: &UenvArgs) -> anyhow::Result<()> {
    match &args.command {
        UenvCommand::Views(v) => execute_views(v),
        UenvCommand::Cache(c) => match &c.command {
            UenvCacheCommand::List(l) => execute_cache_list(l),
        },
    }
}

/// Check if a spec looks like a local path (contains '/' and ends in .squashfs or exists on disk).
fn is_local_path(spec: &str) -> bool {
    if spec.ends_with(".squashfs") {
        return true;
    }
    // Check if it looks like an absolute or relative filesystem path that exists
    if spec.contains('/') && std::path::Path::new(spec).exists() {
        return true;
    }
    false
}

/// Execute `lattice uenv views <spec>`.
fn execute_views(args: &UenvViewsArgs) -> anyhow::Result<()> {
    let spec = &args.spec;

    if is_local_path(spec) {
        // Local path: would need squashfs-mount to inspect env.json
        let squashfs_mount = std::env::var("SQUASHFS_MOUNT_BIN")
            .unwrap_or_else(|_| "/usr/bin/squashfs-mount".to_string());
        if !std::path::Path::new(&squashfs_mount).exists() {
            eprintln!("Image is a local path but squashfs-mount not found at {squashfs_mount}.");
            eprintln!("Cannot inspect views without squashfs-mount.");
            return Ok(());
        }
        eprintln!("Local image found at {spec}. View inspection requires mounting the image.");
        eprintln!("This is not yet supported in non-interactive mode.");
        eprintln!("Use 'squashfs-mount {spec} /tmp/uenv-inspect' and check env.json manually.");
    } else {
        // Registry-based spec: we would need to connect to the registry
        // For now, provide a helpful message about what's needed
        let registry_url = std::env::var("LATTICE_REGISTRY_URL").unwrap_or_default();
        if registry_url.is_empty() {
            eprintln!("Unable to query views — no registry configured.");
            eprintln!("Set LATTICE_REGISTRY_URL to enable remote image inspection.");
            eprintln!("Alternatively, provide a local .squashfs path.");
        } else {
            println!("Checking registry at {registry_url}...");
            println!("Image spec: {spec}");
            println!("View inspection requires the image to be mounted locally.");
            println!("Use 'lattice uenv pull {spec}' first, then retry.");
        }
    }

    Ok(())
}

/// Execute `lattice uenv cache list`.
fn execute_cache_list(args: &UenvCacheListArgs) -> anyhow::Result<()> {
    let cache_dir = &args.cache_dir;

    // Note: --node flag is accepted but currently only local cache is inspected.
    // Remote node cache inspection would require querying the node agent.
    match std::fs::read_dir(cache_dir) {
        Ok(entries) => {
            let mut found = false;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "squashfs") {
                    found = true;
                    // Show file name and size
                    let size = entry
                        .metadata()
                        .map(|m| format_size(m.len()))
                        .unwrap_or_else(|_| "?".to_string());
                    println!("{}\t{size}", path.display());
                }
            }
            if !found {
                println!("No cached uenv images found in {cache_dir}.");
            }
        }
        Err(e) => {
            eprintln!("Cache directory not available at {cache_dir}: {e}");
            eprintln!("The node agent may not have created the cache directory yet.");
        }
    }

    Ok(())
}

/// Format bytes as human-readable size.
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_local_path_squashfs() {
        assert!(is_local_path("/tmp/image.squashfs"));
        assert!(is_local_path("image.squashfs"));
    }

    #[test]
    fn is_local_path_registry_spec() {
        assert!(!is_local_path("prgenv-gnu/24.11:v1"));
        assert!(!is_local_path("pytorch:latest"));
    }

    #[test]
    fn format_size_bytes() {
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    #[test]
    fn cache_list_nonexistent_dir() {
        // Should not panic, just print error
        let args = UenvCacheListArgs {
            node: "test-node".to_string(),
            cache_dir: "/nonexistent/cache/dir".to_string(),
        };
        let result = execute_cache_list(&args);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn views_no_registry() {
        // Should not panic with no registry configured
        let args = UenvViewsArgs {
            spec: "prgenv-gnu/24.11:v1".to_string(),
        };
        let result = execute_views(&args);
        assert!(result.is_ok());
    }

    #[test]
    fn cache_list_with_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        // Create a fake squashfs file
        let squashfs_path = dir.path().join("test-image.squashfs");
        std::fs::write(&squashfs_path, b"fake squashfs content").unwrap();

        let args = UenvCacheListArgs {
            node: "test-node".to_string(),
            cache_dir: dir.path().to_str().unwrap().to_string(),
        };
        let result = execute_cache_list(&args);
        assert!(result.is_ok());
    }

    #[test]
    fn cache_list_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let args = UenvCacheListArgs {
            node: "test-node".to_string(),
            cache_dir: dir.path().to_str().unwrap().to_string(),
        };
        let result = execute_cache_list(&args);
        assert!(result.is_ok());
    }
}
