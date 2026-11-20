//! `spandeck link add`: what the owner runs at the machine they sit at to
//! put spandeck on a machine they reach with their own `ssh`, and to add
//! the `[[link]]` to it (DECISIONS.md, "A station reached over the owner's
//! own SSH", the part of 20 Nov 2026 headed `spandeck link add`).
//!
//! Here: what does not need a terminal. Which `ssh` is run and with what,
//! the script the far side is given, the edit of the near side's config
//! file, and the one call that hands the near side's command line a
//! release to send. The command itself (the questions, the sentences) is
//! in the CLI (`link_add.rs`).
//!
//! Nothing here is reached by a browser, an agent or another station: the
//! host is named on the command line or in the config file, and the two
//! calls (`link.release`, `link.reload`) are on the local list (rpc.rs,
//! `LOCAL_METHODS`) and name no host.

use serde_json::{json, Value};

use super::{is_ssh, plain_name, LinkConfig, SSH_OPTIONS};
use crate::daemon::{invalid, Daemon, Result};
use crate::paths::Config;
use crate::supply::{self, Force, NoSupply, Source};

/// What marks the line `spandeck link add` puts in a crontab: by it the
/// line is found again, never written twice, and taken out.
pub const CRON_MARK: &str = "# spandeck: start at boot";

/// The options of `ssh` that take a value (OpenSSH's own list).
const SSH_WITH_VALUE: &str = "BbcDEeFIiJLlmOoPpQRSWw";
/// Those that take none.
const SSH_ALONE: &str = "46AaCfGgKkMNnqsTtVvXxYy";
/// What would make of the connection something a link cannot use: no
/// command, in the background, a terminal, a subsystem, a question of
/// ssh's own, or its input and output given to another host.
const SSH_NOT_FOR_A_LINK: &str = "fGNnOQstVW";

/// The carrier of a new link, from what the owner typed after
/// `spandeck link add`: their own destination and options for `ssh`, with
/// or without the word `ssh` (or the path of one) before them. Returns the
/// `cmd` of the `[[link]]` and the name the link has when none is given.
pub fn carrier_of(typed: &[String]) -> std::result::Result<(Vec<String>, Option<String>), String> {
    let (program, args) = match typed.split_first() {
        Some((first, rest)) if is_ssh(first) => (first.clone(), rest),
        _ => ("ssh".to_string(), typed),
    };
    if args.len() > 64 {
        return Err("too many arguments for ssh".into());
    }
    let mut destination: Option<&str> = None;
    let mut i = 0;
    let mut only_names = false;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;
        if arg.is_empty() || arg.len() > 1024 || arg.chars().any(|c| c.is_control()) {
            return Err("an argument for ssh is empty, too long or not plain text".into());
        }
        if only_names || !arg.starts_with('-') || arg == "-" {
            if destination.is_some() {
                return Err(format!(
                    "`{arg}` comes after the destination, where ssh takes a command to run there. What runs on the far side is spandeck's to say: give the destination and ssh's options only"
                ));
            }
            destination = Some(arg);
            continue;
        }
        if arg == "--" {
            only_names = true;
            continue;
        }
        for (at, c) in arg[1..].char_indices() {
            if SSH_NOT_FOR_A_LINK.contains(c) {
                return Err(format!(
                    "ssh's -{c} cannot be used for a link: spandeck runs its own command on the far side and speaks to it over ssh's input and output"
                ));
            }
            if SSH_WITH_VALUE.contains(c) {
                // Its value: the rest of this argument, or the next one.
                if arg[1 + at + c.len_utf8()..].is_empty() {
                    if i >= args.len() {
                        return Err(format!("ssh's -{c} needs a value"));
                    }
                    i += 1;
                }
                break;
            }
            if !SSH_ALONE.contains(c) {
                return Err(format!("-{c} is not an option of ssh that spandeck knows"));
            }
        }
    }
    let Some(destination) = destination else {
        return Err("no destination: say where ssh is to connect, as you would to ssh itself (`spandeck link add me@devbox`)".into());
    };
    let mut cmd = vec![program];
    cmd.extend(args.iter().cloned());
    Ok((cmd, name_of(destination)))
}

/// A name for the link from ssh's destination: the host's first label, or
/// the whole of an address. `None` when that is not a name a link may have.
fn name_of(destination: &str) -> Option<String> {
    let host = destination.strip_prefix("ssh://").unwrap_or(destination);
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host.split('/').next().unwrap_or(host);
    let host = match host.rsplit_once(':') {
        Some((h, port)) if !h.contains(':') && port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => host,
    };
    let name = if host.parse::<std::net::IpAddr>().is_ok() {
        host.to_string()
    } else {
        host.split('.').next().unwrap_or(host).to_ascii_lowercase()
    };
    plain_name(&name).then_some(name)
}

