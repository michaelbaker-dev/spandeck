//! Supplying a release to a machine over an SSH session that is already
//! open (the owner's decision of 17 Nov 2026: a machine that cannot
//! download spandeck is sent it by the station that is adding it).
//!
//! Three parts, none of which knows about Add a machine, so that another
//! road to a host over SSH can use them (DECISIONS.md, "A machine that
//! cannot download is sent the release over SSH"):
//!
//! - which bytes: `Daemon::release_for` gets the published archive for a
//!   platform, checked against its published SHA-256 as `spandeck update`
//!   checks its own (`ReleaseHost`, which `station_update` uses too), kept
//!   in a small cache; or, when the release host cannot be reached and the
//!   platform is this station's own, an archive of the program this
//!   station runs;
//! - what the other machine runs to take them: `unix_receive_lines` and
//!   `windows_receive_lines`;
//! - how they travel: `send`, into the standard input of the session.
//!
//! Nothing here is taken from the other machine but the name of its
//! platform, which must be one of the five that have a release. The
//! address releases come from is the one in the code or in this station's
//! own config (`[update] url`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::daemon::{Daemon, VERSION};
use crate::manage::{release_target, RELEASES};

/// The platforms that have a release. A machine names one of these, and
/// nothing else it says is used.
pub const TARGETS: [&str; 5] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
];

/// How many versions the cache keeps.
const KEPT_VERSIONS: usize = 2;

/// A Windows release is a .zip; the others are .tar.gz.
pub fn archive_ext(target: &str) -> &'static str {
    if target.ends_with("windows-msvc") {
        "zip"
    } else {
        "tar.gz"
    }
}

/// A platform in the owner's words.
pub fn platform_words(target: &str) -> &'static str {
    match target {
        "aarch64-apple-darwin" => "macOS (Apple silicon)",
        "x86_64-apple-darwin" => "macOS (Intel)",
        "x86_64-unknown-linux-gnu" => "Linux (x86-64)",
        "aarch64-unknown-linux-gnu" => "Linux (arm64)",
        "x86_64-pc-windows-msvc" => "Windows (x86-64)",
        _ => "that system",
    }
}

/// The release platform of a station, from what it says of itself
/// (`station.info`: `os` and `arch`, Rust's names for them).
pub fn target_of(os: &str, arch: &str) -> Option<&'static str> {
    let os = match os {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        "windows" => "pc-windows-msvc",
        _ => return None,
    };
    let want = format!("{arch}-{os}");
    TARGETS.iter().copied().find(|t| *t == want)
}

/// A size as it is shown: "18 MB".
pub fn size_words(bytes: u64) -> String {
    if bytes < 1_000_000 {
        format!("{} KB", bytes.div_ceil(1000).max(1))
    } else {
        format!("{} MB", (bytes + 500_000) / 1_000_000)
    }
}

/// A release's name is `v` and then letters, digits, dots and dashes.
pub fn valid_tag(tag: &str) -> bool {
    tag.len() <= 40
        && tag.starts_with('v')
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
}

