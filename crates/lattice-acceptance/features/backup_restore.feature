Feature: Backup and Restore
  As a system administrator, I can export, verify, and restore Raft state backups.

  Scenario: Export backup captures current state
    Given a quorum with 3 nodes and 2 allocations
    When I export a backup
    Then the backup metadata should show 3 nodes and 2 allocations
    And the backup file should exist on disk

  Scenario: Verify backup validates integrity
    Given a quorum with 3 nodes and 2 allocations
    And I export a backup
    When I verify the backup
    Then the verification should succeed
    And the verified metadata should match the export metadata

  Scenario: Verify detects corrupt backup
    Given a corrupt backup file
    When I verify the backup
    Then the verification should fail with "invalid"

  Scenario: Restore backup writes snapshot files
    Given a quorum with 3 nodes and 2 allocations
    And I export a backup
    When I restore the backup to a new data directory
    Then the snapshot directory should contain a current pointer
    And the restored snapshot should be valid JSON

  Scenario: Restore cleans up stale WAL
    Given a quorum with 3 nodes and 2 allocations
    And I export a backup
    And a data directory with existing WAL files
    When I restore the backup to that data directory
    Then the WAL directory should be empty
    And the snapshot directory should contain a current pointer
