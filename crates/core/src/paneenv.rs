//! `[pane_env]`: variables a station sets for the process of every pane
//! (DECISIONS.md, "A station reached over the owner's own SSH", 20 Nov
//! 2026). A station started by systemd, cron or a link's helper does not
//! have what a login shell sets, and an agent on a company server needs
//! the proxy and its certificates.
//!
//! A value may hold a credential (a proxy's password). So a value is never
//! logged, never shown and never in an answer of the API: only names are.
//! The config file they are kept in is the station's own, and whoever
//! writes them there (`spandeck link login-env --keep`) leaves it readable
//! by its user alone.
//!
//! With no `[pane_env]` nothing here runs and nothing changes.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::paths::{Config, Paths};

/// How many variables are taken, and how long a name and a value may be.
pub const MAX_VARS: usize = 64;
pub const MAX_NAME: usize = 64;
pub const MAX_VALUE: usize = 4096;

/// The variables `spandeck link add` looks for in the far side's login
/// shell: the proxy, and where the company's certificates are.
pub const LOGIN_NAMES: [&str; 12] = [
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "https_proxy",
    "http_proxy",
    "no_proxy",
    "all_proxy",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
];

/// The line the program run inside the login shell answers with: what a
/// login shell's own files print (a greeting, say) is not mistaken for it.
const MARK: &str = "@spandeck-pane-env";

/// The table as it is written. What is not a text is kept so that the
/// file still reads, and is not used.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneEnv(pub BTreeMap<String, toml::Value>);

/// Names only: a value may be a credential, and a config that is printed
/// while something is looked for must not show one.
impl std::fmt::Debug for PaneEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

impl PaneEnv {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Is this a name a variable of `[pane_env]` may have? Letters, digits
/// and `_`, not beginning with a digit, and never one of spandeck's own
/// (`SPANDECK_…`): a pane's token and socket are not the config's to set.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && !name.to_ascii_uppercase().starts_with("SPANDECK_")
}

/// Is this a value a variable may have? Not empty, capped, one line.
pub fn valid_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_VALUE && !value.chars().any(|c| c.is_control())
}

/// What is set for a pane, and the names that are not and why. Never a
/// value in the second list.
pub fn checked(table: &PaneEnv) -> (Vec<(String, String)>, Vec<String>) {
    let (mut set, mut skipped) = (vec![], vec![]);
    for (name, value) in &table.0 {
        if !valid_name(name) {
            let shown: String = name
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_NAME)
                .collect();
            skipped.push(format!("{shown} (not a name a variable may have)"));
            continue;
        }
        match value.as_str() {
            Some(v) if set.len() >= MAX_VARS => {
                let _ = v;
                skipped.push(format!("{name} (more than {MAX_VARS} variables)"));
            }
            Some(v) if valid_value(v) => set.push((name.clone(), v.to_string())),
            Some(_) => skipped.push(format!(
                "{name} (its value is empty, longer than {MAX_VALUE} bytes or more than one line)"
            )),
            None => skipped.push(format!("{name} (its value is not a text in quotes)")),
        }
    }
    (set, skipped)
}

