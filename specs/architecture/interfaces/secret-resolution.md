# Secret Resolution Interface

Structural skeleton for the SecretResolver subsystem. Lives in `lattice-common` (shared kernel). Consumed by `lattice-api` and `lattice-quorum` at startup.

**Spec sources:** INV-SEC1 through INV-SEC6, FM-25, FM-26, IP-15, A-V6 through A-V11

## Trait: SecretBackend

The pluggable backend for fetching secret values. Three implementations: Vault, Env, Config literal.

```
trait SecretBackend {
    /// Fetch a secret value by section and field name.
    /// Returns the raw string value (or base64 for binary secrets).
    /// Errors are fatal — propagated to caller as SecretResolutionError.
    fn fetch(&self, section: &str, field: &str) -> Result<SecretValue, SecretResolutionError>;
}
```

**Invariant enforcement:**
- INV-SEC2: `SecretValue` is an opaque wrapper. `Debug` and `Display` emit `[REDACTED]`. No `.to_string()` that reveals content.
- INV-SEC4: When Vault is configured, only `VaultBackend` is used. No fallback chain.
- INV-SEC6: When Vault is not configured, `EnvBackend` → `ConfigBackend` chain.

### VaultBackend

```
VaultBackend {
    client: VaultClient,       // Authenticated at construction time
    prefix: String,            // Default: "secret/data/lattice"
}
```

**Construction:** `VaultBackend::new(config: &VaultConfig) -> Result<Self, SecretResolutionError>`
- Authenticates via AppRole (role_id + secret_id)
- Authentication failure → `SecretResolutionError::AuthenticationFailed`
- Connection failure → `SecretResolutionError::ConnectionFailed`
- INV-SEC3: Both are fatal. No retry.

**Fetch:** `GET {prefix}/{section}` → parse KV v2 response → extract `data.data.{field}`
- Missing path → `SecretResolutionError::PathNotFound { path }`
- Missing key → `SecretResolutionError::KeyNotFound { path, key }`
- INV-SEC5: Path is `{prefix}/{section}`, key is `{field}`. Pure function of inputs.

### EnvBackend

```
EnvBackend {}
```

**Fetch:** `std::env::var("LATTICE_{SECTION}_{FIELD}")` (uppercase, underscores)
- `section = "storage"`, `field = "vast_password"` → `LATTICE_STORAGE_VAST_PASSWORD`
- Variable unset → `SecretResolutionError::NotFound` (triggers fallback to ConfigBackend)

### ConfigBackend

```
ConfigBackend {
    config: LatticeConfig,    // Parsed from YAML
}
```

**Fetch:** Reads field value from parsed config struct.
- Empty string → `SecretResolutionError::NotFound`
- Non-empty → `Ok(SecretValue::from(value))`

## Struct: SecretResolver

The composition root that wires backends and resolves all secrets in one call.

```
SecretResolver {
    backend: Box<dyn SecretBackend>,    // Vault or FallbackChain(Env, Config)
}
```

**Construction:**
```
SecretResolver::new(config: &LatticeConfig) -> Result<Self, SecretResolutionError>
```
- If `config.vault.address` is set → construct `VaultBackend` (INV-SEC4)
- Else → construct `FallbackChain { env: EnvBackend, config: ConfigBackend }`

**Resolution:**
```
SecretResolver::resolve_all(&self) -> Result<ResolvedSecrets, SecretResolutionError>
```
- Calls `self.backend.fetch(section, field)` for each registered secret field
- Any failure → return first error (fatal, INV-SEC1)
- All succeed → return `ResolvedSecrets`

**Secret field registry** (hardcoded, not dynamic):

| Section | Field | Type | Required |
|---|---|---|---|
| `storage` | `vast_username` | String | Yes (when storage configured) |
| `storage` | `vast_password` | String | Yes (when storage configured) |
| `accounting` | `waldur_token` | String | Yes (when accounting configured) |
| `quorum` | `audit_signing_key` | Binary (base64) | No (random if absent, dev mode) |
| `sovra` | `key_path` | String | Yes (when federation configured) |

"Required" is conditional on feature configuration. If `config.storage` is `None`, VAST secrets are not resolved.

**Binary secret handling:** `quorum.audit_signing_key` is base64-encoded in Vault/env/config. The resolver decodes it to `[u8; 32]` after fetch. Decode failure → `SecretResolutionError::InvalidFormat`.

## Struct: FallbackChain

Used when Vault is not configured. Tries env, then config.

```
FallbackChain {
    env: EnvBackend,
    config: ConfigBackend,
}
```

Implements `SecretBackend`:
```
fn fetch(&self, section, field) -> Result<SecretValue, SecretResolutionError> {
    match self.env.fetch(section, field) {
        Ok(v) => Ok(v),
        Err(SecretResolutionError::NotFound) => self.config.fetch(section, field),
        Err(e) => Err(e),
    }
}
```

## Struct: ResolvedSecrets

The output of resolution. Consumed by component initialization code.

