Feature: CLI Authentication (Lattice-specific)

  Lattice CLI wraps the shared hpc-auth crate for token acquisition.
  These scenarios cover Lattice-specific behavior on top of the crate.

  # --- CLI Commands ---

  Scenario: lattice login initiates auth flow
    Given the lattice-api server is configured
    When the user runs lattice login
    Then the system discovers the IdP from the lattice-api auth discovery endpoint
    And delegates to the hpc-auth crate for token acquisition
    And uses lenient permission mode for the token cache

  Scenario: lattice login with explicit server
    Given no default server is configured
    When the user runs lattice login --server cluster.example.com
    Then the system contacts cluster.example.com for IdP discovery
    And sets cluster.example.com as the default server

  Scenario: lattice logout clears token
    Given the user is logged in
    When the user runs lattice logout
    Then the system delegates to the hpc-auth crate for logout
    And the user is informed the session has ended

  Scenario: lattice login with --device-code
    Given the lattice-api server is configured
    When the user runs lattice login --device-code
    Then the system forces the device code flow via the hpc-auth crate

  Scenario: lattice login with --service-account
    Given client_id and client_secret are available via config or environment
    When the user runs lattice login --service-account
    Then the system delegates to the hpc-auth crate client credentials flow

  # --- Unauthenticated Commands ---

  Scenario: lattice version does not require auth
    Given no token exists in the cache
    When the user runs lattice version
    Then the command succeeds and displays the version

  Scenario: lattice --help does not require auth
    Given no token exists in the cache
    When the user runs lattice --help
    Then the command succeeds and displays help text

  # --- Authenticated Commands ---

  Scenario: lattice status requires auth
    Given no token exists in the cache
    When the user runs lattice status
    Then the command fails with an authentication error
    And the user is prompted to run lattice login

  Scenario: lattice submit requires auth
    Given no token exists in the cache
    When the user runs lattice submit job.yaml
    Then the command fails with an authentication error
    And the user is prompted to run lattice login

  Scenario: Authenticated command with valid token
    Given a valid token exists in the cache for the configured server
    When the user runs lattice status
    Then the token is included in the request to lattice-api
    And the command proceeds

  Scenario: Authenticated command triggers silent refresh
    Given an expired access token in the cache
    And a valid refresh token in the cache
    When the user runs lattice status
    Then the hpc-auth crate silently refreshes the access token
    And the command proceeds with the new token

  # --- Waldur Authorization ---

  Scenario: Valid token but no Waldur allocation for vCluster
    Given the user is authenticated
    And the user has no Waldur allocation for vcluster-3
    When the user runs lattice submit --vcluster vcluster-3 job.yaml
    Then the command fails with an authorization error
    And the error explains the user has no allocation on vcluster-3

  Scenario: Valid token with Waldur allocation
    Given the user is authenticated
    And the user has a Waldur allocation for vcluster-1
    When the user runs lattice submit --vcluster vcluster-1 job.yaml
    Then the command is accepted for scheduling

  # --- Server Discovery Endpoint ---

  Scenario: lattice-api exposes auth discovery
    Given the lattice-api server is running
    When a client requests the auth discovery endpoint
    Then the server returns the IdP URL and public client ID
    And the endpoint does not require authentication

  Scenario: lattice-api auth discovery with optional FirecREST gateway
    Given the lattice-api server is running
    And FirecREST is deployed as an optional compatibility gateway
    When a client requests the auth discovery endpoint
    Then the server returns the same IdP configuration
    And FirecREST presence does not change the auth model
    And lattice authenticates directly against the IdP via hpc-auth

  # --- Multi-Server ---

  Scenario: Switching between lattice deployments
    Given the user is logged in to cluster-a as default
    And the user is logged in to cluster-b
    When the user runs lattice status --server cluster-b
    Then the command uses the token cached for cluster-b
    And the result reflects cluster-b state

  Scenario: Default server used when --server omitted
    Given the user is logged in to cluster-a as default
    And the user is logged in to cluster-b
    When the user runs lattice status
    Then the command uses the token cached for cluster-a
