//! Validation for user-supplied values at the API boundary.

use thiserror::Error;

pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_CONTENT_BYTES: usize = 256 * 1024;
pub const MAX_TAGS: usize = 20;
pub const MAX_TAG_CHARS: usize = 50;
pub const MAX_NAME_CHARS: usize = 100;
pub const MAX_EMAIL_BYTES: usize = 254;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("{field}: {reason}")]
pub struct ValidationError {
    pub field: &'static str,
    pub reason: &'static str,
}

fn invalid(field: &'static str, reason: &'static str) -> ValidationError {
    ValidationError { field, reason }
}

/// Slugs identify orgs and teams in URLs and must stay stable and ASCII.
pub fn slug(field: &'static str, value: &str) -> Result<(), ValidationError> {
    let bytes = value.as_bytes();
    let valid_len = (2..=63).contains(&bytes.len());
    let valid_chars = bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
    let valid_edges = bytes.first().is_some_and(|b| *b != b'-') && bytes.last().is_some_and(|b| *b != b'-');
    if valid_len && valid_chars && valid_edges {
        Ok(())
    } else {
        Err(invalid(
            field,
            "must be 2-63 chars of a-z, 0-9 or '-', not starting or ending with '-'",
        ))
    }
}

pub fn display_name(field: &'static str, value: &str) -> Result<String, ValidationError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(invalid(field, "must not be empty"));
    }
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(invalid(field, "is too long"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(invalid(field, "must not contain control characters"));
    }
    Ok(trimmed.to_owned())
}

/// Returns the normalized (trimmed, lowercased) email.
pub fn email(value: &str) -> Result<String, ValidationError> {
    let normalized = value.trim().to_ascii_lowercase();
    let Some((local, domain)) = normalized.split_once('@') else {
        return Err(invalid("email", "must be an email address"));
    };
    let well_formed = !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && normalized.len() <= MAX_EMAIL_BYTES
        && normalized.bytes().all(|b| b.is_ascii_graphic());
    if well_formed {
        Ok(normalized)
    } else {
        Err(invalid("email", "must be an email address"))
    }
}

pub fn title(value: &str) -> Result<String, ValidationError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(invalid("title", "must not be empty"));
    }
    if trimmed.chars().count() > MAX_TITLE_CHARS {
        return Err(invalid("title", "is too long"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(invalid("title", "must not contain control characters"));
    }
    Ok(trimmed.to_owned())
}

/// Title prefixes of pages only a team admin may write: team policies and
/// the wiki's service pages.
pub const PROTECTED_TITLE_PREFIXES: [&str; 2] = ["wiki", "policy"];

/// Shown when a write to a protected page is refused.
pub const PROTECTED_TITLE_MESSAGE: &str = "Policy: and Wiki: pages are edited only by the teamspace owner";

/// Invisible format characters (Unicode category Cf), which could hide a
/// protected prefix from a comparison while looking the same to a reader.
/// SQL `nanace_is_protected_title` lists the same ranges.
const FORMAT_CHARS: [(u32, u32); 21] = [
    (0x00AD, 0x00AD),
    (0x0600, 0x0605),
    (0x061C, 0x061C),
    (0x06DD, 0x06DD),
    (0x070F, 0x070F),
    (0x0890, 0x0891),
    (0x08E2, 0x08E2),
    (0x180E, 0x180E),
    (0x200B, 0x200F),
    (0x202A, 0x202E),
    (0x2060, 0x2064),
    (0x2066, 0x206F),
    (0xFEFF, 0xFEFF),
    (0xFFF9, 0xFFFB),
    (0x110BD, 0x110BD),
    (0x110CD, 0x110CD),
    (0x13430, 0x1343F),
    (0x1BCA0, 0x1BCA3),
    (0x1D173, 0x1D17A),
    (0xE0001, 0xE0001),
    (0xE0020, 0xE007F),
];

/// White space as SQL `nanace_is_protected_title` lists it (Unicode
/// White_Space).
fn is_title_space(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{D}'
            | ' '
            | '\u{85}'
            | '\u{A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
    )
}