```
ResolvedSecrets {
    vast_username: Option<SecretValue>,
    vast_password: Option<SecretValue>,
    waldur_token: Option<SecretValue>,
    audit_signing_key: Option<[u8; 32]>,    // Decoded from base64
    sovra_key: Option<SecretValue>,
}
```

All fields are `Option` — presence determined by which features are configured.

## Struct: SecretValue

Opaque wrapper for resolved secret values.

```
SecretValue {
    inner: String,      // Private — not accessible except via expose()
}

impl SecretValue {
    /// Reveal the secret value. Use sparingly — only when passing to
    /// client constructors that need the raw string.
    fn expose(&self) -> &str;
}

impl Debug for SecretValue {
    fn fmt(&self, f) -> "[REDACTED]"
}

impl Display for SecretValue {
    fn fmt(&self, f) -> "[REDACTED]"
}
```

**INV-SEC2 enforcement:** The only way to access the value is `expose()`. All logging, error messages, and debug output go through `Debug`/`Display` which redact.

## Usage Pattern (lattice-api main.rs)

```
fn main() {
    let config = load_config()?;

    // Step 1: Resolve secrets (INV-SEC1: before serving)
    let resolver = SecretResolver::new(&config)?;     // May fail (INV-SEC3)
    let secrets = resolver.resolve_all()?;             // May fail (FM-25, FM-26)

    // Step 2: Construct clients using resolved secrets
    let vast_client = VastClient::new(
        &config.storage,
        secrets.vast_username.as_ref().map(|s| s.expose()),
        secrets.vast_password.as_ref().map(|s| s.expose()),
    );

    let waldur_client = WaldurClient::new(
        &config.accounting,
        secrets.waldur_token.as_ref().map(|s| s.expose()),
    );

    // Step 3: Identity cascade (hpc-identity — separate from SecretResolver)
    let identity = IdentityCascade::new(...)?;

    // Step 4: Build server and start serving
    // ...
}
```

## Usage Pattern (lattice-quorum)

```
fn main() {
    let config = load_config()?;

    let resolver = SecretResolver::new(&config)?;
    let secrets = resolver.resolve_all()?;
    // When Vault is configured: audit_signing_key MUST be in Vault.
    // Missing key → fatal (INV-SEC4). No file fallback.
    //
    // When Vault is NOT configured: resolution order is:
    //   1. env var LATTICE_QUORUM_AUDIT_SIGNING_KEY (base64)
    //   2. config literal (base64)
    //   3. file path (config.quorum.audit_signing_key_path)
    //   4. random key (dev mode, with warning)
    //
    // File path is a non-Vault-only fallback for the signing key because
    // it predates the SecretResolver. It is NOT available when Vault is active.

    if let Some(key_bytes) = secrets.audit_signing_key {
        quorum.state().load_signing_key_from_bytes(&key_bytes)?;
    } else if config.vault.is_none() {
        // Vault not configured — try file path fallback
        if let Some(ref path) = config.quorum.audit_signing_key_path {
            quorum.state().load_signing_key_from_file(path)?;
        } else {
            warn!("No audit_signing_key configured — using random key (dev mode only)");
        }
    }
    // When Vault IS configured and key is missing, resolve_all() already
    // returned a fatal error above — we never reach this point.
}
```

## Relationship to Existing Config

The `SecretResolver` does NOT replace `LatticeConfig`. Config parsing happens first (YAML → struct). Secret resolution happens second (replaces secret field values with Vault/env values). Non-secret config fields (ports, timeouts, feature flags) are never touched by the resolver.

New config section added:
```
VaultConfig {
    address: String,              // Vault server URL
    prefix: String,               // Default: "secret/data/lattice"
    role_id: String,              // AppRole role ID
    secret_id_env: String,        // Env var name containing AppRole secret ID
                                  // (secret_id itself is never in config file)
    tls_ca_path: Option<PathBuf>, // CA bundle for Vault TLS. None = system trust store.
    timeout_secs: u64,            // Default: 10
}
```

Note: `secret_id` is loaded from an env var (named by `secret_id_env`), NOT from config. This avoids putting a Vault credential in a config file.

## Implementation Notes (from adversary review)

**SEC-F4 — Zeroization:** `SecretValue` should implement `Drop` with zeroization (use `zeroize` crate). The `[u8; 32]` audit signing key should be wrapped in `Zeroizing<[u8; 32]>`. Low-cost hardening for management-plane services where core dumps may be enabled.

**SEC-F7 — VaultError body sanitization:** The `VaultError { body }` field should be truncated to 256 bytes to prevent accidental secret leakage from unexpected Vault responses.

**SEC-F8 — Collect all errors:** `resolve_all()` should attempt ALL secret resolutions and collect errors, returning them as a combined error rather than stopping at the first failure. This gives operators a complete picture during initial Vault setup. Still fatal (INV-SEC1).

**SEC-F3 — ConfigBackend exhaustiveness:** The ConfigBackend match on `(section, field)` should be tested at compile time or via a unit test that iterates the secret field registry and asserts each entry has a config mapping.