/// The `ssh` the owner's command line runs to look at the far side and
/// set it up: the link's own options, so that nothing is forwarded and no
/// command of the configuration's runs, but without `BatchMode` and
/// without insisting that the host's key is known already: this once the
/// owner is at the terminal, and ssh asks them what it has to ask.
/// `remote` is what the far side's shell is given.
pub fn interactive_argv(cmd: &[String], remote: &str) -> Vec<String> {
    let Some(program) = cmd.first() else {
        return vec![];
    };
    let mut argv = vec![program.clone(), "-T".to_string()];
    for option in SSH_OPTIONS {
        if option.starts_with("BatchMode=") || option.starts_with("StrictHostKeyChecking=") {
            continue;
        }
        argv.push("-o".into());
        argv.push((*option).to_string());
    }
    argv.extend(cmd[1..].iter().cloned());
    argv.push(remote.to_string());
    argv
}

/// What the far side's shell is given: a shell that reads one line.
pub const REMOTE_SH: &str = "sh -s";

/// The script `spandeck link add` runs on the far side, a Mac or a Linux
/// machine. It is started by `supply::unix_boot_line`, so its shell does
/// not read it from the session's standard input, which is the script's
/// file descriptor 3: the near side's answers and the release come in
/// there. It says what it finds in lines that begin `@spandeck`, and
/// everything else it prints is for the owner to read.
///
/// In order: the platform; the spandeck that is there (`have VERSION
/// LINK`), and then it waits for `install`, `keep` or anything else, which
/// ends it with nothing changed; the release, taken and checked as Add a
/// machine takes one (`supply::unix_receive_lines`) and installed by the
/// README's install script from that file; a config file only where there
/// is none; the proxy and certificate variables of the login shell, by
/// name, and `copy` or `skip`; start at login by the first way that
/// works; and the station running.
pub fn far_script() -> String {
    let receive = supply::unix_receive_lines();
    let install_sh = crate::add::INSTALL_SH;
    format!(
        r##"main() {{
set -u
say() {{ printf '@spandeck %s\n' "$*"; }}
fail() {{ say "fail $1"; exit 1; }}
case "$(uname -s)" in
  Darwin) os=apple-darwin ;;
  Linux) os=unknown-linux-gnu ;;
  *) echo "This machine runs $(uname -s). spandeck runs on macOS and Linux only."; fail unsupported ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) echo "This machine's processor is $(uname -m). There is no spandeck release for it."; fail unsupported ;;
esac
say "platform $arch-$os"
# Where a link looks for spandeck: ~/.local/bin, then the PATH.
bin=$HOME/.local/bin/spandeck
found=
if [ -x "$bin" ]; then found=$bin; else found=$(command -v spandeck 2>/dev/null) || found=; fi
have=none knows=no
if [ -n "$found" ]; then
  have=$("$found" --version 2>/dev/null | sed -n 's/^spandeck \([0-9][0-9A-Za-z.-]*\).*/\1/p' | head -n 1)
  [ -n "$have" ] || have=unknown
  "$found" link --help >/dev/null 2>&1 && knows=yes
fi
say "have $have $knows"
IFS= read -r decision <&3 || decision=
# The README's install script, as it is there.
spandeck_install() {{
sh -s <<'SPANDECK_INSTALL_SH'
{install_sh}
SPANDECK_INSTALL_SH
}}
{receive}
installed=no
case "$decision" in
  install)
    say step install
    spandeck_receive "$arch-$os" asked || fail supply
    SPANDECK_ARCHIVE=$supplied SPANDECK_ARCHIVE_SHA256=$supplied_sum SPANDECK_VERSION=$supplied_version
    SPANDECK_BIN_DIR=$HOME/.local/bin
    export SPANDECK_ARCHIVE SPANDECK_ARCHIVE_SHA256 SPANDECK_VERSION SPANDECK_BIN_DIR
    spandeck_install 3<&- || fail install
    rm -rf "$supplied_dir"
    supplied_dir=
    installed=yes ;;
  keep) bin=$found ;;
  *) say stopped; exit 0 ;;
esac
# A config file only where there is none: one that is there is not touched.
config=${{SPANDECK_CONFIG_DIR:-$HOME/.config/spandeck}}
if [ ! -e "$config/config.toml" ]; then
  tailscale=no
  command -v tailscale >/dev/null 2>&1 && tailscale=yes
  [ -x /Applications/Tailscale.app/Contents/MacOS/Tailscale ] && tailscale=yes
  [ -d "$config" ] || {{ mkdir -p "$config" && chmod 700 "$config"; }} || fail config
  (
    umask 077
    {{
      echo "# Written by spandeck link add, because there was no config file here."
      if [ "$tailscale" = no ]; then
        echo "# This machine has no Tailscale: the station does not look for others through it."
        echo "discovery = false"
      fi
    }} > "$config/config.toml"
  ) || fail config
  say "config written $tailscale"
