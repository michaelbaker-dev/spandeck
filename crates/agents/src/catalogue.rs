//! The catalogue of agent programs: what each is called, how to read its
//! version, the ways it can be installed and how it updates itself.
//!
//! Every command is fixed here. They are never taken from a caller or from
//! another station; a station's own config file (`[agents.<name>]`) may
//! override them for tests and unusual setups. Each was read from the
//! vendor's documentation or a package registry on 14 Nov 2026 (`source`
//! says where); a way to install that could not be confirmed is left out.
//!
//! Gemini CLI and Grok are not listed: spandeck has no adapter for either.
//! (For when one is written: on 14 Nov 2026 Gemini CLI's documentation
//! listed Windows 11 24H2 and later only, installed with npm; Grok Build
//! documented a PowerShell installer.)
//!
//! Each way to install also says where the newest published version is read
//! without installing anything (`latest`), and each program what it does
//! about updates by itself (`by_itself`). Those were read on 17 Nov 2026 by
//! fetching each address and reading the vendors' install scripts and pages
//! (DECISIONS.md, "Agent programs: every action says what it did").
//!
//! Windows has its own list (`CATALOGUE_WINDOWS`), read from the same
//! pages on the same day: a PowerShell line where the vendor documents one,
//! npm, and WinGet for Claude Code. `all()` gives the list for this system.

use crate::AgentKind;

/// One way to install an agent program.
pub struct Method {
    /// `script` (the vendor's own installer, first wherever there is one:
    /// the default), `npm`, `brew`, or on Windows `winget`.
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
    /// Where the newest published version of a copy installed this way is
    /// read. None: not established, and the station says "could not check".
    pub latest: Option<LatestSource>,
}

