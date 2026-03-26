Feature: Secret Resolution
  Lattice components resolve operational secrets (API credentials, signing keys)
  at startup via a SecretResolver. When Vault is globally configured, all secrets
  come from Vault KV v2 with convention-based paths. When Vault is not configured,
  secrets fall back to environment variables, then config file literals.

  TLS certificates are NOT handled by the SecretResolver — they use the
  hpc-identity cascade (SPIRE, self-signed CA, bootstrap).

  Secret values must never appear in log output (INV-SEC2).

  # --- Vault Configured: Success ---

  Scenario: All secrets resolved from Vault when configured
    Given Vault is configured with address "https://vault.example.com"
    And Vault prefix is "secret/data/lattice"
    And Vault contains key "storage/vast_username" with value "lattice-svc"
    And Vault contains key "storage/vast_password" with value "s3cret"
    And Vault contains key "accounting/waldur_token" with value "tok-abc"
    And Vault contains key "quorum/audit_signing_key" with base64 value of 32 bytes
    And the config file has storage.vast_password set to "ignored-literal"
    When the SecretResolver resolves all secrets
    Then the resolved vast_password should be "s3cret"
    And the config literal "ignored-literal" should not be used

  # --- Vault Configured: Missing Key ---

  Scenario: Fatal error when Vault key is missing
    Given Vault is configured with address "https://vault.example.com"
    And Vault contains key "storage/vast_username" with value "lattice-svc"
    And Vault does not contain key "storage/vast_password"
    When the SecretResolver attempts to resolve all secrets
    Then resolution should fail with error containing "storage/vast_password"
    And the error should contain the Vault path "secret/data/lattice/storage"

  # --- Vault Configured: Vault Unreachable ---

  Scenario: Fatal error when Vault is unreachable
    Given Vault is configured with address "https://vault.unreachable.example.com"
    And the Vault server is not reachable
    When the SecretResolver attempts to resolve all secrets
    Then resolution should fail with a connection error
    And the error should contain "vault.unreachable.example.com"

  # --- Vault Configured: TLS Verification Failure ---

  Scenario: Fatal error when Vault TLS certificate is untrusted
    Given Vault is configured with address "https://vault.example.com"
    And Vault uses a certificate signed by an unknown CA
    And tls_ca_path is not set
    When the SecretResolver attempts to resolve all secrets
    Then resolution should fail with a TLS verification error

  # --- Vault Configured: Auth Failure ---

  Scenario: Fatal error when Vault AppRole auth fails
    Given Vault is configured with address "https://vault.example.com"
    And AppRole credentials are invalid
    When the SecretResolver attempts to resolve all secrets
    Then resolution should fail with an authentication error

  # --- No Vault: Env Var Fallback ---

  Scenario: Secret resolved from environment variable when Vault not configured
    Given Vault is not configured
    And environment variable "LATTICE_STORAGE_VAST_PASSWORD" is set to "env-secret"
    And the config file has storage.vast_password set to "config-literal"
    When the SecretResolver resolves all secrets
    Then the resolved vast_password should be "env-secret"

  # --- No Vault: Config Literal Fallback ---

  Scenario: Secret resolved from config literal when no env var set
    Given Vault is not configured
    And environment variable "LATTICE_STORAGE_VAST_PASSWORD" is not set
    And the config file has storage.vast_password set to "config-literal"
    When the SecretResolver resolves all secrets
    Then the resolved vast_password should be "config-literal"

  # --- No Vault: Missing Required Secret ---

  Scenario: Fatal error when required secret is empty and no fallback
    Given Vault is not configured
    And environment variable "LATTICE_STORAGE_VAST_PASSWORD" is not set
    And the config file has storage.vast_password set to ""
    When the SecretResolver attempts to resolve all secrets
    Then resolution should fail with error containing "storage.vast_password"
    And the error should indicate the field is required

  # --- Audit Signing Key: File Fallback ---

  Scenario: Audit signing key loaded from file when Vault not configured
    Given Vault is not configured
    And audit_signing_key_path points to a valid 32-byte key file
    When the SecretResolver resolves the audit signing key
    Then the signing key should be loaded from the file
    And the signing key should be 32 bytes

  # --- Audit Signing Key: Dev Mode ---

  Scenario: Random audit signing key generated in dev mode
    Given Vault is not configured
    And audit_signing_key_path is not set
    When the SecretResolver resolves the audit signing key
    Then a random signing key should be generated
    And a warning should be logged mentioning "dev mode"

  # --- Secret Logging Protection (INV-SEC2) ---

  Scenario: Resolved secret values never appear in logs
    Given Vault is configured with address "https://vault.example.com"
    And Vault contains key "storage/vast_password" with value "super-secret-pw"
    When the SecretResolver resolves all secrets
    Then the log output should not contain "super-secret-pw"
    And the log output may contain "secret/data/lattice/storage"
    And the log output may contain "vast_password"

  # --- Convention Path Mapping ---

  Scenario: Config field names map to Vault paths by convention
    Given Vault is configured with prefix "secret/data/lattice"
    Then field "storage.vast_password" should map to path "secret/data/lattice/storage" key "vast_password"
    And field "storage.vast_username" should map to path "secret/data/lattice/storage" key "vast_username"
    And field "accounting.waldur_token" should map to path "secret/data/lattice/accounting" key "waldur_token"
    And field "quorum.audit_signing_key" should map to path "secret/data/lattice/quorum" key "audit_signing_key"

  # --- Vault Override Semantics ---

  Scenario: Vault overrides all secret sources when configured
    Given Vault is configured with address "https://vault.example.com"
    And Vault contains key "storage/vast_password" with value "vault-value"
    And environment variable "LATTICE_STORAGE_VAST_PASSWORD" is set to "env-value"
    And the config file has storage.vast_password set to "config-value"
    When the SecretResolver resolves all secrets
    Then the resolved vast_password should be "vault-value"
