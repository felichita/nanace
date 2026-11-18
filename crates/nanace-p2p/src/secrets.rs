//! Rules that keep secrets out of messages. Every body is scanned twice: by
//! the sender before it leaves and by the receiver before a person or a model
//! sees it. The built-in rules cannot be switched off; a team policy can
//! only add patterns.
//!
//! This recognizes known formats. A secret in an unknown format, encoded, or
//! described in words is not caught.

use std::sync::LazyLock;

use regex::Regex;

struct Rule {
    name: &'static str,
    pattern: Regex,
    /// For rules that match `key = value`: whether the captured value `v`
    /// looks like a secret rather than a word or a placeholder.
    value_is_secret: bool,
}

fn rule(name: &'static str, pattern: &str) -> Rule {
    Rule {
        name,
        pattern: Regex::new(pattern).expect("built-in pattern compiles"),
        value_is_secret: false,
    }
}

static BUILT_IN: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        rule("private_key", r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY( BLOCK)?-----"),
        rule("nanace_token", r"nnc_pat_[A-Za-z0-9_-]{8,}"),
        rule(
            "jwt",
            r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
        ),
        rule("aws_access_key", r"\b(AKIA|ASIA)[0-9A-Z]{16}\b"),
        rule(
            "github_token",
            r"\b(ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{30,}|\bgithub_pat_[A-Za-z0-9_]{30,}",
        ),
        rule("slack_token", r"\bxox[abprs]-[A-Za-z0-9-]{10,}"),
        rule("google_api_key", r"\bAIza[0-9A-Za-z_-]{35}"),
        rule("api_secret_key", r"\b(sk|rk)[-_][A-Za-z0-9_-]{20,}"),
        rule(
            "authorization_header",
            r"(?i)\bauthorization\s*:\s*(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}",
        ),
        rule("url_credentials", r"[a-zA-Z][a-zA-Z0-9+.-]*://[^\s/:@]+:[^\s/@]{3,}@"),
        Rule {
            value_is_secret: true,
            ..rule(
                "secret_assignment",
                r#"(?i)(?:^|[^a-z0-9])(password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key)["']?\s*[:=]\s*["']?(?P<v>[^\s"',;]{8,})"#,
            )
        },
    ]
});

/// A value after `password=` and the like counts when it is not a
/// placeholder and mixes letters with digits or symbols, as generated
/// secrets do and words do not.
fn looks_like_secret(value: &str) -> bool {
    let placeholder = value.starts_with(['<', '$', '{', '[', '%', '*']) || value.contains("...") || value.contains('…');
    let letters = value.chars().any(char::is_alphabetic);
    let other = value
        .chars()
        .any(|c| c.is_ascii_digit() || (c.is_ascii_punctuation() && c != '_' && c != '-'));
    !placeholder && letters && other
}

/// The built-in rules plus a team's own patterns.
pub struct SecretScanner {
    extra: Vec<Regex>,
}

impl SecretScanner {
    /// `patterns` come from a policy the server already validated; one that
    /// does not compile here is an error, never silently dropped.
    pub fn new(patterns: &[String]) -> Result<Self, regex::Error> {
        Ok(Self {
            extra: patterns.iter().map(|p| Regex::new(p)).collect::<Result<_, _>>()?,
        })
    }

    /// The name of the first rule `text` breaks, never the matched text.
    pub fn find(&self, text: &str) -> Option<String> {
        for rule in BUILT_IN.iter() {
            let hit = if rule.value_is_secret {
                rule.pattern
                    .captures_iter(text)
                    .any(|c| c.name("v").is_some_and(|v| looks_like_secret(v.as_str())))
            } else {
                rule.pattern.is_match(text)
            };
            if hit {
                return Some(rule.name.to_owned());
            }
        }
        self.extra
            .iter()
            .position(|pattern| pattern.is_match(text))
            .map(|i| format!("policy_pattern_{i}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_secret_formats_are_caught() {
        let scanner = SecretScanner::new(&[]).unwrap();
        for (text, rule) in [
            ("-----BEGIN OPENSSH PRIVATE KEY-----\nb3Bl", "private_key"),
            ("-----BEGIN PRIVATE KEY-----", "private_key"),
            ("use nnc_pat_AbCdEfGh1234 for the api", "nanace_token"),
            (
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
                "jwt",
            ),
            ("key AKIAIOSFODNN7EXAMPLE here", "aws_access_key"),
            ("ghp_abcdefghijklmnopqrstuvwxyz0123456789", "github_token"),
            ("xoxb-123456789012-abcdefghij", "slack_token"),
            ("sk-ant-api03-abcdefghijklmnopqrstuvwxyz", "api_secret_key"),
            ("sk_live_abcdefghijklmnopqrstuv", "api_secret_key"),
            ("Authorization: Bearer abc.def-ghi_jkl", "authorization_header"),
            ("postgres://nanace:hunter2@db.internal:5432/app", "url_credentials"),
            ("DB_PASSWORD=Tr0ub4dor&3xyz", "secret_assignment"),
            ("\"api_key\": \"a81f9c02e7d44b1c\"", "secret_assignment"),
            ("token: 9f8e7d6c5b4a3f2e1d0c", "secret_assignment"),
        ] {
            assert_eq!(scanner.find(text).as_deref(), Some(rule), "{text}");
        }
    }

    #[test]
    fn ordinary_text_passes() {
        let scanner = SecretScanner::new(&[]).unwrap();
        for text in [
            "Which services depend on security group sg-123?",
            "The token is rotated weekly; ask the owner for access.",
            "set password=<your password> in the env file",
            "password: required",
            "token = ${NANACE_TOKEN}",
            "secret: [redacted]",
            "see https://example.com/docs/auth and http://10.0.0.5:8080/health",
            "commit 9f8e7d6c5b4a3f2e1d0c9f8e7d6c5b4a3f2e1d0c fixed it",
            "the api-key header is documented in [[Ref: API keys]]",
            "",
        ] {
            assert_eq!(scanner.find(text), None, "{text}");
        }
    }

    #[test]
    fn policy_patterns_add_to_the_built_in_rules() {
        let scanner = SecretScanner::new(&["PROJECT-X-[0-9]+".to_owned(), "(?i)codename".to_owned()]).unwrap();
        assert_eq!(
            scanner.find("ask about PROJECT-X-42").as_deref(),
            Some("policy_pattern_0")
        );
        assert_eq!(scanner.find("the Codename is").as_deref(), Some("policy_pattern_1"));
        // Built-in rules still apply and come first.
        assert_eq!(
            scanner.find("codename nnc_pat_AbCdEfGh1234").as_deref(),
            Some("nanace_token")
        );
        assert!(SecretScanner::new(&["(".to_owned()]).is_err());
    }
}