/// How the newest published version is read. Nothing is installed, and
/// nothing is sent but the request itself.
pub enum Latest {
    /// GET this address: the body is the version.
    Text(&'static str),
    /// GET this address: JSON, the version is the text at this pointer
    /// (RFC 6901). `{platform}` in the address is this system, as
    /// Antigravity's installer names it (`linux_amd64`).
    Json(&'static str, &'static str),
    /// A command on the station that prints the version, or JSON with the
    /// version at the pointer. It is the package manager the copy was
    /// installed with, asked what it would install.
    Command(&'static str, Option<&'static str>),
}

pub struct LatestSource {
    pub how: Latest,
    /// Where this was established, and when.
    pub source: &'static str,
}

const fn text(url: &'static str, source: &'static str) -> Option<LatestSource> {
    Some(LatestSource {
        how: Latest::Text(url),
        source,
    })
}

const fn json(
    url: &'static str,
    pointer: &'static str,
    source: &'static str,
) -> Option<LatestSource> {
    Some(LatestSource {
        how: Latest::Json(url, pointer),
        source,
    })
}

const fn command(
    line: &'static str,
    pointer: Option<&'static str>,
    source: &'static str,
) -> Option<LatestSource> {
    Some(LatestSource {
        how: Latest::Command(line, pointer),
        source,
    })
}

const NPM_VIEW: &str =
    "docs.npmjs.com, npm-view (`npm view <pkg> version` prints the `latest` tag); the registry's dist-tags for this package were read 17 Nov 2026";
const BREW_CASK: &str =
    "docs.brew.sh/Manpage, `info --json=v2` (casks[0].version); formulae.brew.sh's JSON for this cask was read 17 Nov 2026. Not run against a real Homebrew";
const AGY_MANIFEST: &str =
    "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/{platform}.json";

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
    /// What the program does about updates by itself, and where that was
    /// read (17 Nov 2026). Shown to the owner as it is.
    pub by_itself: &'static str,
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
                latest: text(
                    "https://downloads.claude.ai/claude-code-releases/latest",
                    "claude.ai/install.sh (its bootstrap.sh reads this address), fetched 17 Nov 2026: `2.1.326`",
                ),
            },
            Method {
                id: "npm",
                label: "npm",
                tool: Some("npm"),
                install: "npm install -g @anthropic-ai/claude-code@latest",
                update: Some("npm install -g @anthropic-ai/claude-code@latest"),
                source: "code.claude.com/docs/en/setup, Install with npm (the same line upgrades); registry.npmjs.org/@anthropic-ai/claude-code",
                latest: command("npm view @anthropic-ai/claude-code version", None, NPM_VIEW),
            },
            Method {
                id: "brew",
                label: "Homebrew",
                tool: Some("brew"),
                install: "brew install --cask claude-code",
                update: Some("brew upgrade claude-code"),
                source: "code.claude.com/docs/en/setup, Homebrew; formulae.brew.sh/cask/claude-code",
                latest: command(
                    "brew info --json=v2 --cask claude-code",
                    Some("/casks/0/version"),
                    BREW_CASK,
                ),
            },
        ],
        update_args: &["update"],
        source: "code.claude.com/docs/en/setup (`claude update`)",
        by_itself: "A copy installed with Anthropic's script, or with npm, updates itself in the background: \"Claude Code checks for updates on startup and periodically while running. Updates download and install in the background, then take effect the next time you start Claude Code.\" A copy from Homebrew or WinGet does not (code.claude.com/docs/en/setup).",
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
                latest: json(
                    "https://releases.openai.com/codex/channels/latest",
                    "/tag_name",
                    "chatgpt.com/codex/install.sh (its first source; GitHub's releases/latest is its second), fetched 17 Nov 2026: `rust-v0.169.0`",
                ),
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
                latest: command("npm view @openai/codex version", None, NPM_VIEW),
            },
            Method {
                id: "brew",
                label: "Homebrew",
                tool: Some("brew"),
                install: "brew install --cask codex",
                update: None,
                source: "github.com/openai/codex README; formulae.brew.sh/cask/codex",
                latest: command(
                    "brew info --json=v2 --cask codex",
                    Some("/casks/0/version"),
                    BREW_CASK,
                ),
            },
        ],
        update_args: &["update"],
        source: "`codex --help` 0.169.0 (update)",
        by_itself: "Codex checks for an update when it starts (`check_for_update_on_startup`, on unless set to false) and `codex update` applies one. Whether it installs one without being asked was not established (developers.openai.com/codex/config-reference; /codex/cli/reference).",
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
                latest: json(
                    "https://opencode.ai/update/api/latest/cli/npm",
                    "/version",
                    "opencode.ai/v2/install (the address the script reads), fetched 17 Nov 2026: `2.3.0`. Not GitHub's releases/latest, which is OpenCode 1",
                ),
            },
            // The docs give no update line for either: `opencode upgrade` is used.
            Method {
                id: "npm",
                label: "npm",
                tool: Some("npm"),
                install: "npm install -g @opencode/cli",
                update: None,
                source: "opencode.ai/docs; registry.npmjs.org/@opencode/cli",
                latest: command("npm view @opencode/cli version", None, NPM_VIEW),
            },
            Method {
                id: "brew",
                label: "Homebrew",
                tool: Some("brew"),
                install: "brew install anomalyco/tap/opencode-v2",
                update: None,
                source: "opencode.ai/docs; github.com/anomalyco/homebrew-tap (opencode-v2.rb)",
                latest: command(
                    "brew info --json=v2 anomalyco/tap/opencode-v2",
                    Some("/formulae/0/versions/stable"),
                    "docs.brew.sh/Manpage, `info --json=v2` (formulae[0].versions.stable); the tap's opencode-v2.rb was read 17 Nov 2026. Not run against a real Homebrew",
                ),
            },
        ],
        update_args: &["upgrade"],
        source: "`opencode upgrade --help` 2.3.0 (update)",
        by_itself: "OpenCode checks by itself and, unless its config says otherwise, only tells you: `update` \"defaults to \"notify\"\"; with \"auto\" it installs, and \"does not restart a running server\" (opencode.ai/docs/config).",
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
            latest: json(
                AGY_MANIFEST,
                "/version",
                "antigravity.google/cli/install.sh (the manifest the script reads), fetched 17 Nov 2026: `1.5.10`. Google rolls a version out in stages, so a copy may not be offered it yet",
            ),
        }],
        update_args: &["update"],
        source: "`agy --help` 1.5.9 (update)",
        by_itself: "Antigravity updates itself: it \"contains a native, statically linked self-updater that runs in the background\", looking every 15 minutes while it runs (antigravity.google/docs/cli/troubleshooting).",
    },
];

