//! Shell completion generation.
//!
//! Generates bash/zsh/fish completion scripts using clap_complete.

use clap::CommandFactory;
use clap_complete::{generate, Shell};

use crate::commands::Cli;

/// Generate shell completions to stdout.
pub fn generate_completions(shell: Shell) {
    let mut cmd = Cli::command();
    generate(shell, &mut cmd, "lattice", &mut std::io::stdout());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_bash_completions() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        generate(Shell::Bash, &mut cmd, "lattice", &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("lattice"));
    }

    #[test]
    fn generate_zsh_completions() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        generate(Shell::Zsh, &mut cmd, "lattice", &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("lattice"));
    }

    #[test]
    fn generate_fish_completions() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        generate(Shell::Fish, &mut cmd, "lattice", &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("lattice"));
    }
}