/// Whether version `a` is older than `b` ("0.4.9" is older than "0.4.10",
/// and "0.5.0-beta.1" is older than "0.5.0"). A leading `v` is ignored.
pub fn older(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> (Vec<u64>, bool) {
        let v = v.trim_start_matches('v');
        let (numbers, pre) = match v.split_once('-') {
            Some((n, _)) => (n, true),
            None => (v, false),
        };
        (
            numbers.split('.').map(|n| n.parse().unwrap_or(0)).collect(),
            pre,
        )
    }
    let ((a, a_pre), (b, b_pre)) = (parts(a), parts(b));
    match a.cmp(&b) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Equal => a_pre && !b_pre,
        std::cmp::Ordering::Greater => false,
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Why something could not be had from the release host.
#[derive(Debug)]
pub enum Fetch {
    /// It did not answer, or the transfer broke off. The words say which.
    Unreachable(String),
    /// It answered with an HTTP error (404: there is no such file).
    Http(String, u16),
    /// What came does not match the SHA-256 published beside it.
    Checksum,
}

/// Where releases are published, for this station: the project's GitHub
/// releases, or `[update] url` in this station's config. Nothing a caller,
/// another station or another machine says changes it.
pub struct ReleaseHost {
    base: String,
    http: reqwest::Client,
}

impl ReleaseHost {
    pub fn new(configured: Option<&str>, timeout: Duration) -> Result<ReleaseHost, String> {
        let http = crate::http::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(10).min(timeout))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(ReleaseHost {
            base: configured
                .unwrap_or(RELEASES)
                .trim_end_matches('/')
                .to_string(),
            http,
        })
    }

    /// "GitHub", or the host of the address the owner configured.
    pub fn words(&self) -> String {
        if self.base == RELEASES {
            return "GitHub".to_string();
        }
        let rest = self.base.split_once("://").map_or(&*self.base, |(_, r)| r);
        rest.split('/').next().unwrap_or(rest).to_string()
    }

    async fn get(&self, url: String) -> Result<reqwest::Response, Fetch> {
        let r = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| Fetch::Unreachable(format!("could not reach {url}: {e}")))?;
        if !r.status().is_success() {
            return Err(Fetch::Http(url, r.status().as_u16()));
        }
        Ok(r)
    }

    /// The newest release's name: the "latest" page redirects to its tag.
    pub async fn latest(&self) -> Result<String, Fetch> {
        let r = self.get(format!("{}/latest", self.base)).await?;
        let tag = r.url().path().rsplit('/').next().unwrap_or("").to_string();
        if !valid_tag(&tag) {
            return Err(Fetch::Unreachable(format!("{tag:?} is not a release name")));
        }
        Ok(tag)
    }

    fn file(&self, tag: &str, target: &str) -> String {
        let ext = archive_ext(target);
        format!("{}/download/{tag}/spandeck-{tag}-{target}.{ext}", self.base)
    }

    /// The SHA-256 published beside a release's archive.
    pub async fn published_sum(&self, tag: &str, target: &str) -> Result<String, Fetch> {
        let url = format!("{}.sha256", self.file(tag, target));
        let text = self
            .get(url.clone())
            .await?
            .text()
            .await
            .map_err(|e| Fetch::Unreachable(format!("could not read {url}: {e}")))?;
        let sum = text.split_whitespace().next().unwrap_or("").to_lowercase();
        if sum.len() != 64 || !sum.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Fetch::Checksum);
        }
        Ok(sum)
    }

    /// A release's archive for a platform, only if it matches the SHA-256
    /// published beside it. This is the check `spandeck update` makes.
    pub async fn verified(&self, tag: &str, target: &str) -> Result<(Vec<u8>, String), Fetch> {
        let archive = self
            .get(self.file(tag, target))
            .await?
            .bytes()
            .await
            .map_err(|e| Fetch::Unreachable(format!("the download did not finish: {e}")))?;
        let want = self.published_sum(tag, target).await?;
        if !crate::rpc::constant_eq(&sha256_hex(&archive), &want) {
            return Err(Fetch::Checksum);
        }
        Ok((archive.to_vec(), want))
    }
}

/// Where the bytes that are sent came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Downloaded now, or held from before and found to match the SHA-256
    /// the release host publishes now.
    Published,
    /// Downloaded and checked earlier; the release host does not answer now.
    Kept,
    /// An archive of the program this station runs.
    OwnCopy,
}

/// A release archive ready to be sent.
#[derive(Debug)]
pub struct Supplied {
    /// The release's name: `v0.4.10`.
    pub tag: String,
    pub target: String,
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
    pub source: Source,
    /// Made for this one sending, and deleted after it.
    temporary: Option<PathBuf>,
}

impl Supplied {
    /// A file that is already here, as something to send.
    pub fn of_file(
        tag: &str,
        target: &str,
        path: &Path,
        source: Source,
    ) -> std::io::Result<Supplied> {
        let bytes = std::fs::read(path)?;
        Ok(Supplied {
            tag: tag.to_string(),
            target: target.to_string(),
            path: path.to_path_buf(),
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
            source,
            temporary: None,
        })
    }
}

