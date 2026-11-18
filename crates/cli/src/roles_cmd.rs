//! `spandeck roles`: the role library. The list, one role's prompt, and the
//! owner's changes: edit, new, reset, remove, and sending a pane its role
//! again.
//!
//! A prompt is printed as it is stored, as text.

use std::io::Read;
use std::path::Path;

use clap::Subcommand;
use serde_json::{json, Value};
use spandeck_core::client::Client;

#[derive(Subcommand)]
pub enum RolesCmd {
    /// Print a role's prompt as it is stored, placeholders as written.
    Show {
        name: String,
        /// With the placeholders filled, as a pane would be sent it.
        #[arg(long)]
        filled: bool,
        /// With --filled: the project whose name and panes fill it.
        #[arg(long, requires = "filled")]
        project: Option<String>,
        /// A changed built-in role: print the built-in text instead.
        #[arg(long, conflicts_with = "filled")]
        builtin: bool,
    },
    /// Change a role: its prompt from a file (or from standard input when
    /// no --file is given and something is piped in), and any of the
    /// fields below. What is not named stays as it is.
    Edit {
        name: String,
        #[command(flatten)]
        fields: Fields,
    },
    /// Add a role of your own. Its prompt comes from --file, from standard
    /// input, or with --copy from another role.
    New {
        name: String,
        /// Start from this role's prompt and fields.
        #[arg(long)]
        copy: Option<String>,
        #[command(flatten)]
        fields: Fields,
    },
    /// A built-in role you changed goes back to the built-in text.
    Reset { name: String },
    /// Remove a role of your own. A built-in role cannot be removed.
    Remove { name: String },
    /// Send a pane its role's prompt again.
    Send { pane: String },
}

