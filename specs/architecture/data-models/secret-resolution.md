# Secret Resolution Data Model

Types for the SecretResolver subsystem in `lattice-common`.

**Spec sources:** INV-SEC1 through INV-SEC6, ubiquitous language (Secret Field, Convention-Based Vault Path, Vault Global Override)

## Configuration Types

### VaultConfig

Added to `LatticeConfig` as `vault: Option<VaultConfig>`. Presence of this section activates Vault mode (INV-SEC4).

```
VaultConfig {
    address: String,
    // Vault server URL. Required when vault section present.
    // Example: "https://vault.example.com:8200"

    prefix: String,
    // KV v2 mount + path prefix. Default: "secret/data/lattice"
    // Maps to: GET {address}/v1/{prefix}/{section}

    role_id: String,
    // AppRole role ID. Can be in config (it's not secret).

    secret_id_env: String,
    // Name of env var containing the AppRole secret ID.
    // Default: "VAULT_SECRET_ID"
    // The secret_id value is NEVER in the config file.
    // Trust boundary: this field determines which env var supplies the
    // Vault credential. Config file integrity is assumed — if the config
    // file is writable by an untrusted actor, they can redirect auth.

    tls_ca_path: Option<PathBuf>,
    // CA certificate bundle for verifying Vault's TLS certificate.
    // When None: uses system trust store (may include SPIRE-provisioned CAs).
    // When Some: uses this file as the sole trust root for Vault connections.
    // Required when Vault uses a private CA not in the system trust store.
    // TLS verification is NEVER disabled.

    timeout_secs: u64,
    // HTTP client timeout for Vault requests. Default: 10
}
```

**Presence semantics:**
- `vault: None` (section absent) → no Vault, use env/config fallback (INV-SEC6)
- `vault: Some(config)` → all secrets from Vault, no fallback (INV-SEC4)

### Modified Existing Config Sections

No structural changes to existing config types. Secret fields remain as `String` / `Option<String>` in the config struct. The `SecretResolver` overrides their values after config parsing.

| Existing field | Config type | Used by |
|---|---|---|
| `StorageConfig.vast_username` | `Option<String>` | lattice-api |
| `StorageConfig.vast_password` | `Option<String>` | lattice-api |
| `AccountingConfig.waldur_token_secret_ref` | `String` | lattice-api |
| `QuorumConfig.audit_signing_key_path` | `Option<PathBuf>` | lattice-quorum |
| `SovraConfig.key_path` | `String` | lattice-api |

**Rename:** `waldur_token_secret_ref` → `waldur_token`. The `_secret_ref` suffix was misleading (the value was never a reference). This is a breaking config change — documented in migration notes.

## Core Types

### SecretValue

```
SecretValue {
    inner: String,
}
```

- Private field. Constructed via `SecretValue::from(s: String)` or `SecretValue::from(s: &str)`.
- `expose() -> &str` — the only way to read the value.
- `Debug` → `"SecretValue([REDACTED])"`
- `Display` → `"[REDACTED]"`
- `Clone` — necessary for passing to client constructors.
- No `Serialize` — secrets must never be serialized.
- No `PartialEq` — comparing secrets is a code smell (testing uses `expose()`).

### ResolvedSecrets

```
ResolvedSecrets {
    vast_username: Option<SecretValue>,
    vast_password: Option<SecretValue>,
    waldur_token: Option<SecretValue>,
    audit_signing_key: Option<[u8; 32]>,
    sovra_key: Option<SecretValue>,
}
```

All fields `Option` — absence means the feature is not configured (not that resolution failed — failure is always fatal per INV-SEC1).

`audit_signing_key` is `[u8; 32]` (decoded), not `SecretValue`. Binary secrets are decoded from base64 during resolution.

### SecretResolutionError

```
SecretResolutionError
  ├── ConnectionFailed { address: String, source: String }
  │     Vault TCP/TLS connection or certificate verification failed (FM-25)
  │
  ├── AuthenticationFailed { address: String, detail: String }
  │     Vault AppRole auth rejected (FM-25)
  │
  ├── PathNotFound { path: String }
  │     Vault KV v2 path does not exist (FM-26)
  │
  ├── KeyNotFound { path: String, key: String }
  │     Path exists but field missing in KV v2 response (FM-26)
  │
  ├── NotFound { section: String, field: String }
  │     Non-Vault: env var unset AND config empty
  │
  ├── InvalidFormat { section: String, field: String, reason: String }
  │     Base64 decode failed for binary secret
  │
  └── VaultError { path: String, status: u16, body: String }
        Unexpected Vault response (permission denied, server error, etc.)
```

**INV-SEC2 enforcement:** No variant contains a resolved secret value. Error messages reference paths and field names only.

**Not added to `LatticeError`:** `SecretResolutionError` is a startup error. It never reaches the API layer. It is used only in `main()` to produce a descriptive error message and exit.

## Secret Field Registry

Hardcoded mapping from (section, field) to config source and conditionality:

```
SecretField {
    section: &'static str,
    field: &'static str,
    required_when: FeatureCondition,
}

enum FeatureCondition {
    StorageConfigured,      // config.storage is Some
    AccountingConfigured,   // config.accounting.enabled
    FederationConfigured,   // config.federation is Some
    Always,                 // Always resolved (but may be None for dev mode)
}
```

Registry (compile-time):
```
SECRETS: &[SecretField] = &[
    SecretField { section: "storage",    field: "vast_username",      required_when: StorageConfigured },
    SecretField { section: "storage",    field: "vast_password",      required_when: StorageConfigured },
    SecretField { section: "accounting", field: "waldur_token",       required_when: AccountingConfigured },
    SecretField { section: "quorum",     field: "audit_signing_key",  required_when: Always },
    SecretField { section: "sovra",      field: "key_path",           required_when: FederationConfigured },
];
```

## Vault KV v2 Response Model

For mapping Vault HTTP responses to secret values.

```
VaultKvResponse {
    data: VaultKvData {
        data: HashMap<String, String>,    // The actual key-value pairs
        metadata: VaultKvMetadata {
            version: u64,
            created_time: String,
            // Other metadata fields ignored
        },
    },
}
```

**Convention path mapping (INV-SEC5):**
- Input: `section = "storage"`, `field = "vast_password"`, `prefix = "secret/data/lattice"`
- HTTP request: `GET {vault_address}/v1/secret/data/lattice/storage`
- Response extraction: `response.data.data["vast_password"]`

## Logging Conventions

| Event | Log level | Content |
|---|---|---|
| Vault mode activated | INFO | `"Secret resolution via Vault at {address}"` |
| Env/config fallback mode | INFO | `"Secret resolution via environment/config (no Vault configured)"` |
| Secret resolved from Vault | DEBUG | `"Resolved {section}.{field} from Vault path {path}"` |
| Secret resolved from env | DEBUG | `"Resolved {section}.{field} from env var LATTICE_{SECTION}_{FIELD}"` |
| Secret resolved from config | DEBUG | `"Resolved {section}.{field} from config file"` |
| Dev mode random key | WARN | `"No audit_signing_key configured — using random key (dev mode only)"` |
| Resolution failed | ERROR | `"Failed to resolve {section}.{field}: {error}"` |

**INV-SEC2:** No log message ever contains a resolved value. Only paths, field names, and source indicators.
