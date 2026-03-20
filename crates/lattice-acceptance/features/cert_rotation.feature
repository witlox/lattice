Feature: Certificate Rotation
  The LatticeRotator swaps the active workload identity when a certificate
  renewal occurs. The rotator holds the current identity behind an Arc<RwLock>.

  Failed rotation keeps the current identity unchanged (no disruption).
  Multiple rotations are supported as the identity evolves over time.

  # --- Basic Rotation ---

  Scenario: Rotation stores new identity
    Given a rotator with an initial bootstrap identity
    When a new self-signed identity is rotated in
    Then the current identity source should be "SelfSigned"

  Scenario: Current identity accessible after rotation
    Given a rotator with an initial bootstrap identity
    When a new self-signed identity is rotated in
    Then the current identity should have cert data
    And the current identity should have key data

  # --- First Identity ---

  Scenario: Rotation with no prior identity works
    Given a rotator with no initial identity
    When a new bootstrap identity is rotated in
    Then the current identity source should be "Bootstrap"

  # --- Multiple Rotations ---

  Scenario: Multiple rotations succeed
    Given a rotator with no initial identity
    When a new bootstrap identity is rotated in
    Then the current identity source should be "Bootstrap"
    When a new self-signed identity is rotated in
    Then the current identity source should be "SelfSigned"
    When a new SPIRE identity is rotated in
    Then the current identity source should be "Spire"

  # --- Failed Rotation ---

  Scenario: Failed rotation keeps current identity unchanged
    Given a rotator with an initial bootstrap identity
    When a rotation fails
    Then the current identity source should be "Bootstrap"
    And the current identity should still be valid

  # --- Renewal Timing ---

  Scenario: Rotation at 2/3 lifetime triggers correctly
    Given a rotator with a 3-day identity issued 2 days ago
    Then the current identity should need renewal

  # --- Provider Upgrade ---

  Scenario: SPIRE upgrade replaces bootstrap identity
    Given a rotator with an initial bootstrap identity
    When a new SPIRE identity is rotated in
    Then the current identity source should be "Spire"
    And the current identity should not be the bootstrap identity
