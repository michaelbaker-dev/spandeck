//! The catalogue of agent programs: what each is called, how to read its
//! version, the vendor's install command and how it updates itself.
//!
//! The install commands are fixed here and are HTTPS only. They are never
//! taken from a caller or from another station; a station's own config file
//! (`[agents.<name>]`) may override them for tests and unusual setups.
//!
//! Gemini CLI and Grok are not listed: their install and update commands
//! were not checked against the vendors' documentation.

use crate::AgentKind;

pub struct Program {
    pub kind: AgentKind,
    /// The command name, looked for on PATH and in `WELL_KNOWN_DIRS`.
    pub program: &'static str,
    /// Arguments that print the version.
    pub version_args: &'static [&'static str],
    /// The vendor's install command: one shell line, shown to the user
    /// exactly as it is run.
    pub install: &'static str,
    /// Arguments that make the program update itself.
    pub update_args: &'static [&'static str],
    /// Where the commands were checked.
    pub source: &'static str,
}

pub const CATALOGUE: &[Program] = &[
    Program {
        kind: AgentKind::Claude,
        program: "claude",
        version_args: &["--version"],
        install: "curl -fsSL https://claude.ai/install.sh | bash",
        update_args: &["update"],
        source: "code.claude.com/docs/en/setup (install, `claude update`)",
    },
    Program {
        kind: AgentKind::Codex,
        program: "codex",
        version_args: &["--version"],
        install: "curl -fsSL https://chatgpt.com/codex/install.sh | sh",
        update_args: &["update"],
        source: "learn.chatgpt.com/docs/codex/cli (install); `codex --help` 0.169.0 (update)",
    },
    Program {
        kind: AgentKind::OpenCode,
        program: "opencode",
        version_args: &["--version"],
        install: "curl -fsSL https://opencode.ai/v2/install | bash",
        update_args: &["upgrade"],
        source: "opencode.ai/docs (install); `opencode upgrade --help` 2.3.0 (update)",
    },
    Program {
        kind: AgentKind::Antigravity,
        program: "agy",
        version_args: &["--version"],
        install: "curl -fsSL https://antigravity.google/cli/install.sh | bash",
        update_args: &["update"],
        source:
            "antigravity.google/docs/getting-started?tab=cli (install); `agy --help` 1.5.9 (update)",
    },
];

/// Where agent programs are installed, besides PATH. `~` is the home folder.
/// A daemon started by the login service often has a shorter PATH than the
/// user's shell, and the vendors' installers use these.
pub const WELL_KNOWN_DIRS: &[&str] = &[
    "~/.local/bin",
    "~/.opencode/bin",
    "~/.cargo/bin",
    "/opt/homebrew/bin",
    "/usr/local/bin",
];

pub fn program(kind: AgentKind) -> Option<&'static Program> {
    CATALOGUE.iter().find(|p| p.kind == kind)
}

/// The version in what a program prints for `--version`: the first word that
/// is a dotted number, with a leading `v` dropped.
pub fn parse_version(output: &str) -> Option<String> {
    output
        .lines()
        .take(5)
        .flat_map(|l| l.split_whitespace())
        .find_map(|word| {
            let w = word.trim_matches(|c: char| matches!(c, '(' | ')' | ',' | ':'));
            let w = w.strip_prefix('v').unwrap_or(w);
            let numeric = w
                .split(['-', '+'])
                .next()
                .unwrap_or("")
                .split('.')
                .collect::<Vec<_>>();
            let ok = numeric.len() >= 2
                && numeric
                    .iter()
                    .all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
            ok.then(|| w.to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner asked to see each agent's version. These are what the four
    /// programs printed on the build station on 14 Nov 2026.
    #[test]
    fn versions_are_read_from_what_each_program_prints() {
        for (printed, want) in [
            ("2.1.325 (Claude Code)\n", "2.1.325"),
            ("codex-cli 0.169.0\n", "0.169.0"),
            ("opencode v2.3.0\n", "2.3.0"),
            ("1.5.9\n", "1.5.9"),
            ("tool 3.0.0-beta.2 (build 7)", "3.0.0-beta.2"),
        ] {
            assert_eq!(parse_version(printed).as_deref(), Some(want), "{printed}");
        }
        assert_eq!(parse_version("command not found"), None);
        assert_eq!(parse_version(""), None);
    }

    /// An install command is run on the owner's machine without asking again,
    /// so each must be a fetch over HTTPS from the vendor and nothing else.
    #[test]
    fn install_commands_fetch_over_https_only() {
        for p in CATALOGUE {
            assert!(
                p.install.starts_with("curl -fsSL https://") && !p.install.contains("http://"),
                "{}",
                p.install
            );
        }
    }
}