else
  say "config kept"
fi
# The proxy and the certificates of the login shell, by name. A value is
# never said: the spandeck here reads and writes them itself.
if names=$("$bin" link login-env 2>/dev/null 3<&-); then
  say "env $names"
  if [ -n "$names" ]; then
    IFS= read -r decision <&3 || decision=
    if [ "$decision" = copy ]; then
      if "$bin" link login-env --keep >/dev/null 3<&-; then say "kept $names"; else say "kept ?"; fi
    fi
  fi
else
  say "env ?"
fi
# Nothing more is read from the session: what is started below must not hold it.
exec 3<&-
say step start
running=no
"$bin" status >/dev/null 2>&1 && running=yes
method=link linger=no now=no
case "$(uname -s)" in
  Darwin)
    unit=$HOME/Library/LaunchAgents/dev.spandeck.spandeckd.plist
    if [ -f "$unit" ] || "$bin" service --install >/dev/null 2>&1; then
      method=launchd
      if launchctl print "gui/$(id -u)/dev.spandeck.spandeckd" >/dev/null 2>&1; then
        now=yes
      elif [ "$running" = no ] && launchctl bootstrap "gui/$(id -u)" "$unit" 2>/dev/null; then
        now=yes
      fi
    fi ;;
  *)
    unit=$HOME/.config/systemd/user/spandeckd.service
    user=$(id -un)
    if command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
      # Lingering is what keeps a user's service running with nobody
      # logged in, and starts it at boot. Where it is refused and no
      # service is there yet, none is made: one would stop at logout.
      loginctl enable-linger "$user" >/dev/null 2>&1 || true
      [ "$(loginctl show-user "$user" -p Linger 2>/dev/null)" = "Linger=yes" ] && linger=yes
      if [ "$linger" = yes ] || [ -f "$unit" ]; then
        if [ -f "$unit" ] || "$bin" service --install >/dev/null 2>&1; then
          systemctl --user daemon-reload >/dev/null 2>&1
          if systemctl --user is-active spandeckd >/dev/null 2>&1; then
            systemctl --user enable spandeckd >/dev/null 2>&1 && method=systemd now=yes
          elif [ "$running" = yes ]; then
            # The station that runs was started another way and is not
            # ended for this: the service takes over at the next boot.
            systemctl --user enable spandeckd >/dev/null 2>&1 && method=systemd
          else
            systemctl --user enable --now spandeckd >/dev/null 2>&1 && method=systemd now=yes
          fi
        fi
      fi
    fi
    if [ "$method" = link ] && command -v crontab >/dev/null 2>&1; then
      cron=no had=
      if out=$(crontab -l 2>&1); then had=$out cron=yes
      else case "$out" in *"no crontab"*) cron=yes ;; esac
      fi
      case "$bin$PATH" in *[%\"\\\$\`]*) cron=no ;; esac
      if [ "$cron" = yes ]; then
        if printf '%s\n' "$had" | grep -F -q "{CRON_MARK}"; then
          method=cron
        else
          line="@reboot PATH=\"$HOME/.local/bin:$PATH\" \"$bin\" daemon >/dev/null 2>&1 {CRON_MARK}"
          {{ [ -z "$had" ] || printf '%s\n' "$had"; printf '%s\n' "$line"; }} | crontab - 2>/dev/null
          if crontab -l 2>/dev/null | grep -F -q "{CRON_MARK}"; then method=cron; fi
        fi
        if [ "$method" = cron ]; then
          echo "The line in your crontab: $(crontab -l 2>/dev/null | grep -F "{CRON_MARK}" | head -n 1)"
        fi
      fi
    fi ;;
esac
say "start $method $linger $now"
if ! "$bin" status >/dev/null 2>&1; then
  (nohup "$bin" daemon >/dev/null 2>&1 &)
  n=0
  until "$bin" status >/dev/null 2>&1; do
    n=$((n + 1))
    [ "$n" -le 30 ] || {{ echo "spandeck is installed but did not start."; fail start; }}
    sleep 1
  done
fi
runs() {{ "$bin" status 2>/dev/null | sed -n 's/.*spandeck \([0-9][0-9A-Za-z.-]*\).*/\1/p' | head -n 1; }}
# A station that was already running goes on running the program it was
# started from: after an install it is brought to the one just installed,
# in place, with its terminals kept. It is never ended for this.
if [ "$installed" = yes ]; then
  have=$("$bin" --version 2>/dev/null | sed -n 's/^spandeck \([0-9][0-9A-Za-z.-]*\).*/\1/p' | head -n 1)
  was=$(runs)
  if [ -n "$have" ] && [ -n "$was" ] && [ "$have" != "$was" ]; then
    echo "spandeck $was was already running here: bringing it to $have, with its terminals kept."
    "$bin" upgrade "$bin" >/dev/null 2>&1 || echo "It could not be brought to $have: it still runs $was."
    n=0
    until [ "$(runs)" = "$have" ]; do
      n=$((n + 1))
      [ "$n" -le 30 ] || break
      sleep 1
    done
  fi
fi
say "station $(runs)"
say done
}}
main 3<&0 </dev/null; exit $?
"##
    )
}