/// True when `title` names a protected page: after NFKC normalization and
/// with invisible format characters removed, it starts with `wiki` or
/// `policy` in any ASCII case, optional white space and a colon. Only ASCII
/// case is folded, so the result does not depend on a locale and matches the
/// SQL function of the same name exactly.
pub fn is_protected_title(title: &str) -> bool {
    use unicode_normalization::UnicodeNormalization;

    let normalized: String = title
        .nfkc()
        .filter(|c| !FORMAT_CHARS.iter().any(|(lo, hi)| (*lo..=*hi).contains(&u32::from(*c))))
        .collect();
    let rest = normalized.trim_start_matches(is_title_space);
    PROTECTED_TITLE_PREFIXES.iter().any(|prefix| {
        rest.get(..prefix.len())
            .filter(|head| head.eq_ignore_ascii_case(prefix))
            .is_some_and(|_| rest[prefix.len()..].trim_start_matches(is_title_space).starts_with(':'))
    })
}

/// Letters and colons of other scripts that read as the ASCII ones in a
/// protected prefix.
fn ascii_look_alike(c: char) -> char {
    match c {
        'ԝ' | 'Ԝ' | 'ѡ' | 'Ѡ' | 'ω' => 'w',
        'і' | 'І' | 'ı' | 'ι' | 'Ι' => 'i',
        'к' | 'К' | 'κ' | 'Κ' => 'k',
        'р' | 'Р' | 'ρ' | 'Ρ' => 'p',
        'о' | 'О' | 'ο' | 'Ο' | '0' => 'o',
        'ӏ' | 'Ӏ' => 'l',
        'с' | 'С' | 'ϲ' | 'Ϲ' => 'c',
        'у' | 'У' | 'γ' | 'Υ' => 'y',
        '∶' | '꞉' | 'ː' | '։' | '׃' => ':',
        other => other,
    }
}

/// True when `title` is not protected but reads like a protected one to a
/// person (`Wіki:` with a Cyrillic letter). Handlers refuse such titles from
/// non-admins as a courtesy; the security boundary is [`is_protected_title`],
/// which the policy loader and the database use.
pub fn resembles_protected_title(title: &str) -> bool {
    !is_protected_title(title) && is_protected_title(&title.chars().map(ascii_look_alike).collect::<String>())
}

/// Titles and what [`is_protected_title`] and SQL `nanace_is_protected_title`
/// must both answer for them.
#[doc(hidden)]
pub const PROTECTED_TITLE_CASES: [(&str, bool); 34] = [
    ("Wiki: Schema", true),
    ("Wiki: Index", true),
    ("Policy: P2P", true),
    ("wiki: schema", true),
    ("WIKI:", true),
    ("pOlIcY:x", true),
    ("Wiki : Schema", true),
    ("Wiki\t:\u{A0}Schema", true),
    ("  Wiki: Schema", true),
    ("\u{3000}Policy: P2P", true),
    // Full-width and compatibility forms fold under NFKC.
    ("Ｗｉｋｉ： Schema", true),
    ("Wiki： Schema", true),
    ("Wi\u{212A}i: Schema", true),
    ("ℙolicy: P2P", true),
    ("Wiki︓ Schema", true),
    // Invisible format characters are ignored.
    ("\u{200B}Wiki: Schema", true),
    ("W\u{200D}iki: Schema", true),
    ("Wiki\u{FEFF}: Schema", true),
    ("\u{202E}Policy\u{2060}: P2P", true),
    ("Wi\u{AD}ki: Schema", true),
    ("Wiki\u{E0041}: Schema", true),
    // Not protected.
    ("Wiki", false),
    ("Wikipedia: notes", false),
    ("Policies: travel", false),
    ("Topic: Wiki: Schema", false),
    ("My Wiki: Schema", false),
    ("Wiki- Schema", false),
    ("W iki: Schema", false),
    ("", false),
    (":", false),
    // Look-alikes of other scripts are different titles.
    ("Wіki: Schema", false),
    ("Роlісу: P2P", false),
    ("Wiki∶ Schema", false),
    // Turkish dotted capital I does not fold to ASCII `i`.
    ("Wİki: Schema", false),
];

