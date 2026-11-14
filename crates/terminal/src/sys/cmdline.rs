//! How a Windows command line is put together. Windows hands a program one
//! string, not a list of arguments, and each kind of program splits it by its
//! own rules: ordinary programs by the C runtime's, `.cmd` and `.bat` files by
//! `cmd.exe`'s, which also expands `%NAME%` and reads `&`, `|`, `<`, `>` as
//! commands. Getting this wrong is how an argument becomes a command.
//!
//! Plain string code with no system calls, so it is compiled and tested on
//! every system although only Windows uses it.

/// Add one argument, quoted so the C runtime (and `CommandLineToArgvW`) reads
/// back exactly `arg`.
pub fn push_arg(line: &mut String, arg: &str) {
    if !line.is_empty() {
        line.push(' ');
    }
    let plain = !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']);
    if plain {
        line.push_str(arg);
        return;
    }
    line.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // 2n+1 backslashes, then the quote: n backslashes and a quote.
                line.extend(std::iter::repeat_n('\\', backslashes + 1));
                backslashes = 0;
            }
            _ => backslashes = 0,
        }
        line.push(c);
    }
    // Backslashes before the closing quote are doubled so it still closes.
    line.extend(std::iter::repeat_n('\\', backslashes));
    line.push('"');
}

/// The command line for an ordinary program.
pub fn program_line(program: &str, args: &[String]) -> String {
    let mut line = String::new();
    push_arg(&mut line, program);
    for a in args {
        push_arg(&mut line, a);
    }
    line
}

/// The command line that runs a `.cmd` or `.bat` file through `cmd.exe`
/// (npm installs `claude.cmd`, `codex.cmd`), the way Rust's standard library
/// does it since CVE-2024-24576: every argument that is not plainly harmless
/// is quoted, a quote is doubled, and `%` is broken so no variable expands.
/// An argument with a line break cannot be passed safely and is refused.
pub fn script_line(script: &str, args: &[String]) -> Result<String, String> {
    if script.contains('"') || script.ends_with('\\') {
        return Err(format!("{script:?} cannot be run through cmd.exe"));
    }
    let mut line = format!("cmd.exe /e:ON /v:OFF /d /c \"\"{script}\"");
    for arg in args {
        if arg.contains(['\n', '\r', '\0']) {
            return Err(format!(
                "an argument with a line break cannot be passed to {script}"
            ));
        }
        line.push(' ');
        // Quote unless every character is known to mean nothing to cmd.exe.
        const HARMLESS: &str = r"#$*+-./:?@\_";
        let quote = arg.is_empty()
            || arg.ends_with('\\')
            || arg.chars().any(|c| {
                (c.is_ascii() && !(c.is_ascii_alphanumeric() || HARMLESS.contains(c)))
                    || c.is_control()
            });
        if quote {
            line.push('"');
        }
        let mut backslashes = 0;
        for c in arg.chars() {
            if c == '\\' {
                backslashes += 1;
            } else {
                if c == '"' {
                    line.extend(std::iter::repeat_n('\\', backslashes));
                    line.push('"');
                } else if c == '%' {
                    // `%%cd:~,%` expands to nothing and stops `%NAME%` being read.
                    line.push_str("%%cd:~,");
                }
                backslashes = 0;
            }
            line.push(c);
        }
        if quote {
            line.extend(std::iter::repeat_n('\\', backslashes));
            line.push('"');
        }
    }
    line.push('"');
    Ok(line)
}

/// Is this a file `cmd.exe` must run, not a program?
pub fn is_script(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".cmd") || lower.ends_with(".bat")
}

/// The file names a command name can stand for: itself when it already has
/// an extension, else itself with each extension in `PATHEXT`, in order.
pub fn names_for(name: &str, pathext: &str) -> Vec<String> {
    let base = name.rsplit(['\\', '/']).next().unwrap_or(name);
    if base.contains('.') {
        return vec![name.to_string()];
    }
    pathext
        .split(';')
        .filter(|e| e.starts_with('.') && e.len() > 1)
        .map(|e| format!("{name}{}", e.to_ascii_lowercase()))
        .collect()
}

/// What `PATHEXT` is when the environment does not say.
pub const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    /// The reason for this test: an argument must reach a pane's program
    /// exactly as given (a prompt with quotes, a Windows path that ends in a
    /// backslash). The expected strings are the C runtime's documented rules.
    #[test]
    fn arguments_survive_the_c_runtime_rules() {
        assert_eq!(
            program_line(
                r"C:\Program Files\x\claude.exe",
                &args(&["--resume", "abc"])
            ),
            r#""C:\Program Files\x\claude.exe" --resume abc"#
        );
        assert_eq!(program_line("p", &args(&[""])), r#"p """#);
        assert_eq!(
            program_line("p", &args(&[r#"say "hi""#])),
            r#"p "say \"hi\"""#
        );
        // A trailing backslash inside quotes must not swallow the closing quote.
        assert_eq!(
            program_line("p", &args(&[r"C:\my dir\"])),
            r#"p "C:\my dir\\""#
        );
        assert_eq!(program_line("p", &args(&[r#"a\"b"#])), r#"p "a\\\"b""#);
        // Backslashes that are not before a quote are left alone.
        assert_eq!(program_line("p", &args(&[r"C:\a\b"])), r"p C:\a\b");
    }

    /// The reason: npm installs agents as `.cmd` files, and `cmd.exe` runs
    /// `&`, `|` and `%NAME%` found in an argument unless it is quoted.
    #[test]
    fn a_cmd_file_argument_cannot_become_a_command() {
        let line = script_line(
            r"C:\Users\me\AppData\Roaming\npm\claude.cmd",
            &args(&["--resume", "a&calc", "100%PATH%", r#"x"y"#]),
        )
        .unwrap();
        assert_eq!(
            line,
            r#"cmd.exe /e:ON /v:OFF /d /c ""C:\Users\me\AppData\Roaming\npm\claude.cmd" --resume "a&calc" "100%%cd:~,%PATH%%cd:~,%" "x""y"""#
        );
        assert!(script_line("x.cmd", &args(&["two\nlines"])).is_err());
        assert!(script_line(r#"x".cmd"#, &[]).is_err());
    }

    /// The reason: `claude` must find `claude.exe` or `claude.cmd`, and a
    /// name that already says which file it is must not be guessed at.
    #[test]
    fn a_command_name_stands_for_each_pathext_file() {
        assert_eq!(
            names_for("claude", ".COM;.EXE;.BAT;.CMD"),
            ["claude.com", "claude.exe", "claude.bat", "claude.cmd"]
        );
        assert_eq!(names_for("claude.cmd", DEFAULT_PATHEXT), ["claude.cmd"]);
        assert_eq!(names_for(r"C:\a.b\agy", ".EXE"), [r"C:\a.b\agy.exe"]);
        assert!(is_script(r"C:\x\Claude.CMD") && !is_script("claude.exe"));
    }
}