impl Drop for Supplied {
    fn drop(&mut self) {
        if let Some(dir) = &self.temporary {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Why there is nothing to send.
#[derive(Debug)]
pub enum NoSupply {
    /// The machine named a platform that has no release.
    NoSuchPlatform,
    /// The release host answers and has no archive for that platform.
    NotPublished { tag: String },
    /// A download here did not match its published SHA-256.
    Checksum { tag: String },
    /// The release host cannot be reached from here (or does not have this
    /// station's version), nothing is held from before, and the machine is
    /// a different kind from this one, so this station's own copy would
    /// not run there.
    NoSource { reached: bool, why: String },
    /// This station's own files could not be put together.
    OwnCopy(String),
}

/// What the owner asked for by hand (`fleet.add` with `send`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Force {
    #[default]
    No,
    /// Send over SSH even though the machine could download.
    Send,
    /// Send this station's own copy, as if the release host did not answer.
    OwnCopy,
}

impl Daemon {
    fn supply_dir(&self) -> PathBuf {
        self.paths.state_dir.join("supply")
    }

    /// The archive to send to a machine of platform `target`.
    ///
    /// The version is the one the machine would have downloaded for itself:
    /// the newest release, as the release host says. It is never older than
    /// what this station runs: a release host that names an older one as
    /// its newest is asked for this station's version instead.
    pub async fn release_for(
        &self,
        target: &str,
        force: Force,
    ) -> std::result::Result<Supplied, NoSupply> {
        let Some(target) = TARGETS.iter().copied().find(|t| *t == target) else {
            return Err(NoSupply::NoSuchPlatform);
        };
        let host = ReleaseHost::new(self.cfg.update.url.as_deref(), Duration::from_secs(300))
            .map_err(|e| NoSupply::NoSource {
                reached: false,
                why: e,
            })?;
        let mut reached = false;
        let mut why = String::new();
        let newest = match force {
            Force::OwnCopy => Err(Fetch::Unreachable(
                "this station's own copy was asked for".into(),
            )),
            _ => host.latest().await,
        };
        match newest {
            Ok(latest) => {
                reached = true;
                let own = format!("v{VERSION}");
                let behind = older(&latest, VERSION);
                let tag = if behind { own } else { latest };
                match self.published(&host, &tag, target).await {
                    Ok(supplied) => return Ok(supplied),
                    Err(Fetch::Checksum) => return Err(NoSupply::Checksum { tag }),
                    // The newest release has nothing for that platform.
                    Err(Fetch::Http(_, 404)) if !behind => {
                        return Err(NoSupply::NotPublished { tag })
                    }
                    Err(Fetch::Http(url, code)) => why = format!("{url}: HTTP {code}"),
                    Err(Fetch::Unreachable(e)) => why = e,
                }
            }
            Err(Fetch::Http(url, code)) => why = format!("{url}: HTTP {code}"),
            Err(Fetch::Unreachable(e)) => why = e,
            Err(Fetch::Checksum) => {}
        }
        tracing::info!(%why, target, "the release host gave nothing to send");
        // Held from before, checked then against its published SHA-256, and
        // not older than this station.
        if force != Force::OwnCopy {
            if let Some(kept) = self.kept(target) {
                return Ok(kept);
            }
        }
        if release_target().as_deref() == Some(target) {
            return self.own_copy(target).await.map_err(NoSupply::OwnCopy);
        }
        Err(NoSupply::NoSource { reached, why })
    }

    /// The published archive of `tag` for `target`: the cache's copy when
    /// it still matches the SHA-256 the release host publishes, else a
    /// download checked against it. A copy that no longer matches is thrown
    /// away and replaced.
    async fn published(
        &self,
        host: &ReleaseHost,
        tag: &str,
        target: &str,
    ) -> std::result::Result<Supplied, Fetch> {
        let dir = self.supply_dir().join(tag);
        let path = dir.join(format!("{target}.{}", archive_ext(target)));
        let sum_file = dir.join(format!("{target}.sha256"));
        if path.is_file() {
            let want = host.published_sum(tag, target).await?;
            let held = tokio::fs::read(&path).await.unwrap_or_default();
            if crate::rpc::constant_eq(&sha256_hex(&held), &want) {
                return Ok(Supplied {
                    tag: tag.to_string(),
                    target: target.to_string(),
                    size: held.len() as u64,
                    path,
                    sha256: want,
                    source: Source::Published,
                    temporary: None,
                });
            }
            tracing::warn!(file = %path.display(), "a held release no longer matches its published checksum; downloading it again");
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&sum_file);
        }
        let (archive, sum) = host.verified(tag, target).await?;
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(&dir)?;
            let part = dir.join(format!(".{target}.part"));
            std::fs::write(&part, &archive)?;
            std::fs::rename(&part, &path)?;
            std::fs::write(&sum_file, format!("{sum}\n"))
        };
        write().map_err(|e| Fetch::Unreachable(format!("could not keep the download: {e}")))?;
        self.prune_supply();
        Ok(Supplied {
            tag: tag.to_string(),
            target: target.to_string(),
            size: archive.len() as u64,
            path,
            sha256: sum,
            source: Source::Published,
            temporary: None,
        })
    }