/// The config file's text with these variables added to `[pane_env]`:
/// after the table's own line when there is one, else as a new table at
/// the end. Nothing else of the file is touched. `Err` when the result
/// would not read as the same config with these variables added, in which
/// case nothing is to be written.
pub fn config_with(text: &str, vars: &[(String, String)]) -> Result<String, String> {
    let old: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let lines: String = vars
        .iter()
        .map(|(name, value)| format!("{name} = {}\n", toml::Value::String(value.clone())))
        .collect();
    let header = text.split_inclusive('\n').position(|line| {
        let line = line.split('#').next().unwrap_or("");
        line.split_whitespace().collect::<String>() == "[pane_env]"
    });
    let new = match header {
        Some(at) => {
            let mut out = String::new();
            for (i, line) in text.split_inclusive('\n').enumerate() {
                out.push_str(line);
                if i == at {
                    if !line.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&lines);
                }
            }
            out
        }
        None => {
            let gap = match text {
                "" => "",
                t if t.ends_with('\n') => "\n",
                _ => "\n\n",
            };
            format!("{text}{gap}[pane_env]\n{lines}")
        }
    };
    // The same config, and these variables: or nothing is written.
    let mut want = old;
    let table = want
        .entry("pane_env")
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let Some(table) = table.as_table_mut() else {
        return Err("pane_env is not a table".into());
    };
    for (name, value) in vars {
        if table
            .insert(name.clone(), toml::Value::String(value.clone()))
            .is_some()
        {
            return Err(format!("{name} is already in [pane_env]"));
        }
    }
    match toml::from_str::<toml::Table>(&new) {
        Ok(got) if got == want => Ok(new),
        Ok(_) => Err("[pane_env] could not be added to without changing something else".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// The variables of `LOGIN_NAMES` that this process has, that may be kept,
/// and that the station's `[pane_env]` does not have yet.
fn found_here(paths: &Paths) -> Result<Vec<(String, String)>, String> {
    let has = Config::load(paths).map_err(|e| e.to_string())?.pane_env;
    Ok(LOGIN_NAMES
        .iter()
        .filter(|name| !has.0.contains_key(**name))
        .filter_map(|name| Some((name.to_string(), std::env::var(name).ok()?)))
        .filter(|(_, value)| valid_value(value))
        .collect())
}

/// Add them to the station's config file, which is then readable by its
/// user alone.
fn keep(paths: &Paths, vars: &[(String, String)]) -> Result<(), String> {
    if vars.is_empty() {
        return Ok(());
    }
    let file = paths.config_file();
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
    };
    let new = config_with(&text, vars)?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let part = file.with_file_name(".config.toml.new");
    let _ = std::fs::remove_file(&part);
    spandeck_terminal::sys::write_private(&part, new.as_bytes())
        .and_then(|_| std::fs::rename(&part, &file))
        .map_err(|e| {
            let _ = std::fs::remove_file(&part);
            e.to_string()
        })
}

/// `spandeck link login-env [--keep]`, on the far side of a link, run by
/// `spandeck link add`: print the names (never a value) of the proxy and
/// certificate variables the user's login shell sets that `[pane_env]`
/// does not have; with `--keep`, write them there, from this machine's own
/// environment. No value leaves the machine or is put on a command line.
///
/// The login shell is asked by running this program inside it
/// (`$SHELL -l -c`), which then reads its own environment: nothing a
/// shell prints is parsed for a value. A shell that cannot be asked this
/// way leaves what this process itself has.
pub fn login_env_main(paths: &Paths, keep_them: bool, inner: bool) -> i32 {
    if !inner {
        if let Some(names) = ask_login_shell(keep_them) {
            println!("{names}");
            return 0;
        }
    }
    let vars = match found_here(paths) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("spandeck link: {e}");
            return 1;
        }
    };
    if keep_them {
        if let Err(e) = keep(paths, &vars) {
            eprintln!("spandeck link: [pane_env] was not written: {e}");
            return 1;
        }
    }
    let names: Vec<&str> = vars.iter().map(|(n, _)| n.as_str()).collect();
    if inner {
        println!("{MARK} {}", names.join(" "));
    } else {
        println!("{}", names.join(" "));
    }
    0
}

/// Run this program inside the user's login shell and take the names it
/// answers with. `None`: there is no such shell, or it did not answer.
fn ask_login_shell(keep_them: bool) -> Option<String> {
    if spandeck_terminal::sys::WINDOWS {
        return None;
    }
    let shell = std::env::var("SHELL").ok().filter(|s| s.starts_with('/'))?;
    let exe = std::env::current_exe().ok()?;
    let exe = exe.to_str()?;
    // Inside single quotes, which every shell reads the same way.
    if exe.contains('\'') {
        return None;
    }
    let line = format!(
        "'{exe}' link login-env --inner{}",
        if keep_them { " --keep" } else { "" }
    );
    let mut child = std::process::Command::new(shell)
        .args(["-l", "-c", &line])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let read = std::thread::spawn(move || {
        use std::io::Read;
        let mut text = Vec::new();
        let _ = (&mut out).take(256 * 1024).read_to_end(&mut text);
        text
    });
    // A login shell that waits for something is not waited for.
    let began = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if began.elapsed() < Duration::from_secs(15) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let text = String::from_utf8_lossy(&read.join().ok()?).to_string();
    text.lines()
        .rev()
        .find_map(|l| l.strip_prefix(MARK))
        .map(|names| {
            names
                .split_whitespace()
                .filter(|n| LOGIN_NAMES.contains(n))
                .collect::<Vec<_>>()
                .join(" ")
        })
}

impl crate::daemon::Daemon {
    /// What every pane started now is given, from the config file as it is
    /// now: a change there needs no restart. A file that does not read
    /// leaves what the station started with.
    pub(crate) fn pane_env(&self) -> Vec<(String, String)> {
        let table = match Config::load(&self.paths) {
            Ok(cfg) => cfg.pane_env,
            Err(_) => self.cfg.pane_env.clone(),
        };
        if table.is_empty() {
            return vec![];
        }
        checked(&table).0
    }

