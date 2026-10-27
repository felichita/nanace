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