    /// The versions the cache holds, newest first.
    fn kept_versions(&self) -> Vec<String> {
        let mut tags: Vec<String> = std::fs::read_dir(self.supply_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|t| valid_tag(t))
            .collect();
        tags.sort_by(|a, b| {
            if older(a, b) {
                std::cmp::Ordering::Greater
            } else if older(b, a) {
                std::cmp::Ordering::Less
            } else {
                a.cmp(b)
            }
        });
        tags
    }

    fn prune_supply(&self) {
        for old in self.kept_versions().into_iter().skip(KEPT_VERSIONS) {
            let _ = std::fs::remove_dir_all(self.supply_dir().join(old));
        }
    }

    /// The newest held archive for `target` that is not older than this
    /// station and still matches the SHA-256 it was checked against when it
    /// was downloaded.
    fn kept(&self, target: &str) -> Option<Supplied> {
        for tag in self.kept_versions() {
            if older(&tag, VERSION) {
                break;
            }
            let dir = self.supply_dir().join(&tag);
            let path = dir.join(format!("{target}.{}", archive_ext(target)));
            let Ok(held) = std::fs::read(&path) else {
                continue;
            };
            let want = std::fs::read_to_string(dir.join(format!("{target}.sha256")))
                .unwrap_or_default()
                .trim()
                .to_string();
            if want.len() == 64 && crate::rpc::constant_eq(&sha256_hex(&held), &want) {
                return Some(Supplied {
                    tag,
                    target: target.to_string(),
                    size: held.len() as u64,
                    path,
                    sha256: want,
                    source: Source::Kept,
                    temporary: None,
                });
            }
        }
        None
    }

    /// An archive of the program this station runs, laid out as a release
    /// is, so that the install script makes of it what it makes of one:
    /// `spandeck` and `sd` (on Windows also `spandeckd.exe` and the pane
    /// host). Only for a machine of this station's own platform.
    async fn own_copy(&self, target: &str) -> std::result::Result<Supplied, String> {
        let windows = spandeck_terminal::sys::WINDOWS;
        let tag = format!("v{VERSION}");
        let name = format!("spandeck-{tag}-{target}");
        let work = self
            .supply_dir()
            .join(format!("own-{}", &crate::paths::random_token()[..8]));
        let tidy = |e: String| {
            let _ = std::fs::remove_dir_all(&work);
            e
        };
        let inside = work.join(&name);
        std::fs::create_dir_all(&inside).map_err(|e| e.to_string())?;
        // The program that is running, not a file that may have been
        // replaced since it started.
        let running = if cfg!(target_os = "linux") {
            PathBuf::from("/proc/self/exe")
        } else {
            std::env::current_exe().unwrap_or_else(|_| self.exe.clone())
        };
        let beside = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let exe = |n: &str| {
            if windows {
                format!("{n}.exe")
            } else {
                n.to_string()
            }
        };
        let copy = |from: &Path, to: &str| {
            std::fs::copy(from, inside.join(to))
                .map(|_| ())
                .map_err(|e| format!("{}: {e}", from.display()))
        };
        copy(&running, &exe("spandeck")).map_err(tidy)?;
        // The short command is the same program under another name: the
        // file beside the running one when there is one, else the program.
        let sd = beside.join(exe("sd"));
        copy(if sd.is_file() { &sd } else { &running }, &exe("sd")).map_err(tidy)?;
        if windows {
            for more in ["spandeckd.exe", "spandeck-pane-host.exe"] {
                copy(&beside.join(more), more).map_err(tidy)?;
            }
        }
        let file = work.join(format!("{name}.{}", archive_ext(target)));
        // The system's tar, as an update uses it to unpack: on Windows it
        // (bsdtar) writes a .zip when told the name ends in one.
        let made = tokio::process::Command::new("tar")
            .args(if windows {
                vec!["-a", "-cf"]
            } else {
                vec!["-czf"]
            })
            .arg(&file)
            .arg("-C")
            .arg(&work)
            .arg(&name)
            .output()
            .await
            .map_err(|e| tidy(format!("could not run tar: {e}")))?;
        if !made.status.success() {
            return Err(tidy(
                String::from_utf8_lossy(&made.stderr).trim().to_string(),
            ));
        }
        let bytes = tokio::fs::read(&file)
            .await
            .map_err(|e| tidy(e.to_string()))?;
        let _ = std::fs::remove_dir_all(&inside);
        Ok(Supplied {
            tag,
            target: target.to_string(),
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
            path: file,
            source: Source::OwnCopy,
            temporary: Some(work),
        })
    }

