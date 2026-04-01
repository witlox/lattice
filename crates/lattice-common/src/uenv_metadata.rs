//! Parser for uenv `env.json` metadata files.
//!
//! uenv SquashFS images contain an `env.json` at the root describing
//! available views and their environment variable patches. This module
//! converts that JSON into [`ImageMetadata`].

use serde::Deserialize;
use std::collections::HashMap;

use crate::types::{EnvOp, EnvPatch, ImageMetadata, ViewDef};

// ─── Serde models matching the env.json schema ─────────────

#[derive(Debug, Deserialize)]
struct EnvJson {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    mount: String,
    #[serde(default)]
    views: HashMap<String, ViewJson>,
    #[serde(default)]
    default_view: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ViewJson {
    #[serde(default)]
    description: String,
    #[serde(default)]
    env: Option<EnvBlock>,
}

#[derive(Debug, Deserialize)]
struct EnvBlock {
    #[serde(default)]
    values: Option<EnvValues>,
}

#[derive(Debug, Deserialize)]
struct EnvValues {
    #[serde(default)]
    set: HashMap<String, String>,
    #[serde(default)]
    prepend: HashMap<String, String>,
    #[serde(default)]
    append: HashMap<String, String>,
    #[serde(default)]
    unset: Vec<String>,
}

// ─── Public API ────────────────────────────────────────────

/// Parse a uenv `env.json` string into [`ImageMetadata`].
///
/// Returns an error description if the JSON is malformed or cannot be
/// deserialized into the expected schema.
pub fn parse_uenv_metadata(json: &str) -> Result<ImageMetadata, String> {
    let env: EnvJson = serde_json::from_str(json).map_err(|e| format!("invalid env.json: {e}"))?;

    let mut views: Vec<ViewDef> = env
        .views
        .into_iter()
        .map(|(name, v)| {
            let patches = build_patches(v.env);
            ViewDef {
                name,
                description: v.description,
                patches,
            }
        })
        .collect();

    // Sort views by name for deterministic output.
    views.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(ImageMetadata {
        name: env.name,
        description: env.description,
        mount_point: env.mount,
        views,
        default_view: env.default_view,
    })
}

/// Convert the optional env block into a flat list of [`EnvPatch`] entries.
fn build_patches(env: Option<EnvBlock>) -> Vec<EnvPatch> {
    let values = match env.and_then(|e| e.values) {
        Some(v) => v,
        None => return Vec::new(),
    };

    let mut patches = Vec::new();

    // Deterministic ordering: set, prepend, append, unset — each sorted by key.
    let mut set_keys: Vec<_> = values.set.into_iter().collect();
    set_keys.sort_by(|a, b| a.0.cmp(&b.0));
    for (var, val) in set_keys {
        patches.push(EnvPatch {
            variable: var,
            op: EnvOp::Set,
            value: val,
            separator: ":".to_string(),
        });
    }

    let mut prepend_keys: Vec<_> = values.prepend.into_iter().collect();
    prepend_keys.sort_by(|a, b| a.0.cmp(&b.0));
    for (var, val) in prepend_keys {
        patches.push(EnvPatch {
            variable: var,
            op: EnvOp::Prepend,
            value: val,
            separator: ":".to_string(),
        });
    }

    let mut append_keys: Vec<_> = values.append.into_iter().collect();
    append_keys.sort_by(|a, b| a.0.cmp(&b.0));
    for (var, val) in append_keys {
        patches.push(EnvPatch {
            variable: var,
            op: EnvOp::Append,
            value: val,
            separator: ":".to_string(),
        });
    }

    for var in values.unset {
        patches.push(EnvPatch {
            variable: var,
            op: EnvOp::Unset,
            value: String::new(),
            separator: ":".to_string(),
        });
    }

    patches
}

// ─── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_ENV_JSON: &str = r#"{
        "name": "prgenv-gnu",
        "description": "GNU programming environment",
        "mount": "/user-environment",
        "views": {
            "default": {
                "description": "Default view",
                "env": {
                    "values": {
                        "set": {
                            "CUDA_HOME": "/user-environment/cuda"
                        },
                        "prepend": {
                            "PATH": "/user-environment/bin",
                            "LD_LIBRARY_PATH": "/user-environment/lib"
                        },
                        "append": {},
                        "unset": ["OLD_VAR"]
                    }
                }
            },
            "spack": {
                "description": "Spack packages",
                "env": {
                    "values": {
                        "set": {},
                        "prepend": {
                            "PATH": "/user-environment/spack/bin"
                        },
                        "append": {},
                        "unset": []
                    }
                }
            }
        },
        "default_view": "default"
    }"#;

    #[test]
    fn parse_valid_env_json() {
        let meta = parse_uenv_metadata(FULL_ENV_JSON).unwrap();

        assert_eq!(meta.name, "prgenv-gnu");
        assert_eq!(meta.description, "GNU programming environment");
        assert_eq!(meta.mount_point, "/user-environment");
        assert_eq!(meta.default_view, Some("default".to_string()));
        assert_eq!(meta.views.len(), 2);

        // Views are sorted: "default" < "spack"
        let default_view = &meta.views[0];
        assert_eq!(default_view.name, "default");
        assert_eq!(default_view.description, "Default view");

        // Check patches: 1 Set + 2 Prepend + 1 Unset = 4
        assert_eq!(default_view.patches.len(), 4);

        // Set: CUDA_HOME
        let set_patch = &default_view.patches[0];
        assert_eq!(set_patch.variable, "CUDA_HOME");
        assert!(matches!(set_patch.op, EnvOp::Set));
        assert_eq!(set_patch.value, "/user-environment/cuda");

        // Prepend: LD_LIBRARY_PATH, PATH (sorted)
        let prepend1 = &default_view.patches[1];
        assert_eq!(prepend1.variable, "LD_LIBRARY_PATH");
        assert!(matches!(prepend1.op, EnvOp::Prepend));

        let prepend2 = &default_view.patches[2];
        assert_eq!(prepend2.variable, "PATH");
        assert!(matches!(prepend2.op, EnvOp::Prepend));

        // Unset: OLD_VAR
        let unset_patch = &default_view.patches[3];
        assert_eq!(unset_patch.variable, "OLD_VAR");
        assert!(matches!(unset_patch.op, EnvOp::Unset));
        assert!(unset_patch.value.is_empty());
    }

    #[test]
    fn parse_multiple_views_all_present() {
        let meta = parse_uenv_metadata(FULL_ENV_JSON).unwrap();
        let view_names: Vec<&str> = meta.views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(view_names, vec!["default", "spack"]);

        let spack = &meta.views[1];
        assert_eq!(spack.description, "Spack packages");
        assert_eq!(spack.patches.len(), 1);
        assert_eq!(spack.patches[0].variable, "PATH");
        assert!(matches!(spack.patches[0].op, EnvOp::Prepend));
    }

    #[test]
    fn parse_unset_list_creates_unset_patches() {
        let json = r#"{
            "name": "test",
            "mount": "/env",
            "views": {
                "cleanup": {
                    "description": "Cleanup view",
                    "env": {
                        "values": {
                            "set": {},
                            "prepend": {},
                            "append": {},
                            "unset": ["FOO", "BAR", "BAZ"]
                        }
                    }
                }
            }
        }"#;

        let meta = parse_uenv_metadata(json).unwrap();
        let view = &meta.views[0];
        assert_eq!(view.patches.len(), 3);
        for patch in &view.patches {
            assert!(matches!(patch.op, EnvOp::Unset));
            assert!(patch.value.is_empty());
        }
        let vars: Vec<&str> = view.patches.iter().map(|p| p.variable.as_str()).collect();
        assert_eq!(vars, vec!["FOO", "BAR", "BAZ"]);
    }

    #[test]
    fn parse_empty_views() {
        let json = r#"{
            "name": "empty",
            "mount": "/env",
            "views": {}
        }"#;

        let meta = parse_uenv_metadata(json).unwrap();
        assert!(meta.views.is_empty());
    }

    #[test]
    fn parse_malformed_json_returns_error() {
        let result = parse_uenv_metadata("{ not valid json }");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("invalid env.json"), "error was: {err}");
    }

    #[test]
    fn parse_missing_optional_fields() {
        // Minimal valid JSON — only required structure
        let json = r#"{"name": "minimal"}"#;
        let meta = parse_uenv_metadata(json).unwrap();
        assert_eq!(meta.name, "minimal");
        assert!(meta.views.is_empty());
        assert!(meta.mount_point.is_empty());
        assert!(meta.default_view.is_none());
    }

    #[test]
    fn parse_view_without_env_block() {
        let json = r#"{
            "name": "bare",
            "mount": "/env",
            "views": {
                "noenv": {
                    "description": "View without env"
                }
            }
        }"#;

        let meta = parse_uenv_metadata(json).unwrap();
        assert_eq!(meta.views.len(), 1);
        assert!(meta.views[0].patches.is_empty());
    }
}