/// Content may be empty: a new page starts blank and is still findable by
/// its title.
pub fn content(value: &str) -> Result<(), ValidationError> {
    if value.len() > MAX_CONTENT_BYTES {
        return Err(invalid("content", "exceeds 256 KiB"));
    }
    Ok(())
}

/// Returns normalized tags: trimmed, lowercased, deduplicated, sorted.
pub fn tags(values: &[String]) -> Result<Vec<String>, ValidationError> {
    if values.len() > MAX_TAGS {
        return Err(invalid("tags", "too many tags"));
    }
    let mut out = Vec::with_capacity(values.len());
    for raw in values {
        let tag = raw.trim().to_lowercase();
        if tag.is_empty() || tag.chars().count() > MAX_TAG_CHARS {
            return Err(invalid("tags", "each tag must be 1-50 chars"));
        }
        if !tag
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.'))
        {
            return Err(invalid(
                "tags",
                "tags may contain letters, digits, '-', '_', '.', and spaces",
            ));
        }
        out.push(tag);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_titles_are_recognized() {
        for (title, expected) in PROTECTED_TITLE_CASES {
            assert_eq!(is_protected_title(title), expected, "{title:?}");
        }
    }

    #[test]
    fn look_alike_titles_are_flagged_but_not_protected() {
        for title in [
            "Wіki: Schema",
            "Роlісу: P2P",
            "Wiki∶ Schema",
            "Poӏicy: x",
            "P0licy: x",
            "Wıki: Index",
        ] {
            assert!(!is_protected_title(title), "{title}");
            assert!(resembles_protected_title(title), "{title}");
        }
        for title in [
            "Wiki: Schema",
            "Topic: Wiki",
            "Wikipedia: notes",
            "PROJECT-1: task",
            "Рабочая: заметка",
        ] {
            assert!(!resembles_protected_title(title), "{title}");
        }
    }

    #[test]
    fn slug_rules() {
        assert!(slug("slug", "example").is_ok());
        assert!(slug("slug", "dev-team-2").is_ok());
        assert!(slug("slug", "a").is_err());
        assert!(slug("slug", "-example").is_err());
        assert!(slug("slug", "example-").is_err());
        assert!(slug("slug", "Example").is_err());
        assert!(slug("slug", "ac_me").is_err());
        assert!(slug("slug", &"a".repeat(63)).is_ok());
        assert!(slug("slug", &"a".repeat(64)).is_err());
    }

    #[test]
    fn email_is_normalized_and_checked() {
        assert_eq!(email("  Alice@Example.COM ").unwrap(), "alice@example.com");
        for bad in [
            "",
            "alice",
            "@example.com",
            "alice@example",
            "alice@.com",
            "a@b@c.com",
            "a b@c.com",
        ] {
            assert!(email(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn title_and_content_bounds() {
        assert_eq!(title("  API docs ").unwrap(), "API docs");
        assert!(title("   ").is_err());
        assert!(title(&"x".repeat(MAX_TITLE_CHARS)).is_ok());
        assert!(title(&"x".repeat(MAX_TITLE_CHARS + 1)).is_err());
        assert!(title("a\nb").is_err());
        assert!(content("").is_ok());
        assert!(content(&"x".repeat(MAX_CONTENT_BYTES)).is_ok());
        assert!(content(&"x".repeat(MAX_CONTENT_BYTES + 1)).is_err());
    }

    #[test]
    fn tags_are_normalized() {
        let input = vec!["API".to_owned(), " auth ".to_owned(), "api".to_owned()];
        assert_eq!(tags(&input).unwrap(), vec!["api", "auth"]);
        assert!(tags(&["".to_owned()]).is_err());
        assert!(tags(&["a/b".to_owned()]).is_err());
        assert!(tags(&vec!["t".to_owned(); MAX_TAGS + 1]).is_err());
    }
}