    /// `spandeck doctor`: whether this station can download a release.
    pub async fn release_host_check(&self) -> serde_json::Value {
        let host = match ReleaseHost::new(self.cfg.update.url.as_deref(), Duration::from_secs(5)) {
            Ok(h) => h,
            Err(e) => {
                return serde_json::json!({"name": "release host", "ok": false, "required": false, "detail": e})
            }
        };
        let words = host.words();
        let (ok, detail) = match host.latest().await {
            Ok(tag) => (
                true,
                format!("{words} answers (newest release: {tag}): `spandeck update` downloads here"),
            ),
            Err(_) => (
                false,
                format!("this station cannot reach the release host ({words}), so `spandeck update` cannot download here. A release reaches it through the fleet: from another station's Control Deck, uninstall spandeck on this one (its history and settings are kept; its panes are closed) and add it again, and the release is sent over SSH. Or copy a release archive here and run `spandeck upgrade <the spandeck program in it>`"),
            ),
        };
        serde_json::json!({"name": "release host", "ok": ok, "required": false, "detail": detail})
    }
}

/// What a Mac or Linux machine runs to take a release from the station at
/// the other end of its session. The session's standard input is the
/// machine's file descriptor 3.
///
/// `spandeck_receive PLATFORM WHY` asks (`@spandeck supply PLATFORM WHY`)
/// and reads one line of answer: `release TAG SIZE SHA256`, or anything
/// else, which means there is nothing to be had (the station says why to
/// its owner itself) and the function returns 1. It then says
/// `@spandeck ready` and takes exactly SIZE bytes. Fewer, or bytes that do
/// not have that SHA-256, are deleted, and the script ends with
/// `@spandeck fail transfer`. On success `$supplied` is the file,
/// `$supplied_version` its release and `$supplied_sum` its SHA-256; the
/// folder it is in is deleted when the script ends, however it ends.
///
/// The script that uses it must have `say` and `fail`, and must end the
/// line that starts it with `exit`, so that the shell never reads what
/// follows on its standard input as commands.
pub fn unix_receive_lines() -> &'static str {
    r#"supplied_dir= supplied= supplied_version= supplied_sum=
trap '[ -z "$supplied_dir" ] || rm -rf "$supplied_dir"' EXIT
trap 'exit 1' HUP INT TERM PIPE
spandeck_sum() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d ' ' -f 1
}
spandeck_receive() {
  say "supply $1 $2"
  IFS= read -r answer <&3 || answer=
  set -- $answer
  [ $# -eq 4 ] && [ "$1" = release ] || return 1
  case "$3" in '' | *[!0-9]*) return 1 ;; esac
  supplied_version=$2 supplied_sum=$4
  supplied_dir=$(mktemp -d) || return 1
  supplied=$supplied_dir/release
  say ready
  head -c "$3" <&3 > "$supplied"
  got=$(wc -c < "$supplied" | tr -d ' ')
  if [ "$got" != "$3" ]; then
    rm -rf "$supplied_dir"
    echo "Only $got of $3 bytes arrived, so nothing was installed."
    fail transfer
  fi
  if [ "$(spandeck_sum "$supplied")" != "$supplied_sum" ]; then
    rm -rf "$supplied_dir"
    echo "What arrived does not have the SHA-256 it was sent with, so nothing was installed."
    fail transfer
  fi
  echo "Received $3 bytes, and their SHA-256 matches."
}"#
}

