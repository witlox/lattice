//! `lattice login` / `lattice logout` — authentication commands.

use std::time::Duration;

use clap::Args;
use hpc_auth::types::{AuthClientConfig, OAuthFlow, PermissionMode};
use hpc_auth::AuthClient;

/// Arguments for the login command.
#[derive(Args, Debug)]
pub struct LoginArgs {
    /// OAuth2 flow override: pkce, device, manual, client-credentials
    #[arg(long)]
    pub flow: Option<String>,

    /// Override server URL for authentication
    #[arg(long)]
    pub server: Option<String>,
}

/// Arguments for the logout command.
#[derive(Args, Debug)]
pub struct LogoutArgs {}

/// Parse a flow name string into an `OAuthFlow` enum.
fn parse_flow(flow: &str) -> Result<OAuthFlow, String> {
    match flow {
        "pkce" => Ok(OAuthFlow::AuthCodePkce),
        "device" => Ok(OAuthFlow::DeviceCode),
        "manual" => Ok(OAuthFlow::ManualPaste),
        "client-credentials" => Err(
            "client-credentials flow requires --client-id and --client-secret (not yet supported via CLI)".to_string(),
        ),
        other => Err(format!(
            "unknown flow '{other}': expected pkce, device, manual, or client-credentials"
        )),
    }
}

/// Build an `AuthClientConfig` for the lattice CLI.
pub fn build_auth_config(
    server_url: &str,
    flow_override: Option<&str>,
) -> Result<AuthClientConfig, String> {
    let flow = flow_override.map(parse_flow).transpose()?;

    Ok(AuthClientConfig {
        server_url: server_url.to_string(),
        app_name: "lattice".to_string(),
        permission_mode: PermissionMode::Lenient, // INV-A2
        idp_override: None,
        flow_override: flow,
        timeout: Duration::from_secs(30),
    })
}

/// Execute the login command.
pub async fn execute_login(args: &LoginArgs, server_url: &str, quiet: bool) -> anyhow::Result<()> {
    let effective_server = args.server.as_deref().unwrap_or(server_url);
    let auth_config = build_auth_config(effective_server, args.flow.as_deref())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let auth_client = AuthClient::new(auth_config);

    match auth_client.login().await {
        Ok(_tokens) => {
            if !quiet {
                println!("Logged in to {effective_server}");
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("Login failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Execute the logout command.
pub async fn execute_logout(server_url: &str, quiet: bool) -> anyhow::Result<()> {
    let auth_config = build_auth_config(server_url, None).map_err(|e| anyhow::anyhow!("{e}"))?;
    let auth_client = AuthClient::new(auth_config);

    match auth_client.logout().await {
        Ok(()) => {
            if !quiet {
                println!("Logged out from {server_url}");
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("Logout failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Try to get a valid token for the given server URL.
/// Returns `Ok(token_string)` if successful, or an error if not logged in.
pub async fn get_token_for_server(server_url: &str) -> Result<String, hpc_auth::AuthError> {
    let auth_config = build_auth_config(server_url, None).map_err(hpc_auth::AuthError::Internal)?;
    let auth_client = AuthClient::new(auth_config);
    auth_client.get_token().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_flow_pkce() {
        let flow = parse_flow("pkce").unwrap();
        assert!(matches!(flow, OAuthFlow::AuthCodePkce));
    }

    #[test]
    fn parse_flow_device() {
        let flow = parse_flow("device").unwrap();
        assert!(matches!(flow, OAuthFlow::DeviceCode));
    }

    #[test]
    fn parse_flow_manual() {
        let flow = parse_flow("manual").unwrap();
        assert!(matches!(flow, OAuthFlow::ManualPaste));
    }

    #[test]
    fn parse_flow_client_credentials_rejected() {
        let result = parse_flow("client-credentials");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("client-credentials"));
    }

    #[test]
    fn parse_flow_unknown_rejected() {
        let result = parse_flow("foobar");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown flow"));
    }

    #[test]
    fn build_auth_config_defaults() {
        let config = build_auth_config("https://lattice.example.com", None).unwrap();
        assert_eq!(config.server_url, "https://lattice.example.com");
        assert_eq!(config.app_name, "lattice");
        assert_eq!(config.permission_mode, PermissionMode::Lenient);
        assert!(config.flow_override.is_none());
        assert!(config.idp_override.is_none());
    }

    #[test]
    fn build_auth_config_with_flow_override() {
        let config = build_auth_config("https://lattice.example.com", Some("device")).unwrap();
        assert!(matches!(config.flow_override, Some(OAuthFlow::DeviceCode)));
    }

    #[test]
    fn build_auth_config_with_invalid_flow() {
        let result = build_auth_config("https://lattice.example.com", Some("bogus"));
        assert!(result.is_err());
    }

    #[test]
    fn login_args_parse() {
        use clap::Parser;

        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            login: LoginArgs,
        }

        let cli = TestCli::parse_from(["test"]);
        assert!(cli.login.flow.is_none());
        assert!(cli.login.server.is_none());

        let cli = TestCli::parse_from(["test", "--flow", "pkce", "--server", "https://x.com"]);
        assert_eq!(cli.login.flow.as_deref(), Some("pkce"));
        assert_eq!(cli.login.server.as_deref(), Some("https://x.com"));
    }

    #[test]
    fn logout_args_parse() {
        use clap::Parser;

        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            logout: LogoutArgs,
        }

        let _cli = TestCli::parse_from(["test"]);
    }

    #[tokio::test]
    async fn get_token_returns_expired_when_not_logged_in() {
        // With a fresh temp cache, no token should exist.
        let result = get_token_for_server("https://nonexistent.example.com").await;
        assert!(result.is_err());
    }
}
