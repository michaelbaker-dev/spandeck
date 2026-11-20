//! A link: a station with no Tailscale, reached over a command of the
//! owner's own, usually their `ssh` (DECISIONS.md, "A station reached over
//! the owner's own SSH").
//!
//!   mux     the frames and the rules: many streams over one byte stream
//!   conn    a running link: the multiplexer with tasks reading and writing
//!   near    the station the owner is at: one task per `[[link]]`, its
//!           carrier, and a port on 127.0.0.1 for that station's deck
//!   helper  `spandeck link --stdio`, the short-lived program on the far
//!           side that the carrier runs
//!
//! Nothing here runs unless the configuration has a `[[link]]` or someone
//! runs `spandeck link`: a station without either has no task, no listener
//! and no extra name it answers to.

pub mod conn;
pub mod helper;
pub mod mux;
pub mod near;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::{invalid, Daemon, Error, Result};

/// In `station.info`'s `caps`: this station has links, on either side.
pub const CAP: &str = "link";

/// One `[[link]]` of the configuration.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct LinkConfig {
    /// How it is shown, and named in `spandeck link`. Letters, digits,
    /// `-`, `_` and `.` only.
    pub name: String,
    /// The port on 127.0.0.1 for that station's deck. Not set: one is
    /// chosen the first time and kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The carrier: a command whose standard input and output reach a
    /// shell on the far side. Run as written, with no shell here.
    pub cmd: Vec<String>,
}

/// What a station holds for its links.
#[derive(Default)]
pub struct LinkState {
    /// This station's own links (it is the near side of each).
    pub(crate) near: Mutex<Vec<Arc<near::Near>>>,
    /// As a far side: the local ports of near sides whose helpers are
    /// attached now, and how many helpers named each.
    ports: Mutex<HashMap<u16, usize>>,
    /// Set while this program image is being replaced: no carrier runs.
    pub(crate) stopping: std::sync::atomic::AtomicBool,
}

/// The options spandeck gives every `ssh` it runs for a link, before the
/// owner's own arguments, so that they win over the owner's configuration
/// file: no terminal, nothing asked behind the owner's back, no forwarding
/// of any kind, no command of the configuration's, a host key that must
/// already be known, and a connection that is noticed when it dies.
pub const SSH_OPTIONS: &[&str] = &[
    "BatchMode=yes",
    "ClearAllForwardings=yes",
    "ForwardAgent=no",
    "ForwardX11=no",
    "PermitLocalCommand=no",
    "RemoteCommand=none",
    "RequestTTY=no",
    "ServerAliveInterval=5",
    "ServerAliveCountMax=2",
    "StrictHostKeyChecking=yes",
];

/// What `ssh` is asked to run on the far side. `spandeck` is installed in
/// `~/.local/bin`, which a shell that is not a login shell often does not
/// search. Inside single quotes, so the far side's own shell (whichever it
/// is) passes it to `sh` untouched.
pub const SSH_REMOTE: &str =
    r#"sh -c 'PATH="$HOME/.local/bin:$PATH"; export PATH; exec spandeck link --stdio'"#;

fn is_ssh(program: &str) -> bool {
    std::path::Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("ssh"))
}

/// The command a link's carrier is started with, from the `cmd` of its
/// `[[link]]`. For `ssh`: spandeck's options, the owner's arguments, the
/// far side's command. For anything else (`docker exec -i box`, say):
/// `spandeck link --stdio` is added as arguments.
pub fn carrier_argv(cmd: &[String]) -> Vec<String> {
    let Some(program) = cmd.first() else {
        return vec![];
    };
    let mut argv = vec![program.clone()];
    if is_ssh(program) {
        argv.push("-T".into());
        for option in SSH_OPTIONS {
            argv.push("-o".into());
            argv.push((*option).to_string());
        }
        argv.extend(cmd[1..].iter().cloned());
        argv.push(SSH_REMOTE.into());
    } else {
        argv.extend(cmd[1..].iter().cloned());
        argv.extend(["spandeck", "link", "--stdio"].map(String::from));
    }
    argv
}

/// Is this a name a link may have?
pub fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.')
}

impl Daemon {
    // ------------------------------------------------------- the far side

    /// The Host values a browser may use here because a link's helper is
    /// attached: `localhost` and `127.0.0.1` with the near side's port. A
    /// page that reaches this station by a trick of DNS arrives with the
    /// attacker's host name, never with these.
    pub fn link_hosts(&self) -> Vec<String> {
        let ports = self.link.ports.lock().unwrap();
        if ports.is_empty() {
            return vec![];
        }
        ports
            .keys()
            .flat_map(|p| [format!("localhost:{p}"), format!("127.0.0.1:{p}")])
            .collect()
    }

    /// This station's web listener on this machine alone, as it is really
    /// bound: what a helper connects to.
    fn link_web(&self) -> Result<std::net::SocketAddr> {
        self.web_addrs
            .lock()
            .unwrap()
            .iter()
            .filter_map(|a| a.parse::<std::net::SocketAddr>().ok())
            .find(|a| a.ip().is_loopback())
            .ok_or_else(|| {
                Error::Invalid(
                    "this station's web listener is not on 127.0.0.1 (see web_bind), so a link cannot reach its deck".into(),
                )
            })
    }