/// The first line a Windows machine is sent when the session's standard
/// input must stay open after the script: PowerShell runs this line, which
/// reads the script up to `WINDOWS_SCRIPT_ENDS` and runs it, so that what
/// follows can be read by the script.
pub const WINDOWS_READS_SCRIPT: &str = "$spandeckText = New-Object System.Text.StringBuilder; while ($true) { $spandeckLine = [Console]::In.ReadLine(); if (($null -eq $spandeckLine) -or ($spandeckLine -eq '#spandeck-end-of-script')) { break }; [void]$spandeckText.AppendLine($spandeckLine) }; Invoke-Expression $spandeckText.ToString()";
pub const WINDOWS_SCRIPT_ENDS: &str = "#spandeck-end-of-script";

/// The same for Windows PowerShell 5.1, as lines to put in a script (not a
/// function: what a function writes becomes its result). They expect
/// `$target` and `$supplyWhy`, and `Fail`. They leave `$supplied` (the
/// file, or `$null` when there is nothing to be had), `$suppliedDir`,
/// `$suppliedVersion` and `$suppliedSum`.
///
/// PowerShell reads its standard input as text, so the bytes arrive as
/// lines of base64, through the same reader the script itself came through
/// (see `WINDOWS_READS_SCRIPT`), until SIZE bytes have been decoded. The
/// marks are written straight to the console's output, so that they are
/// sent before the script waits for the answer.
pub fn windows_receive_lines() -> &'static str {
    r#"$supplied = $null; $suppliedDir = $null; $suppliedVersion = ''; $suppliedSum = ''
[Console]::Out.WriteLine("@spandeck supply $target $supplyWhy"); [Console]::Out.Flush()
$answer = "$([Console]::In.ReadLine())" -split ' '
if (($answer.Count -eq 4) -and ($answer[0] -eq 'release') -and ($answer[2] -match '^[0-9]+$')) {
    $suppliedVersion = $answer[1]; $suppliedSize = [int64]$answer[2]; $suppliedSum = $answer[3]
    $suppliedDir = Join-Path ([IO.Path]::GetTempPath()) ("spandeck-supply-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $suppliedDir | Out-Null
    $supplied = Join-Path $suppliedDir 'release.zip'
    [Console]::Out.WriteLine('@spandeck ready'); [Console]::Out.Flush()
    $got = [int64]0
    $out = [IO.File]::Create($supplied)
    try {
        while ($got -lt $suppliedSize) {
            $line = [Console]::In.ReadLine()
            if ($null -eq $line) { break }
            $bytes = [Convert]::FromBase64String($line)
            $out.Write($bytes, 0, $bytes.Length)
            $got += $bytes.Length
        }
    } catch { Write-Output "$_" } finally { $out.Dispose() }
    $sum = ''
    if ($got -eq $suppliedSize) {
        $sha = [System.Security.Cryptography.SHA256]::Create()
        $stream = [System.IO.File]::OpenRead($supplied)
        try { $sum = ([System.BitConverter]::ToString($sha.ComputeHash($stream)) -replace '-', '').ToLower() } finally { $stream.Dispose(); $sha.Dispose() }
    }
    if ($got -ne $suppliedSize) {
        Remove-Item -Recurse -Force $suppliedDir -ErrorAction SilentlyContinue
        Write-Output "Only $got of $suppliedSize bytes arrived, so nothing was installed."
        Fail 'transfer'
    }
    if ($sum -ne $suppliedSum) {
        Remove-Item -Recurse -Force $suppliedDir -ErrorAction SilentlyContinue
        Write-Output 'What arrived does not have the SHA-256 it was sent with, so nothing was installed.'
        Fail 'transfer'
    }
    Write-Output "Received $suppliedSize bytes, and their SHA-256 matches."
}"#
}