    /// For `spandeck doctor`: the names, and what was left out. `None`
    /// with no `[pane_env]`.
    pub(crate) fn pane_env_words(&self) -> Option<(bool, String)> {
        let table = match Config::load(&self.paths) {
            Ok(cfg) => cfg.pane_env,
            Err(_) => self.cfg.pane_env.clone(),
        };
        if table.is_empty() {
            return None;
        }
        let (set, skipped) = checked(&table);
        let names: Vec<&str> = set.iter().map(|(n, _)| n.as_str()).collect();
        let mut words = match names.len() {
            0 => "no variable of [pane_env] is set for panes".to_string(),
            n => format!(
                "{n} variable{} set for every pane started from now on: {} (values are not shown)",
                if n == 1 { " is" } else { "s are" },
                names.join(", ")
            ),
        };
        if !skipped.is_empty() {
            words.push_str(&format!("; left out: {}", skipped.join(", ")));
        }
        Some((skipped.is_empty(), words))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DECISIONS.md, 20 Nov 2026: "names validated, values capped", and a
    /// value is never shown: what is left out is said by name alone.
    #[test]
    fn names_are_checked_values_are_capped_and_no_value_is_ever_said() {
        let table: PaneEnv = toml::from_str(&format!(
            "HTTPS_PROXY = \"http://user:hunter2@proxy:3128\"\nno_proxy = \"localhost\"\nSPANDECK_TOKEN = \"x\"\n\"A B\" = \"x\"\n\"1X\" = \"x\"\nLONG = \"{}\"\nNUMBER = 3\nTWO = \"a\\nb\"\n",
            "v".repeat(MAX_VALUE + 1)
        ))
        .unwrap();
        let (set, skipped) = checked(&table);
        assert_eq!(
            set,
            [
                (
                    "HTTPS_PROXY".to_string(),
                    "http://user:hunter2@proxy:3128".to_string()
                ),
                ("no_proxy".to_string(), "localhost".to_string()),
            ]
        );
        assert_eq!(skipped.len(), 6, "{skipped:?}");
        let said = format!("{skipped:?} {table:?}");
        assert!(
            !said.contains("hunter2") && !said.contains("vvvv"),
            "{said}"
        );
        assert!(said.contains("HTTPS_PROXY") && said.contains("SPANDECK_TOKEN"));
    }

    /// "Never overwrite or edit an existing far config except the
    /// [pane_env] step": what is added is added to that table and the rest
    /// of the file is as it was, byte for byte; a value with a quote or a
    /// backslash in it is still that value when the file is read.
    #[test]
    fn variables_are_added_to_the_table_and_nothing_else_is_touched() {
        let odd = r#"http://me:p"a\ss@proxy:3128"#.to_string();
        let vars = vec![
            ("HTTPS_PROXY".to_string(), odd.clone()),
            ("SSL_CERT_FILE".to_string(), "/etc/ssl/corp.pem".to_string()),
        ];
        // No table yet: a new one at the end.
        let before = "# mine\nstation = \"devbox\"   # a name\n\n[update]\ncheck = false";
        let after = config_with(before, &vars).unwrap();
        assert!(after.starts_with(before));
        let cfg: Config = toml::from_str(&after).unwrap();
        assert_eq!(cfg.pane_env.0["HTTPS_PROXY"].as_str(), Some(odd.as_str()));
        assert_eq!(cfg.update.check, Some(false));
        // A table that is there, in the middle: added under its own line.
        let before = "station = \"devbox\"\n[pane_env] # proxy\nNO_PROXY = \"localhost\"\n\n[update]\ncheck = false\n";
        let after = config_with(before, &vars).unwrap();
        let cfg: Config = toml::from_str(&after).unwrap();
        assert_eq!(cfg.pane_env.0.len(), 3);
        assert_eq!(cfg.update.check, Some(false));
        let without: String = after
            .lines()
            .filter(|l| !l.starts_with("HTTPS_PROXY") && !l.starts_with("SSL_CERT_FILE"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(without, before);
        // One that is there already is not written a second time, and a
        // file that does not read is left alone.
        assert!(config_with(&after, &vars).is_err());
        assert!(config_with("station = ", &vars).is_err());
        assert!(config_with("pane_env = 3\n", &vars).is_err());
        // An empty file.
        let cfg: Config = toml::from_str(&config_with("", &vars).unwrap()).unwrap();
        assert_eq!(cfg.pane_env.0.len(), 2);
    }
}