/// The Windows list. Each line is run by Windows PowerShell
/// (`powershell -NoProfile -ExecutionPolicy Bypass -Command <line>`).
pub const CATALOGUE_WINDOWS: &[Program] = &[
    Program {
        kind: AgentKind::Claude,
        program: "claude",
        version_args: &["--version"],
        methods: &[
            Method {
                id: "script",
                label: "Anthropic's install script",
                tool: None,
                install: "irm https://claude.ai/install.ps1 | iex",
                update: None,
                source: "code.claude.com/docs/en/setup, Native Install, Windows PowerShell",
                latest: text(
                    "https://downloads.claude.ai/claude-code-releases/latest",
                    "claude.ai/install.sh reads this address (17 Nov 2026); install.ps1 itself was not read",
                ),
            },
            Method {
                id: "npm",
                label: "npm",
                tool: Some("npm"),
                install: "npm install -g @anthropic-ai/claude-code@latest",
                update: Some("npm install -g @anthropic-ai/claude-code@latest"),
                source: "code.claude.com/docs/en/setup, Install with npm (win32-x64 is a listed platform; the same line upgrades)",
                latest: command("npm view @anthropic-ai/claude-code version", None, NPM_VIEW),
            },
            Method {
                id: "winget",
                label: "WinGet",
                tool: Some("winget"),
                install: "winget install Anthropic.ClaudeCode",
                update: Some("winget upgrade Anthropic.ClaudeCode"),
                source: "code.claude.com/docs/en/setup, WinGet",
                latest: None,
            },
        ],
        update_args: &["update"],
        source: "code.claude.com/docs/en/setup (`claude update`)",
        by_itself: "A copy installed with Anthropic's script, or with npm, updates itself in the background: \"Claude Code checks for updates on startup and periodically while running. Updates download and install in the background, then take effect the next time you start Claude Code.\" A copy from Homebrew or WinGet does not (code.claude.com/docs/en/setup).",
    },
    Program {
        kind: AgentKind::Codex,
        program: "codex",
        version_args: &["--version"],
        methods: &[
            // The README wraps this line in `powershell -ExecutionPolicy
            // ByPass -c "…"`, which is how spandeck runs every line here.
            Method {
                id: "script",
                label: "OpenAI's install script",
                tool: None,
                install: "irm https://chatgpt.com/codex/install.ps1 | iex",
                update: None,
                source: "github.com/openai/codex README, Windows (PowerShell)",
                latest: json(
                    "https://releases.openai.com/codex/channels/latest",
                    "/tag_name",
                    "chatgpt.com/codex/install.sh reads this address (17 Nov 2026); install.ps1 itself was not read",
                ),
            },
            Method {
                id: "npm",
                label: "npm",
                tool: Some("npm"),
                install: "npm install -g @openai/codex",
                update: None,
                source: "github.com/openai/codex README",
                latest: command("npm view @openai/codex version", None, NPM_VIEW),
            },
        ],
        // Not confirmed for Windows in OpenAI's documentation: it is the
        // command `codex --help` 0.169.0 listed on Linux.
        update_args: &["update"],
        source: "`codex --help` 0.169.0 on Linux (update); not confirmed on Windows",
        by_itself: "Codex checks for an update when it starts (`check_for_update_on_startup`, on unless set to false) and `codex update` applies one. Whether it installs one without being asked was not established (developers.openai.com/codex/config-reference; /codex/cli/reference).",
    },
    // OpenCode's page has no PowerShell installer and says "Windows package
    // managers are not supported": npm is the only documented command.
    Program {
        kind: AgentKind::OpenCode,
        program: "opencode",
        version_args: &["--version"],
        methods: &[Method {
            id: "npm",
            label: "npm",
            tool: Some("npm"),
            install: "npm install -g @opencode/cli",
            update: None,
            source: "opencode.ai/docs",
            latest: command("npm view @opencode/cli version", None, NPM_VIEW),
        }],
        update_args: &["upgrade"],
        source: "`opencode upgrade --help` 2.3.0 on Linux; not confirmed on Windows",
        by_itself: "OpenCode checks by itself and, unless its config says otherwise, only tells you: `update` \"defaults to \"notify\"\"; with \"auto\" it installs, and \"does not restart a running server\" (opencode.ai/docs/config).",
    },
    Program {
        kind: AgentKind::Antigravity,
        program: "agy",
        version_args: &["--version"],
        methods: &[Method {
            id: "script",
            label: "Google's install script",
            tool: None,
            install: "irm https://antigravity.google/cli/install.ps1 | iex",
            update: None,
            source: "antigravity.google/docs/getting-started?tab=cli, Windows (PowerShell)",
            latest: json(
                AGY_MANIFEST,
                "/version",
                "antigravity.google/cli/install.sh reads this address; windows_amd64.json was fetched 17 Nov 2026. install.ps1 itself was not read",
            ),
        }],
        update_args: &["update"],
        source: "`agy --help` 1.5.9 on Linux (update); not confirmed on Windows",
        by_itself: "Antigravity updates itself: it \"contains a native, statically linked self-updater that runs in the background\", looking every 15 minutes while it runs (antigravity.google/docs/cli/troubleshooting).",
    },
];

