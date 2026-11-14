//! The catalogue of agent programs: what each is called, how to read its
//! version, the ways it can be installed and how it updates itself.
//!
//! Every command is fixed here. They are never taken from a caller or from
//! another station; a station's own config file (`[agents.<name>]`) may
//! override them for tests and unusual setups. Each was read from the
//! vendor's documentation or a package registry on 14 Nov 2026 (`source`
//! says where); a way to install that could not be confirmed is left out.
//!
//! Gemini CLI and Grok are not listed: their install and update commands
//! were not checked against the vendors' documentation.

use crate::AgentKind;

/// One way to install an agent program.
pub struct Method {
    /// `script` (the vendor's own installer, always first: the default),
    /// `npm` or `brew`.
    pub id: &'static str,
    pub label: &'static str,
    /// The tool that must be on the station for this to be offered.
    pub tool: Option<&'static str>,
    /// One shell line, shown to the user exactly as it is run.
    pub install: &'static str,
    /// One shell line that updates a copy installed this way, where the
    /// vendor documents one. Without it the program's own updater is used.
    pub update: Option<&'static str>,
    /// Where the commands were checked.
    pub source: &'static str,
}

pub struct Program {
    pub kind: AgentKind,
    /// The command name, looked for on PATH and in `WELL_KNOWN_DIRS`.
    pub program: &'static str,
    /// Arguments that print the version.
    pub version_args: &'static [&'static str],
    /// The ways to install it. The first is the vendor's own script.
    pub methods: &'static [Method],
    /// Arguments that make the program update itself.
    pub update_args: &'static [&'static str],
    /// Where the update arguments were checked.
    pub source: &'static str,
}

pub const CATALOGUE: &[Program] = &[
    Program {
        kind: AgentKind::Claude,
        program: "claude",
        version_args: &["--version"],
        methods: &[
            Method {
                id: "script",
                label: "Anthropic's install script",
                tool: None,
                install: "curl -fsSL https://claude.ai/install.sh | bash",
                update: None,
                source: "code.claude.com/docs/en/setup, Native Install",
            },
            Method {
                id: "npm",
                label: "npm",
                tool: Some("npm"),
                install: "npm install -g @anthropic-ai/claude-code@latest",
                update: Some("npm install -g @anthropic-ai/claude-code@latest"),
                source: "code.claude.com/docs/en/setup, Install with npm (the same line upgrades); registry.npmjs.org/@anthropic-ai/claude-code",
            },
            Method {
                id: "brew",
                label: "Homebrew",
                tool: Some("brew"),
                install: "brew install --cask claude-code",
                update: Some("brew upgrade claude-code"),
                source: "code.claude.com/docs/en/setup, Homebrew; formulae.brew.sh/cask/claude-code",
            },
        ],
        update_args: &["update"],
        source: "code.claude.com/docs/en/setup (`claude update`)",
    },
    Program {
        kind: AgentKind::Codex,
        program: "codex",
        version_args: &["--version"],
        methods: &[
            Method {
                id: "script",
                label: "OpenAI's install script",
                tool: None,
                install: "curl -fsSL https://chatgpt.com/codex/install.sh | sh",
                update: None,
                source: "learn.chatgpt.com/docs/codex/cli; github.com/openai/codex README",
            },
            // The docs page holds the npm and Homebrew update lines behind a
            // script and they were not read: `codex update` is used.
            Method {
                id: "npm",
                label: "npm",
                tool: Some("npm"),
                install: "npm install -g @openai/codex",
                update: None,
                source: "github.com/openai/codex README; registry.npmjs.org/@openai/codex",
            },
            Method {
                id: "brew",
                label: "Homebrew",
                tool: Some("brew"),
                install: "brew install --cask codex",
                update: None,
                source: "github.com/openai/codex README; formulae.brew.sh/cask/codex",
            },
        ],
        update_args: &["update"],
        source: "`codex --help` 0.169.0 (update)",
    },
    Program {
        kind: AgentKind::OpenCode,
        program: "opencode",
        version_args: &["--version"],
        methods: &[
            Method {
                id: "script",
                label: "OpenCode's install script",
                tool: None,
                install: "curl -fsSL https://opencode.ai/v2/install | bash",
                update: None,
                source: "opencode.ai/docs",
            },
            // The docs give no update line for either: `opencode upgrade` is used.
            Method {
                id: "npm",
                label: "npm",
                tool: Some("npm"),
                install: "npm install -g @opencode/cli",
                update: None,
                source: "opencode.ai/docs; registry.npmjs.org/@opencode/cli",
            },
            Method {
                id: "brew",
                label: "Homebrew",
                tool: Some("brew"),
                install: "brew install anomalyco/tap/opencode-v2",
                update: None,
                source: "opencode.ai/docs; github.com/anomalyco/homebrew-tap (opencode-v2.rb)",
            },
        ],
        update_args: &["upgrade"],
        source: "`opencode upgrade --help` 2.3.0 (update)",
    },
    // Antigravity's page gives the script only: no npm package or Homebrew
    // formula for `agy` is documented, so none is offered.
    Program {
        kind: AgentKind::Antigravity,
        program: "agy",
        version_args: &["--version"],
        methods: &[Method {
            id: "script",
            label: "Google's install script",
            tool: None,
            install: "curl -fsSL https://antigravity.google/cli/install.sh | bash",
            update: None,
            source: "antigravity.google/docs/getting-started?tab=cli",
        }],
        update_args: &["update"],
        source: "`agy --help` 1.5.9 (update)",
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

/// The line of a failed program's output that says what went wrong: the
/// first that names an error, else the last one. The owner's Codex printed a
/// stack of lines around `Error: spawn …/codex ENOENT`.
pub fn error_line(output: &str) -> Option<String> {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let named = lines.iter().find(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("error") || l.contains("error:") || l.contains(": not found")
    });
    named
        .or(lines.last())
        .map(|l| l.chars().take(300).collect())
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
    /// so each must be the vendor's script fetched over HTTPS, or a package
    /// manager installing one named package, and nothing else.
    #[test]
    fn install_commands_are_a_vendor_script_over_https_or_one_package() {
        for p in CATALOGUE {
            assert_eq!(p.methods[0].id, "script", "the script is the default");
            for m in p.methods {
                for line in std::iter::once(m.install).chain(m.update) {
                    let ok = match m.id {
                        "script" => line.starts_with("curl -fsSL https://") && m.tool.is_none(),
                        "npm" => line.starts_with("npm install -g ") && m.tool == Some("npm"),
                        "brew" => line.starts_with("brew ") && m.tool == Some("brew"),
                        _ => false,
                    };
                    let plain = !line.contains("http://")
                        && !line.contains([';', '&', '`', '$', '>', '<'])
                        && line.matches('|').count() == usize::from(m.id == "script");
                    assert!(ok && plain, "{}: {line}", m.id);
                }
            }
        }
    }
}