/// The line that answers a machine's request: what is coming.
pub fn answer_line(supplied: &Supplied) -> String {
    format!(
        "release {} {} {}\n",
        supplied.tag, supplied.size, supplied.sha256
    )
}

/// How many bytes go in one line of base64 to a Windows machine: a
/// multiple of three, so that every line but the last has no padding.
const WINDOWS_LINE_BYTES: usize = 48_000;

/// Send the archive into the session's standard input: as it is for a Mac
/// or Linux machine, as lines of base64 for a Windows one. `progress` is
/// told how many bytes of the archive have been handed to the session.
pub async fn send<W: AsyncWrite + Unpin>(
    to: &mut W,
    supplied: &Supplied,
    windows: bool,
    mut progress: impl FnMut(u64),
) -> std::io::Result<()> {
    let mut file = tokio::fs::File::open(&supplied.path).await?;
    let mut buf = vec![
        0u8;
        if windows {
            WINDOWS_LINE_BYTES
        } else {
            64 * 1024
        }
    ];
    let mut sent = 0u64;
    loop {
        // A whole buffer, or what is left: a line of base64 must not end
        // in the middle of three bytes.
        let mut n = 0;
        while n < buf.len() {
            match file.read(&mut buf[n..]).await? {
                0 => break,
                more => n += more,
            }
        }
        if n == 0 {
            break;
        }
        if windows {
            let mut line = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
            line.push('\n');
            to.write_all(line.as_bytes()).await?;
        } else {
            to.write_all(&buf[..n]).await?;
        }
        to.flush().await?;
        sent += n as u64;
        progress(sent);
    }
    if sent != supplied.size {
        return Err(std::io::Error::other(
            "the archive changed while it was being sent",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's decision of 17 Nov 2026 has "no downgrade below the
    /// fleet's version": that rests on telling an older release from a
    /// newer one, where "0.4.9" sorts after "0.4.10" as text.
    #[test]
    fn an_older_release_is_told_from_a_newer_one() {
        for (a, b) in [
            ("0.4.9", "0.4.10"),
            ("v0.4.10", "0.5.0"),
            ("v0.5.0-beta.1", "v0.5.0"),
            ("0.4", "0.4.1"),
        ] {
            assert!(older(a, b), "{a} < {b}");
            assert!(!older(b, a), "{b} > {a}");
        }
        assert!(!older("v0.4.10", "0.4.10"));
    }

    /// The same decision, for a Windows machine: PowerShell reads its
    /// standard input as text, so the archive goes as lines of base64. What
    /// is written must decode, line by line, to the archive's bytes, with
    /// no line ending inside a group of three. (Nothing here runs
    /// PowerShell: the reading side is the Windows-only test in add.rs.)
    #[tokio::test]
    async fn what_a_windows_machine_is_sent_decodes_to_the_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("release.zip");
        let bytes: Vec<u8> = (0..(2 * WINDOWS_LINE_BYTES + 1001))
            .map(|i| (i * 7 % 251) as u8)
            .collect();
        std::fs::write(&path, &bytes).unwrap();
        let supplied =
            Supplied::of_file("v9.9.9", "x86_64-pc-windows-msvc", &path, Source::Published)
                .unwrap();
        let (mut wire, mut steps) = (Vec::new(), vec![]);
        send(&mut wire, &supplied, true, |n| steps.push(n))
            .await
            .unwrap();
        let text = String::from_utf8(wire).unwrap();
        let mut back = vec![];
        for line in text.lines() {
            assert!(line.len() <= 64_000);
            back.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(line)
                    .unwrap(),
            );
        }
        assert_eq!(sha256_hex(&back), supplied.sha256);
        assert_eq!(steps.last(), Some(&supplied.size));
        assert_eq!(text.lines().count(), 3);
        // A Mac or Linux machine is sent the bytes themselves.
        let mut wire = Vec::new();
        send(&mut wire, &supplied, false, |_| {}).await.unwrap();
        assert_eq!(wire, bytes);
    }
}