    /// A helper says which port the near side's browser uses. `first`: it
    /// had not named this port before on its connection.
    pub(crate) fn link_attach(&self, params: &Value, first: bool) -> Result<Value> {
        let port = params["port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p != 0)
            .ok_or_else(|| invalid("port is a number from 1 to 65535"))?;
        let web = self.link_web()?;
        if first {
            let mut ports = self.link.ports.lock().unwrap();
            if ports.len() >= 32 && !ports.contains_key(&port) {
                return Err(Error::Invalid(
                    "too many links are attached to this station".into(),
                ));
            }
            *ports.entry(port).or_insert(0) += 1;
            drop(ports);
            self.record(
                "link.attached",
                None,
                None,
                None,
                Some("you"),
                json!({"port": port}),
            );
        }
        Ok(json!({
            "port": port,
            "web": web.to_string(),
            "station": self.station,
            "version": crate::VERSION,
        }))
    }

    /// The helper that named this port is gone.
    pub(crate) fn link_detach(&self, port: u16) {
        let mut ports = self.link.ports.lock().unwrap();
        if let Some(n) = ports.get_mut(&port) {
            *n -= 1;
            if *n == 0 {
                ports.remove(&port);
            }
        }
        drop(ports);
        self.record(
            "link.detached",
            None,
            None,
            None,
            Some("you"),
            json!({"port": port}),
        );
    }

    // ------------------------------------------------------ the near side

    fn link_named(&self, params: &Value) -> Result<Arc<near::Near>> {
        let name = params["name"]
            .as_str()
            .ok_or_else(|| invalid("missing parameter: name"))?;
        let links = self.link.near.lock().unwrap();
        links
            .iter()
            .find(|l| l.name == name)
            .cloned()
            .ok_or_else(|| {
                let names: Vec<&str> = links.iter().map(|l| l.name.as_str()).collect();
                Error::NotFound(if names.is_empty() {
                    "this station has no links: add a [[link]] to its config.toml and restart it"
                        .into()
                } else {
                    format!("no link named {name}; there is: {}", names.join(", "))
                })
            })
    }

    /// `link.list`: every `[[link]]` and how it stands.
    pub(crate) fn link_list(&self) -> Value {
        let links = self.link.near.lock().unwrap();
        Value::Array(links.iter().map(|l| l.view()).collect())
    }

    /// `link.up` and `link.down`: hold a link closed, or open it again and
    /// try at once. Kept across a restart.
    pub(crate) fn link_hold(&self, params: &Value, held: bool) -> Result<Value> {
        let link = self.link_named(params)?;
        link.hold(self, held);
        Ok(link.view())
    }

    /// `link.open`: have the far station sign this machine's browser in,
    /// and say the address to open.
    pub(crate) async fn link_open(&self, params: &Value) -> Result<Value> {
        let link = self.link_named(params)?;
        link.open(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DECISIONS.md, "A station reached over the owner's own SSH": the
    /// options are spandeck's and come before anything of the owner's (the
    /// first one given wins in ssh), nothing is forwarded, and the far
    /// side's command is found in `~/.local/bin`.
    #[test]
    fn ssh_is_run_with_spandecks_options_first_and_no_forwarding() {
        let argv = carrier_argv(&[
            "ssh".into(),
            "-o".into(),
            "ForwardAgent=yes".into(),
            "me@devbox".into(),
        ]);
        let at = |what: &str| argv.iter().position(|a| a == what).unwrap();
        assert_eq!(argv[..2], ["ssh", "-T"]);
        for option in [
            "BatchMode=yes",
            "ClearAllForwardings=yes",
            "ForwardAgent=no",
            "ForwardX11=no",
            "PermitLocalCommand=no",
            "RemoteCommand=none",
            "RequestTTY=no",
            "StrictHostKeyChecking=yes",
        ] {
            assert_eq!(argv[at(option) - 1], "-o");
            assert!(at(option) < at("ForwardAgent=yes"), "{option}");
        }
        // The owner's own configuration and known hosts are used.
        assert!(!argv.iter().any(|a| a == "-F" || a.contains("KnownHosts")));
        assert!(at("me@devbox") == argv.len() - 2);
        assert!(argv.last().unwrap().contains("$HOME/.local/bin"));
        assert!(argv
            .last()
            .unwrap()
            .ends_with("exec spandeck link --stdio'"));
        // Windows' ssh.exe, by its full path, is ssh too.
        assert!(is_ssh(r"C:\Windows\System32\OpenSSH\ssh.exe") || !cfg!(windows));
        assert!(is_ssh("/usr/bin/ssh") && is_ssh("SSH.EXE") && !is_ssh("sshpass"));
        // Anything else is given the helper as arguments.
        let argv = carrier_argv(&["docker".into(), "exec".into(), "-i".into(), "box".into()]);
        assert_eq!(
            argv,
            ["docker", "exec", "-i", "box", "spandeck", "link", "--stdio"]
        );
    }
}