#[derive(clap::Args)]
pub struct Fields {
    /// The prompt, from this file (`-`: standard input).
    #[arg(long)]
    file: Option<String>,
    #[arg(long)]
    title: Option<String>,
    /// One line saying what the role is for.
    #[arg(long)]
    summary: Option<String>,
    /// standing (on the team for the whole job) or visit (one review).
    #[arg(long)]
    kind: Option<String>,
    /// One line, for {{budget}}. An empty word takes it away.
    #[arg(long)]
    budget: Option<String>,
    /// The program a pane with this role runs when none is named.
    #[arg(long)]
    agent: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    effort: Option<String>,
    #[arg(long)]
    permission_mode: Option<String>,
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// The prompt a command was given: a file's contents, or what was piped in.
/// Standard input is read without being asked for (`--file -`) only when
/// nothing else was given to change: a script that passes `--title` with
/// its own input still open is not kept waiting.
fn prompt_given(file: Option<&str>, alone: bool) -> anyhow::Result<Option<String>> {
    use std::io::IsTerminal;
    let piped = || -> anyhow::Result<String> {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        Ok(s)
    };
    match file {
        Some("-") => Ok(Some(piped()?)),
        Some(path) => Ok(Some(
            std::fs::read_to_string(Path::new(path))
                .map_err(|e| anyhow::anyhow!("could not read {path}: {e}"))?,
        )),
        None if alone && !std::io::stdin().is_terminal() => {
            let s = piped()?;
            Ok((!s.trim().is_empty()).then_some(s))
        }
        None => Ok(None),
    }
}

fn change_of(fields: &Fields, prompt: Option<String>) -> Value {
    let mut change = serde_json::Map::new();
    let mut put = |key: &str, v: &Option<String>| {
        if let Some(v) = v {
            change.insert(key.to_string(), json!(v));
        }
    };
    put("prompt", &prompt);
    put("title", &fields.title);
    put("summary", &fields.summary);
    put("kind", &fields.kind);
    put("budget", &fields.budget);
    put("agent", &fields.agent);
    put("model", &fields.model);
    put("effort", &fields.effort);
    put("permission_mode", &fields.permission_mode);
    Value::Object(change)
}

fn kind_words(kind: &str) -> &'static str {
    if kind == "visit" {
        "a short visit"
    } else {
        "on the team"
    }
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// `spandeck roles`: the list.
pub fn list(c: &mut Client, json_out: bool) -> anyhow::Result<()> {
    let v = c.call("roles.list", json!({}))?;
    if json_out {
        print_json(&v);
        return Ok(());
    }
    for r in v["roles"].as_array().into_iter().flatten() {
        let mark = match text(&r["source"]) {
            "changed" => "changed",
            "own" => "yours",
            _ => "",
        };
        println!(
            "{:<16} {:<14} {:<8} {}",
            text(&r["name"]),
            kind_words(text(&r["kind"])),
            mark,
            text(&r["title"])
        );
    }
    Ok(())
}

pub fn run(c: &mut Client, json_out: bool, cmd: RolesCmd) -> anyhow::Result<()> {
    match cmd {
        RolesCmd::Show {
            name,
            filled,
            project,
            builtin,
        } => {
            let v = c.call(
                "roles.get",
                json!({"name": name, "filled": filled, "project": project}),
            )?;
            if json_out {
                print_json(&v);
                return Ok(());
            }
            let role = &v["role"];
            let prompt = if filled {
                text(&role["filled"])
            } else if builtin {
                match role["builtin_role"]["prompt"].as_str() {
                    Some(p) => p,
                    None if role["builtin"] == json!(true) => text(&role["prompt"]),
                    None => anyhow::bail!("{name} is a role of your own: it has no built-in text"),
                }
            } else {
                text(&role["prompt"])
            };
            print!("{prompt}");
            if !prompt.ends_with('\n') {
                println!();
            }
        }
        RolesCmd::Edit { name, fields } => {
            // Only a role that is there is edited: a name typed wrong
            // would otherwise make a new one.
            c.call("roles.get", json!({"name": name}))?;
            let alone = change_of(&fields, None)
                .as_object()
                .is_some_and(|m| m.is_empty());
            let change = change_of(&fields, prompt_given(fields.file.as_deref(), alone)?);
            if change.as_object().is_some_and(|m| m.is_empty()) {
                anyhow::bail!("nothing to change: give --file <path> (or pipe the prompt in), or one of --title --summary --kind --budget --agent --model --effort --permission-mode");
            }
            let v = c.call("roles.set", json!({"name": name, "role": change}))?;
            if json_out {
                print_json(&v);
                return Ok(());
            }
            let role = &v["role"];
            match text(&role["source"]) {
                "changed" => println!("Saved {name}. It is a built-in role changed by you: `spandeck roles reset {name}` goes back to the built-in text. Panes are sent it from now; `spandeck roles send <pane>` sends it to one that is running."),
                "builtin" => println!("{name} is the built-in text again: nothing of yours is kept for it."),
                _ => println!("Saved {name}. Panes are sent it from now; `spandeck roles send <pane>` sends it to one that is running."),
            }
        }
        RolesCmd::New { name, copy, fields } => {
            if c.call("roles.get", json!({"name": name})).is_ok() {
                anyhow::bail!("there is already a role called {name}: `spandeck roles edit {name}` changes it");
            }
            let mut change = match &copy {
                Some(from) => {
                    let got = c.call("roles.get", json!({"name": from}))?;
                    let r = &got["role"];
                    json!({
                        "title": r["title"], "summary": r["summary"], "prompt": r["prompt"],
                        "kind": r["kind"], "budget": r["budget"], "agent": r["agent"],
                        "model": r["model"], "effort": r["effort"],
                        "permission_mode": r["permission_mode"],
                    })
                }
                None => json!({}),
            };
            let given = change_of(
                &fields,
                prompt_given(fields.file.as_deref(), copy.is_none())?,
            );
            for (k, v) in given.as_object().into_iter().flatten() {
                change[k] = v.clone();
            }
            if change["prompt"]
                .as_str()
                .is_none_or(|p| p.trim().is_empty())
            {
                anyhow::bail!(
                    "a role needs a prompt: give --file <path>, pipe it in, or --copy <role>"
                );
            }
            let v = c.call("roles.set", json!({"name": name, "role": change, "was": 0}))?;
            if json_out {
                print_json(&v);
                return Ok(());
            }
            println!("Added the role {name}. Start a pane with it: spandeck pane new --agent claude --role {name}");
        }
        RolesCmd::Reset { name } => {
            let v = c.call("roles.reset", json!({"name": name}))?;
            if json_out {
                print_json(&v);
                return Ok(());
            }
            println!("{name} is the built-in text again.");
        }
        RolesCmd::Remove { name } => {
            let v = c.call("roles.remove", json!({"name": name}))?;
            if json_out {
                print_json(&v);
                return Ok(());
            }
            println!("Removed the role {name}. A pane that has it keeps the name and is sent nothing from now.");
        }
        RolesCmd::Send { pane } => {
            let v = c.call("roles.send", json!({"pane": pane}))?;
            if json_out {
                print_json(&v);
                return Ok(());
            }
            if v["sent"] == json!(true) {
                println!(
                    "Sent {} its role ({}, the {} text).",
                    text(&v["name"]),
                    text(&v["role"]),
                    text(&v["text"])
                );
            } else {
                anyhow::bail!("nothing was sent: {}", text(&v["why"]));
            }
        }
    }
    Ok(())
}
