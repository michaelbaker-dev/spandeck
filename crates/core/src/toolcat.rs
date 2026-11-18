//! The catalogue of tools spandeck can install for agents, and the runtime
//! it installs them with.
//!
//! Everything a station downloads or runs for a tool is fixed here: the
//! package, the commands, the addresses, and the SHA-256 of each runtime
//! archive. None of it is ever taken from a caller or from another station.
//! A caller names a catalogue entry and a version; a station's own config
//! file (`[tools.<id>]`) may replace the install for tests and unusual
//! setups, as `[agents.<name>]` does for agent programs.
//!
//! Each fact says where it was read and when (`source`). To add a tool: one
//! more `Entry` in `CATALOGUE`.
//!
//! The owner's decision of 18 Nov 2026: a tool does not depend on what the
//! machine has, and needs no administrator. So a tool that needs Node gets
//! a Node that spandeck downloads into its own state folder.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use spandeck_terminal::sys;

/// One tool spandeck knows how to install.
pub struct Entry {
    /// What a caller names.
    pub id: &'static str,
    pub name: &'static str,
    /// What it gives an agent, in a line the owner reads.
    pub gives: &'static str,
    /// The npm package. Installed with spandeck's own Node, into the
    /// tool's own folder, never globally.
    pub package: &'static str,
    /// The file the server is started from, inside the tool's folder.
    pub entry: &'static str,
    /// The arguments an agent starts the server with, after the entry file.
    pub args: &'static [&'static str],
    /// A variable the server and its installer are given, naming a folder
    /// under spandeck's state directory (Playwright: where its browsers
    /// are kept), so that nothing is shared with what the machine has.
    pub data_env: Option<&'static str>,
    /// The option that names where the server writes files of its own
    /// (Playwright: screenshots, traces, its console logs). Given a folder
    /// under spandeck's state directory: left alone, Playwright makes
    /// `.playwright-mcp/` in the folder it runs in, which in a pane is the
    /// agent's working folder and often a repository (seen 18 Nov 2026).
    pub output_arg: Option<&'static str>,
    /// Run after the package is installed, each with spandeck's Node in the
    /// tool's folder: the arguments after `node`.
    pub after_install: &'static [Step],
    /// The last step of an install, with spandeck's Node in the tool's
    /// folder: the tool is started once. If it fails the tool is not ready,
    /// and what it printed is "what this station lacks".
    pub start_check: Option<Step>,
    /// Where the newest published version is read: an address whose answer
    /// is JSON, and the pointer (RFC 6901) to the version in it.
    pub latest: (&'static str, &'static str),
    /// The version spandeck was run with, used when the registry cannot be
    /// asked.
    pub tested: &'static str,
    /// What it needs, in the vendor's words.
    pub needs: &'static str,
    /// Where each of these was read, and when.
    pub source: &'static str,
}

pub struct Step {
    /// What the owner is told is happening.
    pub label: &'static str,
    pub node_args: &'static [&'static str],
}

/// Started once, headless, as the last step of an install: the same
/// browser, started the same way, as the server starts for an agent.
const PLAYWRIGHT_START: &str = "require('playwright').chromium.launch({headless:true}).then(async b=>{const v=b.version();await b.close();console.log('started Chromium '+v+' headless and closed it')}).catch(e=>{console.error(String(e&&e.message||e));process.exit(1)})";

pub const CATALOGUE: &[Entry] = &[Entry {
    id: "playwright",
    name: "Playwright",
    gives: "A browser the agent can drive: open pages, click, type, read the page, take screenshots",
    package: "@playwright/mcp",
    entry: "node_modules/@playwright/mcp/cli.js",
    // `chromium` is the browser Playwright downloads itself. The default,
    // `chrome`, is Google Chrome as installed on the machine, which does
    // not exist for Linux on arm64 and is not spandeck's to install.
    args: &["--headless", "--isolated", "--browser", "chromium"],
    data_env: Some("PLAYWRIGHT_BROWSERS_PATH"),
    output_arg: Some("--output-dir"),
    after_install: &[Step {
        label: "Downloading its browser (Chromium)",
        node_args: &["node_modules/playwright/cli.js", "install", "chromium"],
    }],
    start_check: Some(Step {
        label: "Starting the browser once",
        node_args: &["-e", PLAYWRIGHT_START],
    }),
    latest: ("https://registry.npmjs.org/@playwright/mcp/latest", "/version"),
    tested: "0.0.85",
    needs: "Node 18 or newer (spandeck brings its own)",
    source: "registry.npmjs.org/@playwright/mcp/latest, read 18 Nov 2026: version 0.0.85, bin `playwright-mcp` is `cli.js`, engines node >=18, depends on playwright 1.66.0-alpha-2026-11-09, no install scripts. Installed and run by a station on Linux arm64 on 18 Nov 2026 with exactly these commands and arguments: `npm install --ignore-scripts` added 3 packages; `playwright install chromium` fetched Chrome for Testing 158.0.8127.18, its headless shell and FFmpeg; the browser started headless; the server answered and opened a page (DECISIONS.md, Tools for agents). Not run on macOS or Windows",
}];

pub fn all() -> &'static [Entry] {
    CATALOGUE
}

pub fn entry(id: &str) -> Option<&'static Entry> {
    CATALOGUE.iter().find(|e| e.id == id)
}