/// What `spandeck link remove --uninstall` runs on the far side: the line
/// in the crontab is taken out, and then the script that uninstalls a
/// station from the Control Deck does the rest (add.rs), unchanged.
pub fn far_uninstall_script(erase: bool) -> String {
    format!(
        r#"if command -v crontab >/dev/null 2>&1 && had=$(crontab -l 2>/dev/null) && printf '%s\n' "$had" | grep -F -q "{CRON_MARK}"; then
  printf '%s\n' "$had" | grep -F -v "{CRON_MARK}" | crontab - 2>/dev/null && echo "Took spandeck's line out of your crontab."
fi
{}"#,
        crate::add::uninstall_script(erase, "")
    )
}

/// The near side's config file with one more `[[link]]` at its end.
/// Everything that was there is there still, byte for byte. `Err`, and
/// nothing to write, when a link has that name already or when the result
/// would not read as the same config with this one link more.
pub fn config_with_link(
    text: &str,
    name: &str,
    cmd: &[String],
) -> std::result::Result<String, String> {
    if !plain_name(name) {
        return Err("a link's name is 1 to 32 letters, digits, `-`, `_` or `.`".into());
    }
    let old: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut links: Vec<LinkConfig> = toml::from_str::<Config>(text)
        .map_err(|e| e.to_string())?
        .link;
    if links.iter().any(|l| l.name == name) {
        return Err(format!("there is a link named {name} already"));
    }
    let words: Vec<String> = cmd
        .iter()
        .map(|a| toml::Value::String(a.clone()).to_string())
        .collect();
    let gap = match text {
        "" => "",
        t if t.ends_with('\n') => "\n",
        _ => "\n\n",
    };
    let new = format!(
        "{text}{gap}[[link]]\nname = {}\ncmd = [{}]\n",
        toml::Value::String(name.to_string()),
        words.join(", ")
    );
    links.push(LinkConfig {
        name: name.to_string(),
        port: None,
        cmd: cmd.to_vec(),
    });
    same_but_links(&old, &new, &links)?;
    Ok(new)
}

/// The config file without the `[[link]]` of that name: its lines, from
/// `[[link]]` to the next table, are taken out and nothing else is.
pub fn config_without_link(text: &str, name: &str) -> std::result::Result<String, String> {
    let old: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut links: Vec<LinkConfig> = toml::from_str::<Config>(text)
        .map_err(|e| e.to_string())?
        .link;
    let Some(which) = links.iter().position(|l| l.name == name) else {
        return Err(format!("there is no link named {name}"));
    };
    links.remove(which);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let bare = |line: &str| -> String {
        line.split('#')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .collect()
    };
    let headers: Vec<usize> = (0..lines.len())
        .filter(|i| bare(lines[*i]) == "[[link]]")
        .collect();
    let by_hand = || {
        format!("the [[link]] named {name} is not written in a way spandeck can take out by itself: take it out of the file by hand")
    };
    if headers.len() != links.len() + 1 {
        return Err(by_hand());
    }
    let from = headers[which];
    let mut to = (from + 1..lines.len())
        .find(|i| lines[*i].trim_start().starts_with('['))
        .unwrap_or(lines.len());
    // Comments and empty lines just above the next table are that table's.
    while to < lines.len() && to > from + 1 && {
        let above = lines[to - 1].trim();
        above.is_empty() || above.starts_with('#')
    } {
        to -= 1;
    }
    // The empty line `link add` put before it goes with it.
    let from = if from > 0 && lines[from - 1].trim().is_empty() {
        from - 1
    } else {
        from
    };
    let mut new: String = lines[..from].concat();
    new.push_str(&lines[to..].concat());
    same_but_links(&old, &new, &links).map_err(|_| by_hand())?;
    Ok(new)
}

/// Is `new` the config `old` was, with `links` as its links?
fn same_but_links(
    old: &toml::Table,
    new: &str,
    links: &[LinkConfig],
) -> std::result::Result<(), String> {
    let mut got: toml::Table = toml::from_str(new).map_err(|e| {
        format!("the config file would no longer read ({e}): add the [[link]] by hand")
    })?;
    let has: Vec<LinkConfig> = toml::from_str::<Config>(new)
        .map_err(|e| e.to_string())?
        .link;
    let mut want = old.clone();
    got.remove("link");
    want.remove("link");
    if got != want || has != links {
        return Err("the config file would not be the same but for the link".into());
    }
    Ok(())
}

