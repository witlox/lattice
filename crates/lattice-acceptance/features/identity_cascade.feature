Feature: Identity Cascade
  Lattice node agent obtains mTLS workload identity via a cascading
  provider chain: SPIRE (primary on HPE Cray), self-signed quorum CA,
  then bootstrap cert. The cascade tries each in order and returns the
  first successful identity.

  Private keys never leave the agent. Identity source is logged for audit.
  Bootstrap identity is temporary (1-hour expiry).

  Background:
    Given an identity cascade with all three providers

  # --- Provider Priority ---

  Scenario: Cascade tries SPIRE first
    Given SPIRE is available
    And self-signed provider has a cached identity
    And bootstrap files exist
    When the cascade acquires identity
    Then the identity source should be "Spire"

  Scenario: Cascade falls back to self-signed when SPIRE unavailable
    Given SPIRE is unavailable
    And self-signed provider has a cached identity
    And bootstrap files exist
    When the cascade acquires identity
    Then the identity source should be "SelfSigned"

  Scenario: Cascade falls back to bootstrap when SPIRE and self-signed unavailable
    Given SPIRE is unavailable
    And self-signed provider has no cached identity
    And bootstrap files exist
    When the cascade acquires identity
    Then the identity source should be "Bootstrap"

  # --- Bootstrap Behavior ---

  Scenario: Bootstrap identity is temporary with 1-hour expiry
    Given SPIRE is unavailable
    And self-signed provider has no cached identity
    And bootstrap files exist
    When the cascade acquires identity
    Then the identity source should be "Bootstrap"
    And the identity should expire within 1 hour

  # --- Audit ---

  Scenario: Identity source is logged for audit
    Given SPIRE is unavailable
    And self-signed provider has a cached identity
    When the cascade acquires identity
    Then the identity source should be "SelfSigned"
    And the identity source should be recorded

  # --- Security ---

  Scenario: Private keys never leave the agent
    Given bootstrap files exist
    When the bootstrap provider acquires identity
    Then the identity should contain a private key
    And the private key should only exist in the identity struct
    And the debug output should redact the private key

  # --- TLS Config ---

  Scenario: Tonic TLS config built from WorkloadIdentity
    Given a valid workload identity
    When tonic TLS config is built from the identity
    Then the TLS config should be valid

  # --- Error Handling ---

  Scenario: Cascade with no providers fails with NoProviderAvailable
    Given an empty identity cascade
    When the cascade attempts to acquire identity
    Then the cascade should fail with "NoProviderAvailable"

  # --- Expiry Detection ---

  Scenario: Identity expiry detected correctly
    Given an identity that expired 1 hour ago
    Then the identity should not be valid

  Scenario: Fresh identity is valid
    Given an identity that expires in 1 hour
    Then the identity should be valid

  # --- Renewal Trigger ---

  Scenario: should_renew triggers at 2/3 lifetime
    Given an identity with 3-day lifetime issued 2 days ago
    Then the identity should need renewal

  Scenario: should_renew does not trigger for fresh identity
    Given an identity with 3-day lifetime issued just now
    Then the identity should not need renewal
