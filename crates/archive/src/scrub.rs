//! Secret scrubbing for the searchable copy. Best-effort: it will miss some
//! secrets (Guarantees: Archive). Raw lines are never scrubbed and never leave
//! the station.

use std::sync::OnceLock;

use regex::Regex;

pub const REDACTED: &str = "[redacted]";

fn patterns() -> &'static [Regex] {
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    P.get_or_init(|| {
        [
            // Private key blocks.
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)",
            // Vendor key shapes.
            r"\bsk-ant-[A-Za-z0-9_\-]{16,}",
            r"\bsk-[A-Za-z0-9_\-]{20,}",
            r"\bgh[pousr]_[A-Za-z0-9]{20,}",
            r"\bgithub_pat_[A-Za-z0-9_]{20,}",
            r"\bglpat-[A-Za-z0-9_\-]{16,}",
            r"\bAKIA[0-9A-Z]{16}\b",
            r"\bAIza[0-9A-Za-z_\-]{30,}",
            r"\bxox[abprs]-[A-Za-z0-9\-]{10,}",
            r"\btskey-[A-Za-z0-9\-]{10,}",
            r"\bhf_[A-Za-z0-9]{20,}",
            // JSON web tokens.
            r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
            // Authorization headers.
            r"(?i)\b(?:bearer|basic)\s+[A-Za-z0-9_\-\.=+/]{16,}",
            // URLs with credentials.
            r"(?i)\b[a-z][a-z0-9+.\-]*://[^/\s:@]+:[^/\s:@]+@",
        ]
        .iter()
        .map(|p| Regex::new(p).expect("valid scrub pattern"))
        .collect()
    })
}

fn assignment() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    // name = value where the name says it is a secret. Keeps the name.
    P.get_or_init(|| {
        Regex::new(
            r#"(?i)((?:[a-z0-9_\-]*(?:password|passwd|secret|token|api[_\-]?key|apikey|access[_\-]?key|private[_\-]?key|auth)[a-z0-9_\-]*)["']?\s*[:=]\s*)("[^"\n]{4,}"|'[^'\n]{4,}'|[^\s"',;]{4,})"#,
        )
        .expect("valid assignment pattern")
    })
}

/// Vendor key shapes only: strict enough to act on (block a command, report
/// an exposure) without tripping on ordinary code. `scrub` is wider.
fn key_shapes() -> &'static [(&'static str, Regex)] {
    static P: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    P.get_or_init(|| {
        [
            (
                "private key",
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
            ),
            ("Anthropic", r"\bsk-ant-[A-Za-z0-9_\-]{24,}"),
            ("OpenAI-style", r"\bsk-(?:proj-)?[A-Za-z0-9_\-]{32,}"),
            ("GitHub", r"\bgh[pousr]_[A-Za-z0-9]{30,}"),
            ("GitHub", r"\bgithub_pat_[A-Za-z0-9_]{40,}"),
            ("GitLab", r"\bglpat-[A-Za-z0-9_\-]{20,}"),
            ("AWS", r"\bAKIA[0-9A-Z]{16}\b"),
            ("Google", r"\bAIza[0-9A-Za-z_\-]{35}\b"),
            ("Slack", r"\bxox[abprs]-[A-Za-z0-9\-]{20,}"),
            ("Tailscale", r"\btskey-[a-z]+-[A-Za-z0-9\-]{20,}"),
            ("Hugging Face", r"\bhf_[A-Za-z0-9]{30,}"),
        ]
        .into_iter()
        .map(|(kind, p)| (kind, Regex::new(p).expect("valid key shape")))
        .collect()
    })
}

/// Things in `text` that are shaped like a real provider key: (kind, value).
pub fn find_keys(text: &str) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = vec![];
    for (kind, re) in key_shapes() {
        for m in re.find_iter(text) {
            if !out.iter().any(|(_, v)| v == m.as_str()) {
                out.push((kind, m.as_str().to_string()));
            }
        }
    }
    out
}

pub fn scrub(text: &str) -> String {
    let mut out = text.to_string();
    for p in patterns() {
        if p.is_match(&out) {
            out = p.replace_all(&out, REDACTED).into_owned();
        }
    }
    if assignment().is_match(&out) {
        out = assignment()
            .replace_all(&out, format!("${{1}}{REDACTED}"))
            .into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn find_keys_spots_real_shapes_and_ignores_ordinary_code() {
        let key = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
        let found = find_keys(&format!(
            "curl -H 'x-api-key: {key}' https://api.example.com"
        ));
        assert_eq!(found, vec![("Anthropic", key.to_string())]);
        for ordinary in [
            "let token = get_token(); password = input()",
            "git commit -m 'rotate the api_key handling' && echo sk-short",
            "commit 3f2a9c1e5b7d4e8a9c217d4e5f6a8b90aabbccdd",
            "export OPENAI_API_KEY=$(vault read)",
        ] {
            assert!(find_keys(ordinary).is_empty(), "{ordinary}");
        }
    }

    #[test]
    fn known_shapes_are_removed() {
        for secret in [
            "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "github_pat_11ABCDEFG0abcdefghijkl_mnopqrstuvwxyz",
            "AKIAIOSFODNN7EXAMPLE",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
            "xoxb-123456789012-abcdefghijkl",
        ] {
            let text = format!("export KEYVAL here {secret} and after");
            let out = scrub(&text);
            assert!(!out.contains(secret), "{secret} survived: {out}");
            assert!(out.contains("and after"));
        }
    }

    #[test]
    fn assignments_keep_the_name_and_ordinary_text_is_untouched() {
        assert_eq!(
            scrub("DB_PASSWORD=hunter2hunter2"),
            "DB_PASSWORD=[redacted]"
        );
        assert_eq!(
            scrub(r#""api_key": "abcd1234efgh""#),
            r#""api_key": [redacted]"#
        );
        assert_eq!(
            scrub("postgres://admin:s3cretpw@db.internal/x"),
            "[redacted]db.internal/x"
        );
        let plain =
            "commit 3f2a9c1e5b7d4e8a9c217d4e5f6a8b90aabbccdd fixes the tokenizer in src/auth.rs";
        assert_eq!(scrub(plain), plain);
    }

    proptest! {
        /// A planted key never survives, whatever surrounds it.
        #[test]
        fn planted_secrets_never_survive(
            prefix in "[ -~\\n]{0,60}",
            suffix in "[ -~\\n]{0,60}",
            body in "[A-Za-z0-9]{24,48}",
            which in 0usize..4,
        ) {
            let secret = match which {
                0 => format!("sk-ant-{body}"),
                1 => format!("ghp_{body}"),
                2 => format!("github_pat_{body}"),
                _ => format!("sk-{body}"),
            };
            let text = format!("{prefix} {secret} {suffix}");
            prop_assert!(!scrub(&text).contains(&secret));
        }

        /// Scrubbing twice changes nothing more.
        #[test]
        fn scrub_is_idempotent(text in "[ -~\\n]{0,200}") {
            let once = scrub(&text);
            prop_assert_eq!(scrub(&once), once);
        }
    }
}