/// The catalogue for the system this station runs on.
pub fn all() -> &'static [Program] {
    if crate::on_windows() {
        CATALOGUE_WINDOWS
    } else {
        CATALOGUE
    }
}

/// Where the vendors' Windows installers put their programs. `~` is the
/// home folder (`%USERPROFILE%`); `%NAME%` is that environment variable.
/// Claude Code: `%USERPROFILE%\.local\bin` (its setup page, uninstall).
/// Codex: `%LOCALAPPDATA%\Programs\OpenAI\Codex\bin` (its install.ps1).
/// Antigravity: `%LOCALAPPDATA%\agy\bin` (its getting-started page).
/// npm: `%AppData%\npm` (docs.npmjs.com, folders). A new program's folder
/// reaches the daemon's PATH only at its next start, so these matter more
/// than on Unix.
pub const WELL_KNOWN_DIRS_WINDOWS: &[&str] = &[
    "~/.local/bin",
    r"%LOCALAPPDATA%\Programs\OpenAI\Codex\bin",
    r"%LOCALAPPDATA%\agy\bin",
    r"%APPDATA%\npm",
    "~/.opencode/bin",
    "~/.cargo/bin",
];

pub fn well_known_dirs() -> &'static [&'static str] {
    if crate::on_windows() {
        WELL_KNOWN_DIRS_WINDOWS
    } else {
        WELL_KNOWN_DIRS
    }
}

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
    all().iter().find(|p| p.kind == kind)
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

/// The version in what a release source answered: `2.1.326`, a tag such as
/// `rust-v0.169.0`, or Homebrew's `1.5.10,5253791805734912`. None when it
/// is not a version at all (a page of HTML, an error).
pub fn published_version(answer: &str) -> Option<String> {
    let line = answer.trim().lines().next()?.trim().trim_matches('"');
    if line.len() > 80 {
        return None;
    }
    let from = line.find(|c: char| c.is_ascii_digit())?;
    // Only a name may come before the number: `v`, `rust-v`.
    if !line[..from]
        .chars()
        .all(|c| c.is_ascii_alphabetic() || c == '-')
    {
        return None;
    }
    let version = line[from..].split([',', ' ']).next()?;
    parse_version(version).filter(|v| v == version)
}

/// Is `published` a later version than `installed`? Numbers are compared
/// part by part; a pre-release (`-beta.2`) comes before its release. None
/// when either is not a dotted number, and nothing is claimed.
pub fn is_newer(published: &str, installed: &str) -> Option<bool> {
    fn parts(v: &str) -> Option<(Vec<u64>, bool)> {
        let (numbers, pre) = match v.split_once('-') {
            Some((n, _)) => (n, true),
            None => (v.split('+').next()?, false),
        };
        let numbers: Option<Vec<u64>> = numbers.split('.').map(|n| n.parse().ok()).collect();
        Some((numbers?, pre))
    }
    let ((p, p_pre), (i, i_pre)) = (parts(published)?, parts(installed)?);
    let n = p.len().max(i.len());
    let at = |v: &[u64], k: usize| v.get(k).copied().unwrap_or(0);
    for k in 0..n {
        if at(&p, k) != at(&i, k) {
            return Some(at(&p, k) > at(&i, k));
        }
    }
    Some(i_pre && !p_pre)
}