/// The Node every tool is installed and started with. One version, pinned:
/// a station never runs a Node whose archive is not one of these.
///
/// Read 18 Nov 2026 from https://nodejs.org/dist/v24.22.3/SHASUMS256.txt
/// (v24 is an LTS line; v22.23.5 is the other). To move to another
/// version, replace the version and all six sums from that version's
/// `SHASUMS256.txt`.
pub const NODE_VERSION: &str = "24.22.3";

/// Per platform, as nodejs.org names it: the archive's kind and SHA-256.
const NODE_ARCHIVES: &[(&str, &str, &str)] = &[
    (
        "linux-arm64",
        "tar.gz",
        "261af2af250dfd7e9f25cf151d1b2713a0adf8f10a9ad116c4947f68627affe8",
    ),
    (
        "linux-x64",
        "tar.gz",
        "061ec3e3062115705b56b51fa9cc330ab50d34b6969387d4e4b1a50c6809acc2",
    ),
    (
        "darwin-arm64",
        "tar.gz",
        "ff6e17d4874b76d272212b908cc020d6e815fba5a6c7eaaa06add725840307fd",
    ),
    (
        "darwin-x64",
        "tar.gz",
        "977614af0504c81045e7fd85596dbca0e5f951b96681516dadafedcca8c447f7",
    ),
    (
        "win-x64",
        "zip",
        "e612af99f626ab415ea225af6bce5c971feb448cb5cae250bb1c3fa50c5f7d9f",
    ),
    (
        "win-arm64",
        "zip",
        "b3cc74a295b39d4f273271e96f58c06dd2b35757ca12abec64c7d0598b434f14",
    ),
];

/// The Node archive for one kind of machine.
pub struct NodeArchive {
    /// `node-v24.22.3-linux-arm64`: the archive's one folder.
    pub folder: String,
    pub url: String,
    pub sha256: &'static str,
    pub zip: bool,
}

/// This machine, as nodejs.org names it.
pub fn node_platform(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("linux", "aarch64") => "linux-arm64",
        ("linux", "x86_64") => "linux-x64",
        ("macos", "aarch64") => "darwin-arm64",
        ("macos", "x86_64") => "darwin-x64",
        ("windows", "x86_64") => "win-x64",
        ("windows", "aarch64") => "win-arm64",
        _ => return None,
    })
}

pub fn node_archive(os: &str, arch: &str) -> Option<NodeArchive> {
    let platform = node_platform(os, arch)?;
    let (_, ext, sha256) = NODE_ARCHIVES.iter().find(|(p, _, _)| *p == platform)?;
    let folder = format!("node-v{NODE_VERSION}-{platform}");
    Some(NodeArchive {
        url: format!("https://nodejs.org/dist/v{NODE_VERSION}/{folder}.{ext}"),
        folder,
        sha256,
        zip: *ext == "zip",
    })
}

/// Where the Node program and npm's own entry file are, inside an unpacked
/// archive. npm is started as `node npm-cli.js`: its `npm` launcher looks
/// for a `node` on the path, and there may be none.
pub fn node_files(dir: &Path) -> (PathBuf, PathBuf) {
    if sys::WINDOWS {
        (
            dir.join("node.exe"),
            dir.join("node_modules")
                .join("npm")
                .join("bin")
                .join("npm-cli.js"),
        )
    } else {
        (
            dir.join("bin").join("node"),
            dir.join("lib")
                .join("node_modules")
                .join("npm")
                .join("bin")
                .join("npm-cli.js"),
        )
    }
}

/// Are these the bytes the code was written for? A download that does not
/// match is refused: it is never unpacked and never run.
pub fn verify_sha256(bytes: &[u8], want: &str) -> Result<(), String> {
    let got: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if crate::rpc::constant_eq(&got, &want.to_lowercase()) {
        Ok(())
    } else {
        Err(format!(
            "its SHA-256 is {got}, and the one fixed in spandeck is {want}: refused, nothing was unpacked"
        ))
    }
}

/// A version a caller may name: it becomes a folder's name and one
/// argument of `npm install`, so it is digits, letters, `.`, `-` and `+`,
/// beginning with a digit.
pub fn version_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.starts_with(|c: char| c.is_ascii_digit())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The acceptance criterion: a Node archive whose hash does not match
    /// is refused. Bytes of our own, and one bit of them changed.
    #[test]
    fn an_archive_whose_hash_does_not_match_is_refused() {
        let bytes = b"not really a Node archive".to_vec();
        let sum: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(verify_sha256(&bytes, &sum).is_ok());
        assert!(verify_sha256(&bytes, &sum.to_uppercase()).is_ok());
        let mut changed = bytes.clone();
        changed[3] ^= 1;
        let why = verify_sha256(&changed, &sum).unwrap_err();
        assert!(why.contains("refused") && why.contains(&sum), "{why}");
        // Every platform spandeck runs on has an archive, over HTTPS, with
        // a whole SHA-256 fixed in the code.
        for (os, arch) in [
            ("linux", "aarch64"),
            ("linux", "x86_64"),
            ("macos", "aarch64"),
            ("macos", "x86_64"),
            ("windows", "x86_64"),
            ("windows", "aarch64"),
        ] {
            let a = node_archive(os, arch).unwrap();
            assert!(a.url.starts_with("https://nodejs.org/dist/v"), "{}", a.url);
            assert!(a.sha256.len() == 64 && a.sha256.chars().all(|c| c.is_ascii_hexdigit()));
            assert_eq!(a.zip, os == "windows");
            // The real archive is refused against any other platform's sum.
            assert!(verify_sha256(&bytes, a.sha256).is_err());
        }
        assert!(node_archive("freebsd", "x86_64").is_none());
    }
}