/// Write a config file's new text in its place, with the permissions the
/// file had (a new one: this user's alone).
pub fn write_config(file: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = file.with_file_name(".config.toml.new");
    let _ = std::fs::remove_file(&part);
    let wrote = spandeck_terminal::sys::write_private(&part, text.as_bytes())
        .and_then(|_| match std::fs::metadata(file) {
            Ok(meta) => std::fs::set_permissions(&part, meta.permissions()),
            Err(_) => Ok(()),
        })
        .and_then(|_| spandeck_terminal::sys::replace_file(&part, file));
    if wrote.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    wrote
}

impl Daemon {
    /// `link.release`: the release for a platform, for the owner's command
    /// line to send over the ssh session it has open (`spandeck link
    /// add`). The same choice of bytes as Add a machine makes
    /// (`release_for`): the published archive checked against its
    /// published SHA-256, the kept copy, this station's own copy only for
    /// its own platform, never anything older than this station. The
    /// answer names a file on this machine; the call names a platform and
    /// nothing else.
    pub(crate) async fn link_release(&self, params: &Value) -> Result<Value> {
        let target = params["target"]
            .as_str()
            .filter(|t| supply::TARGETS.contains(t))
            .ok_or_else(|| invalid("target is the name of a platform that has a release"))?;
        let host = supply::ReleaseHost::new(
            self.cfg.update.url.as_deref(),
            std::time::Duration::from_secs(5),
        )
        .map(|h| h.words())
        .unwrap_or_else(|_| "the release host".into());
        let got = tokio::time::timeout(
            std::time::Duration::from_secs(7 * 60),
            self.release_for(target, Force::No),
        )
        .await;
        let own = crate::manage::release_target().unwrap_or_default();
        let none = |kind: &str, more: Value| {
            let mut v = json!({"none": kind, "host": host, "own": own});
            if let (Some(v), Some(more)) = (v.as_object_mut(), more.as_object()) {
                v.extend(more.clone());
            }
            Ok(v)
        };
        let mut supplied = match got {
            Err(_) => return none("slow", json!({})),
            Ok(Err(NoSupply::NoSuchPlatform)) => return none("platform", json!({})),
            Ok(Err(NoSupply::NotPublished { tag })) => {
                return none("not_published", json!({"tag": tag}))
            }
            Ok(Err(NoSupply::Checksum { tag })) => return none("checksum", json!({"tag": tag})),
            Ok(Err(NoSupply::NoSource { reached, why, .. })) => {
                return none("no_source", json!({"reached": reached, "why": why}))
            }
            Ok(Err(NoSupply::OwnCopy(why))) => return none("own_copy", json!({"why": why})),
            Ok(Ok(supplied)) => supplied,
        };
        let from = match supplied.source {
            Source::Published => "published",
            Source::Kept => "kept",
            Source::OwnCopy => "own",
            Source::Fleet => "fleet",
            Source::FleetOwnCopy => "fleet_own",
        };
        self.record(
            "link.release",
            None,
            None,
            None,
            Some("you"),
            json!({"release": supplied.tag, "platform": supplied.target, "sha256": supplied.sha256,
                "bytes": supplied.size, "from": from, "via": supplied.via}),
        );
        // A copy made for this one sending is the command line's to
        // delete when it has sent it (one left behind goes in an hour).
        let temporary = supplied.temporary.take();
        Ok(json!({
            "tag": supplied.tag,
            "target": supplied.target,
            "path": supplied.path,
            "size": supplied.size,
            "sha256": supplied.sha256,
            "from": from,
            "via": supplied.via,
            "host": host,
            "temporary": temporary,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(typed: &[&str]) -> Vec<String> {
        typed.iter().map(|s| s.to_string()).collect()
    }

    /// "Uses the owner's own ssh ... forwarding options stay off as in
    /// `link::SSH_OPTIONS` (except BatchMode for this interactive step)",
    /// and what the owner types is ssh's destination and options and
    /// nothing that would run a command of their own there.
    #[test]
    fn what_the_owner_types_becomes_the_carrier_and_the_first_ssh_is_theirs_to_answer() {
        let (cmd, name) = carrier_of(&words(&["me@devbox.corp.example"])).unwrap();
        assert_eq!(cmd, ["ssh", "me@devbox.corp.example"]);
        assert_eq!(name.as_deref(), Some("devbox"));
        // With ssh's own options, in any of the ways ssh takes them, and
        // with `ssh` (or the path of one) written out.
        let (cmd, name) = carrier_of(&words(&[
            "ssh",
            "-p",
            "2222",
            "-J",
            "me@bastion",
            "-oUser=me",
            "-4C",
            "-i",
            "~/.ssh/corp",
            "Dev-Box",
        ]))
        .unwrap();
        assert_eq!(cmd.len(), 10);
        assert_eq!(name.as_deref(), Some("dev-box"));
        let (cmd, name) = carrier_of(&words(&["/usr/local/bin/ssh.exe", "10.1.2.3"])).unwrap();
        assert_eq!(cmd[0], "/usr/local/bin/ssh.exe");
        assert_eq!(name.as_deref(), Some("10.1.2.3"));
        assert_eq!(
            carrier_of(&words(&["ssh://me@box:2222"]))
                .unwrap()
                .1
                .as_deref(),
            Some("box")
        );
        // An address that is no name for a link: the owner gives one.
        assert_eq!(carrier_of(&words(&["me@fe80::1"])).unwrap().1, None);
        // Refused: no destination, a command for the far side, and what
        // makes of ssh something a link cannot speak through.
        for typed in [
            &[][..],
            &["-p", "22"],
            &["me@devbox", "rm", "-rf", "x"],
            &["-N", "me@devbox"],
            &["-f", "me@devbox"],
            &["-tt", "me@devbox"],
            &["-W", "other:22", "me@devbox"],
            &["-Z", "me@devbox"],
            &["-p"],
            &["me@dev\nbox"],
        ] {
            assert!(carrier_of(&words(typed)).is_err(), "{typed:?}");
        }
        let argv = interactive_argv(
            &cmd_of(&["ssh", "-o", "ForwardAgent=yes", "me@devbox"]),
            REMOTE_SH,
        );
        let at = |what: &str| argv.iter().position(|a| a == what).unwrap();
        assert_eq!(argv[..2], ["ssh", "-T"]);
        for option in [
            "ClearAllForwardings=yes",
            "ForwardAgent=no",
            "ForwardX11=no",
            "PermitLocalCommand=no",
            "RemoteCommand=none",
        ] {
            assert!(at(option) < at("ForwardAgent=yes"), "{option}");
        }
        // The owner is asked what ssh has to ask, with their own
        // configuration and their own known hosts.
        assert!(!argv.iter().any(|a| a.starts_with("BatchMode")
            || a.starts_with("StrictHostKeyChecking")
            || a == "-F"
            || a.contains("KnownHosts")));
        assert_eq!(argv.last().unwrap(), "sh -s");
    }

    fn cmd_of(typed: &[&str]) -> Vec<String> {
        carrier_of(&words(typed)).unwrap().0
    }

    /// "Append a `[[link]]` entry ... to the near config without
    /// disturbing the rest of the file (refuse a duplicate name; keep
    /// comments and formatting of what is there)", and `link remove`
    /// takes out that entry and nothing else.
    #[test]
    fn a_link_is_added_to_the_end_of_the_config_and_taken_out_again_and_nothing_else_moves() {
        let before = "# my laptop\nstation = \"macbook\"   # its name\ndiscovery = false\n\n[update]\ncheck = false   # no\n";
        let cmd = words(&["ssh", "-p", "2222", "me@dev\"box"]);
        let with = config_with_link(before, "devbox", &cmd).unwrap();
        assert!(with.starts_with(before));
        let cfg: Config = toml::from_str(&with).unwrap();
        assert_eq!(cfg.link.len(), 1);
        assert_eq!(
            (cfg.link[0].name.as_str(), &cfg.link[0].cmd),
            ("devbox", &cmd)
        );
        assert_eq!(cfg.update.check, Some(false));
        // A second one, and the same name refused.
        let two = config_with_link(&with, "other", &words(&["ssh", "other"])).unwrap();
        assert!(two.starts_with(&with));
        assert!(config_with_link(&two, "devbox", &cmd)
            .unwrap_err()
            .contains("already"));
        assert!(config_with_link(before, "dev box", &cmd).is_err());
        // A file with no line end at its end, an empty one, and one that
        // does not read (left alone).
        for text in ["station = \"m\"", ""] {
            let with = config_with_link(text, "devbox", &cmd).unwrap();
            assert!(with.starts_with(text));
            assert_eq!(toml::from_str::<Config>(&with).unwrap().link.len(), 1);
        }
        assert!(config_with_link("station = ", "devbox", &cmd).is_err());
        // Links written as one value cannot be added to at the end.
        assert!(config_with_link("link = []\n[update]\ncheck = false\n", "devbox", &cmd).is_err());
        // Taken out again: each leaves the file as it was before it.
        assert_eq!(config_without_link(&two, "other").unwrap(), with);
        assert_eq!(config_without_link(&with, "devbox").unwrap(), before);
        // The first of two, written by hand with a table after them.
        let by_hand = "[[link]]\nname = \"a\"  # first\ncmd = [\n  \"ssh\",\n  \"a\",\n]\n# about b\n[[link]]\nname = \"b\"\ncmd = [\"ssh\", \"b\"]\n\n[update]\ncheck = false\n";
        let left = config_without_link(by_hand, "a").unwrap();
        assert_eq!(
            left,
            "# about b\n[[link]]\nname = \"b\"\ncmd = [\"ssh\", \"b\"]\n\n[update]\ncheck = false\n"
        );
        assert!(config_without_link(by_hand, "c").is_err());
    }

    /// A pretend far machine for the script: a home folder, a PATH that
    /// holds a few of the system's tools and stand-ins for the rest (so
    /// that this user's own services and crontab are never asked), and a
    /// `spandeck` that is a script.
    #[cfg(unix)]
    struct Machine {
        dir: tempfile::TempDir,
    }

    #[cfg(unix)]
    impl Machine {
        fn at(&self, name: &str) -> std::path::PathBuf {
            self.dir.path().join(name)
        }

        fn tool(&self, name: &str, body: &str) {
            use std::os::unix::fs::PermissionsExt;
            let path = self.at("bin").join(name);
            let _ = std::fs::remove_file(&path);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        /// `systemd`: whether a user's service manager answers there, and
        /// `linger`: whether it lets this user's services outlive a logout.
        fn new(systemd: bool, linger: bool) -> Machine {
            use std::os::unix::fs::PermissionsExt;
            let m = Machine {
                dir: tempfile::tempdir().unwrap(),
            };
            std::fs::create_dir_all(m.at("bin")).unwrap();
            std::fs::create_dir_all(m.at("home/.local/bin")).unwrap();
            let path = std::env::var("PATH").unwrap_or_default();
            for tool in [
                "sh", "sed", "head", "id", "grep", "cat", "mkdir", "chmod", "rm", "sleep",
            ] {
                let real = std::env::split_paths(&path)
                    .map(|d| d.join(tool))
                    .find(|p| p.is_file())
                    .unwrap();
                std::os::unix::fs::symlink(real, m.at("bin").join(tool)).unwrap();
            }
            let log = m.at("asked").display().to_string();
            m.tool(
                "systemctl",
                &format!(
                    "echo \"systemctl $*\" >> {log}\n{}\n[ \"$2\" != is-active ]",
                    if systemd { ":" } else { "exit 1" }
                ),
            );
            m.tool(
                "loginctl",
                &format!(
                    "echo \"loginctl $*\" >> {log}\n[ \"$1\" != show-user ] || echo Linger={}",
                    if linger { "yes" } else { "no" }
                ),
            );
            m.tool("launchctl", "exit 1");
            // A Linux server, whatever machine the test runs on.
            m.tool(
                "uname",
                "case \"${1:-}\" in -m) echo x86_64 ;; *) echo Linux ;; esac",
            );
            m.tool(
                "crontab",
                &format!(
                    "f={}\ncase \"${{1:-}}\" in\n  -l) if [ -f \"$f\" ]; then cat \"$f\"; else echo 'no crontab for me' >&2; exit 1; fi ;;\n  -) cat > \"$f\" ;;\nesac",
                    m.at("crontab").display()
                ),
            );
            // A spandeck newer than any, that knows links and says a
            // station runs; `service --install` writes the unit's file.
            let spandeck = m.at("home/.local/bin/spandeck");
            std::fs::write(
                &spandeck,
                "#!/bin/sh\ncase \"$1 ${2:-}\" in\n  '--version '*) echo 'spandeck 99.0.0' ;;\n  'status '*) echo 'station far  spandeck 99.0.0  0 pane(s)' ;;\n  'link --help') ;;\n  'link login-env') echo '' ;;\n  'service --install') mkdir -p \"$HOME/.config/systemd/user\" && echo unit > \"$HOME/.config/systemd/user/spandeckd.service\" ;;\n  *) exit 2 ;;\nesac\n",
            )
            .unwrap();
            std::fs::set_permissions(&spandeck, std::fs::Permissions::from_mode(0o755)).unwrap();
            m
        }

        /// Run the far side's script as `spandeck link add` has it run,
        /// with what the near side answers: what it said, in marks.
        fn run(&self, answers: &str) -> Vec<String> {
            use std::io::Write;
            let mut child = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(far_script())
                .env_clear()
                .env("PATH", self.at("bin"))
                .env("HOME", self.at("home"))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(answers.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            let said = String::from_utf8_lossy(&out.stdout).to_string();
            assert!(out.status.success(), "{said}\n{out:?}");
            said.lines()
                .filter_map(|l| l.strip_prefix("@spandeck "))
                .map(String::from)
                .collect()
        }

        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.at(name)).unwrap_or_default()
        }
    }

    /// "Far config: only if the far side has no config file, write a
    /// minimal one (`discovery = false` when it has no Tailscale program).
    /// Never overwrite or edit an existing far config", and "start at
    /// login on the far side, first that works": the user's service where
    /// it outlives a logout, else one line in the crontab that is never
    /// written twice, else nothing, said as it is. A station that runs is
    /// not ended or started a second time for any of it. The script is
    /// run by a real `sh`, in a pretend home folder, against stand-ins.
    #[cfg(unix)]
    #[test]
    fn the_far_side_gets_a_config_only_where_there_is_none_and_the_first_start_at_login_that_works()
    {
        use std::os::unix::fs::PermissionsExt;
        // No service manager for a user: a config is written, the crontab
        // gets one line.
        let m = Machine::new(false, false);
        let said = m.run("keep\n");
        assert_eq!(
            said,
            [
                "platform x86_64-unknown-linux-gnu",
                "have 99.0.0 yes",
                "config written no",
                "env ",
                "step start",
                "start cron no no",
                "station 99.0.0",
                "done"
            ]
        );
        let config = m.read("home/.config/spandeck/config.toml");
        assert!(config.ends_with("\ndiscovery = false\n"), "{config}");
        let mode = |name: &str| std::fs::metadata(m.at(name)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("home/.config/spandeck/config.toml"), 0o600);
        assert_eq!(mode("home/.config/spandeck"), 0o700);
        let cron = m.read("crontab");
        assert_eq!(cron.lines().count(), 1, "{cron}");
        assert!(cron.starts_with("@reboot PATH=") && cron.trim_end().ends_with(CRON_MARK));
        assert!(
            cron.contains("/home/.local/bin/spandeck\" daemon "),
            "{cron}"
        );
        assert!(!m.at("home/.config/systemd").exists());
        // Again, with the owner's own lines in both by now: neither file
        // is written a second time.
        let config = format!("{config}# mine\nstation = \"devbox\"\n");
        std::fs::write(m.at("home/.config/spandeck/config.toml"), &config).unwrap();
        let cron = format!("0 5 * * * backup\n{cron}");
        std::fs::write(m.at("crontab"), &cron).unwrap();
        let said = m.run("keep\n");
        assert!(said.contains(&"config kept".to_string()), "{said:?}");
        assert!(said.contains(&"start cron no no".to_string()), "{said:?}");
        assert_eq!(m.read("home/.config/spandeck/config.toml"), config);
        assert_eq!(m.read("crontab"), cron);
        // Told to stop (a No to replacing a spandeck a link cannot
        // reach), it changes nothing.
        let m = Machine::new(true, true);
        assert_eq!(m.run("stop\n")[1..], ["have 99.0.0 yes", "stopped"]);
        assert!(!m.at("home/.config").exists() && !m.at("crontab").exists());
        // A user's service that outlives a logout: that, and no crontab.
        // The station that runs was started another way: the service is
        // switched on for the next start, and is not started beside it.
        let said = m.run("keep\n");
        assert!(
            said.contains(&"start systemd yes no".to_string()),
            "{said:?}"
        );
        assert!(m.at("home/.config/systemd/user/spandeckd.service").exists());
        assert!(!m.at("crontab").exists());
        let asked = m.read("asked");
        assert!(asked.contains("loginctl enable-linger"), "{asked}");
        assert!(
            asked.contains("systemctl --user enable spandeckd\n"),
            "{asked}"
        );
        assert!(
            !asked.contains("--now") && !asked.contains("start"),
            "{asked}"
        );
        // A service manager that does not let this user's services
        // outlive a logout: no service is made, and the crontab is used.
        let m = Machine::new(true, false);
        let said = m.run("keep\n");
        assert!(said.contains(&"start cron no no".to_string()), "{said:?}");
        assert!(!m.at("home/.config/systemd").exists());
        // And with no crontab either, nothing: said as it is.
        m.tool(
            "crontab",
            "echo 'You (me) are not allowed to use this program (crontab)' >&2; exit 1",
        );
        std::fs::remove_file(m.at("crontab")).unwrap();
        let said = m.run("keep\n");
        assert!(said.contains(&"start link no no".to_string()), "{said:?}");
        // The far side downloads nothing: no line of ours asks for a
        // download, and the release is installed from a file.
        let ours = far_script().replace(crate::add::INSTALL_SH, "");
        assert!(!ours.contains("curl") && !ours.contains("wget"));
        assert!(ours.contains("SPANDECK_ARCHIVE=$supplied SPANDECK_ARCHIVE_SHA256=$supplied_sum"));
    }
}