/// This system as Antigravity's installer names it in its manifest's
/// address (its install.sh, 17 Nov 2026).
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{os}_{arch}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner, 17 Nov 2026: "don't know if the version listed is the
    /// latest or not". These are what each release source answered that
    /// day; and a newer version is told from an older one by its numbers
    /// (2.1.1000 is after 2.1.326, which text order gets wrong).
    #[test]
    fn a_published_version_is_read_and_compared_by_its_numbers() {
        for (answered, want) in [
            ("2.1.326", Some("2.1.326")),
            ("rust-v0.169.0", Some("0.169.0")),
            ("\"2.3.0\"\n", Some("2.3.0")),
            ("1.5.10,5253791805734912", Some("1.5.10")),
            ("<!doctype html><title>Sign in</title>", None),
            ("Not Found", None),
        ] {
            assert_eq!(published_version(answered).as_deref(), want, "{answered}");
        }
        assert_eq!(is_newer("2.1.330", "2.1.326"), Some(true));
        assert_eq!(is_newer("2.1.1000", "2.1.326"), Some(true));
        assert_eq!(is_newer("2.1.326", "2.1.326"), Some(false));
        // A copy ahead of what is published (Homebrew's cask of Claude Code
        // follows the stable channel, a week behind) has no update waiting.
        assert_eq!(is_newer("2.1.311", "2.1.326"), Some(false));
        assert_eq!(is_newer("3.0.0", "3.0.0-beta.2"), Some(true));
        assert_eq!(is_newer("latest", "2.1.326"), None);
        // What is asked is fixed here: an https address, or the package
        // manager asked about one named package.
        for p in CATALOGUE.iter().chain(CATALOGUE_WINDOWS) {
            assert!(!p.by_itself.is_empty());
            for m in p.methods {
                match m.latest.as_ref().map(|l| &l.how) {
                    Some(Latest::Text(url)) | Some(Latest::Json(url, _)) => {
                        assert!(url.starts_with("https://"), "{url}")
                    }
                    Some(Latest::Command(line, _)) => assert!(
                        (line.starts_with("npm view ") || line.starts_with("brew info --json=v2 "))
                            && !line.contains([';', '&', '`', '$', '>', '<', '|']),
                        "{line}"
                    ),
                    None => assert_eq!(m.id, "winget"),
                }
            }
        }
    }

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

    /// The same rule for the Windows list: a vendor's PowerShell script
    /// fetched over HTTPS, or one named package from npm or WinGet.
    #[test]
    fn windows_install_commands_are_a_vendor_script_over_https_or_one_package() {
        for p in CATALOGUE_WINDOWS {
            for (i, m) in p.methods.iter().enumerate() {
                assert!(m.id != "script" || i == 0, "a script is the default");
                for line in std::iter::once(m.install).chain(m.update) {
                    let ok = match m.id {
                        "script" => {
                            line.starts_with("irm https://")
                                && line.ends_with(" | iex")
                                && m.tool.is_none()
                        }
                        "npm" => line.starts_with("npm install -g ") && m.tool == Some("npm"),
                        "winget" => {
                            (line.starts_with("winget install ")
                                || line.starts_with("winget upgrade "))
                                && m.tool == Some("winget")
                        }
                        _ => false,
                    };
                    let rest = line.strip_suffix(" | iex").unwrap_or(line);
                    let plain = !line.contains("http://")
                        && !rest.contains([';', '&', '`', '$', '>', '<', '|', '(', '{']);
                    assert!(ok && plain, "{}: {line}", p.program);
                }
            }
            // Every agent on Unix is on Windows too, under the same name.
            let unix = CATALOGUE.iter().find(|u| u.kind == p.kind).unwrap();
            assert_eq!(unix.program, p.program);
        }
        assert_eq!(CATALOGUE_WINDOWS.len(), CATALOGUE.len());
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
