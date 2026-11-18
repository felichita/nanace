//! The team P2P policy: a `Policy: P2P` page whose first
//! ```` ```yaml p2p-policy ```` block says whether agents of the team may
//! message each other and within which limits. The server parses and checks
//! the block when the page is saved; daemons receive the parsed form.

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::TeamRole;
use crate::p2p::{AcceptMode, MessageKind};
use crate::validate::{self, is_protected_title};

/// Largest policy block the server parses.
pub const MAX_POLICY_BLOCK_BYTES: usize = 16 * 1024;
pub const MAX_PATTERNS: usize = 32;
pub const MAX_PATTERN_BYTES: usize = 256;
pub const MAX_CALL_RULES: usize = 32;
pub const MAX_SERVER_PEERS: usize = 100;
/// Hard ceiling for a message body, whatever a policy says.
pub const MAX_MESSAGE_BYTES: u32 = 64 * 1024;

/// A page a team owner can start from: strict defaults, everything explained.
pub const TEMPLATE: &str = include_str!("policy_template.md");

const FENCE_INFO: &str = "yaml p2p-policy";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct P2pPolicy {
    pub version: u32,
    pub enabled: bool,
    pub max_accept_mode: AcceptMode,
    /// Members with the desktop app confirm every answer of their agent
    /// before it is sent, whatever their own setting says. Peers without the
    /// app (`server_peers`) have nobody to ask and are not affected.
    #[serde(default)]
    pub require_answer_review: bool,
    #[serde(default)]
    pub server_peers: ServerPeers,
    #[serde(default)]
    pub discovery: Discovery,
    /// Default deny: a call needs a rule that allows it.
    #[serde(default)]
    pub calls: Vec<CallRule>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub data: Data,
    #[serde(default)]
    pub retention: Retention,
    #[serde(default)]
    pub summaries: Summaries,
}

/// Accounts allowed to announce a peer that runs without the desktop app.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerPeers {
    #[serde(default)]
    pub users: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discovery {
    #[serde(default = "all_roles")]
    pub visible_to: Vec<TeamRole>,
    #[serde(default = "yes")]
    pub lan_addrs: bool,
}

/// Senders with one of the `from` roles may send `kinds` to receivers with
/// one of the `to` roles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallRule {
    pub from: Vec<TeamRole>,
    pub to: Vec<TeamRole>,
    pub kinds: Vec<MessageKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default = "default_message_bytes")]
    pub max_message_bytes: u32,
    #[serde(default = "default_hops")]
    pub max_hops: u32,
    #[serde(default = "default_request_ttl")]
    pub request_ttl_secs: u32,
    #[serde(default = "default_consent_timeout")]
    pub consent_timeout_secs: u32,
    #[serde(default = "default_rate")]
    pub rate_per_peer_per_min: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    /// Rust regular expressions that must not match a message body. Added to
    /// the built-in secret rules, never replacing them.
    #[serde(default)]
    pub deny_outbound_patterns: Vec<String>,
    /// Reserved: only `same` is accepted, and nothing enforces it. A daemon
    /// cannot tell which team a referenced page belongs to; the API decides
    /// who may read it.
    #[serde(default = "same_team")]
    pub allow_refs_to_teams: Vec<RefScope>,
}

/// Which teams' pages a message may reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefScope {
    /// Only pages of the team the message is sent in.
    Same,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retention {
    #[serde(default = "default_retention_days")]
    pub exchange_metadata_days: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summaries {
    #[serde(default)]
    pub required_for_kinds: Vec<MessageKind>,
    #[serde(default = "default_title_prefix")]
    pub title_prefix: String,
}

fn yes() -> bool {
    true
}
fn all_roles() -> Vec<TeamRole> {
    vec![TeamRole::Reader, TeamRole::Agent, TeamRole::Writer, TeamRole::Admin]
}
fn same_team() -> Vec<RefScope> {
    vec![RefScope::Same]
}
fn default_message_bytes() -> u32 {
    32 * 1024
}
fn default_hops() -> u32 {
    2
}
fn default_request_ttl() -> u32 {
    3600
}
fn default_consent_timeout() -> u32 {
    600
}
fn default_rate() -> u32 {
    30
}
fn default_retention_days() -> u32 {
    30
}
fn default_title_prefix() -> String {
    "P2P: ".to_owned()
}

impl Default for Discovery {
    fn default() -> Self {
        Self {
            visible_to: all_roles(),
            lan_addrs: true,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message_bytes: default_message_bytes(),
            max_hops: default_hops(),
            request_ttl_secs: default_request_ttl(),
            consent_timeout_secs: default_consent_timeout(),
            rate_per_peer_per_min: default_rate(),
        }
    }
}

impl Default for Data {
    fn default() -> Self {
        Self {
            deny_outbound_patterns: Vec::new(),
            allow_refs_to_teams: same_team(),
        }
    }
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            exchange_metadata_days: default_retention_days(),
        }
    }
}

impl Default for Summaries {
    fn default() -> Self {
        Self {
            required_for_kinds: Vec::new(),
            title_prefix: default_title_prefix(),
        }
    }
}

impl P2pPolicy {
    /// Everything wrong with the values, in words an owner can act on. Empty
    /// when the policy can be enforced as written.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut range = |name: &str, value: u32, min: u32, max: u32| {
            if !(min..=max).contains(&value) {
                problems.push(format!("{name} must be {min}-{max}, got {value}"));
            }
        };
        range("version", self.version, 1, 1);
        range(
            "limits.max_message_bytes",
            self.limits.max_message_bytes,
            1024,
            MAX_MESSAGE_BYTES,
        );
        range("limits.max_hops", self.limits.max_hops, 1, 4);
        range("limits.request_ttl_secs", self.limits.request_ttl_secs, 60, 86_400);
        range(
            "limits.consent_timeout_secs",
            self.limits.consent_timeout_secs,
            30,
            3600,
        );
        range(
            "limits.rate_per_peer_per_min",
            self.limits.rate_per_peer_per_min,
            1,
            600,
        );
        range(
            "retention.exchange_metadata_days",
            self.retention.exchange_metadata_days,
            1,
            365,
        );

        if self.calls.len() > MAX_CALL_RULES {
            problems.push(format!("calls: at most {MAX_CALL_RULES} rules"));
        }
        for (i, rule) in self.calls.iter().enumerate() {
            if rule.from.is_empty() || rule.to.is_empty() || rule.kinds.is_empty() {
                problems.push(format!(
                    "calls[{i}]: from, to and kinds must each list at least one value; remove the rule to forbid"
                ));
            }
        }
        if self.discovery.visible_to.is_empty() {
            problems.push("discovery.visible_to must list at least one role".to_owned());
        }
        if self.server_peers.users.len() > MAX_SERVER_PEERS {
            problems.push(format!("server_peers.users: at most {MAX_SERVER_PEERS} accounts"));
        }
        for user in &self.server_peers.users {
            if validate::email(user).as_deref() != Ok(user.as_str()) {
                problems.push(format!(
                    "server_peers.users: `{user}` is not a lower-case email address"
                ));
            }
        }
        if self.data.deny_outbound_patterns.len() > MAX_PATTERNS {
            problems.push(format!("data.deny_outbound_patterns: at most {MAX_PATTERNS} patterns"));
        }
        for (i, pattern) in self.data.deny_outbound_patterns.iter().enumerate() {
            if pattern.is_empty() || pattern.len() > MAX_PATTERN_BYTES {
                problems.push(format!(
                    "data.deny_outbound_patterns[{i}] must be 1-{MAX_PATTERN_BYTES} bytes"
                ));
            } else if Regex::new(pattern).is_err() {
                problems.push(format!(
                    "data.deny_outbound_patterns[{i}] is not a valid regular expression"
                ));
            } else if Regex::new(pattern).is_ok_and(|re| re.is_match("")) {
                problems.push(format!(
                    "data.deny_outbound_patterns[{i}] matches empty text and would block every message"
                ));
            }
        }
        let prefix = self.summaries.title_prefix.trim();
        if prefix.is_empty()
            || self.summaries.title_prefix.len() > 40
            || self.summaries.title_prefix.chars().any(char::is_control)
            || is_protected_title(&self.summaries.title_prefix)
        {
            problems
                .push("summaries.title_prefix must be 1-40 characters and not a Policy: or Wiki: prefix".to_owned());
        }
        problems
    }

    /// True when a sender with role `from` may send `kind` to a receiver
    /// with role `to`.
    pub fn allows_call(&self, from: TeamRole, to: TeamRole, kind: MessageKind) -> bool {
        self.enabled
            && self
                .calls
                .iter()
                .any(|rule| rule.from.contains(&from) && rule.to.contains(&to) && rule.kinds.contains(&kind))
    }

    /// True when a member with `role` may see who is present.
    pub fn visible_to(&self, role: TeamRole) -> bool {
        self.enabled && self.discovery.visible_to.contains(&role)
    }

    /// The accept mode a member ends up with: theirs, capped by the policy.
    pub fn effective_accept_mode(&self, wanted: AcceptMode) -> AcceptMode {
        wanted.min(self.max_accept_mode)
    }
}

/// True for the page that holds a team's P2P policy: a protected title whose
/// name after `Policy:` is `P2P`, in any ASCII case.
pub fn is_policy_page_title(title: &str) -> bool {
    use unicode_normalization::UnicodeNormalization;

    if !is_protected_title(title) {
        return false;
    }
    let normalized: String = title.nfkc().collect();
    normalized.split_once(':').is_some_and(|(prefix, name)| {
        prefix.trim().eq_ignore_ascii_case("policy") && name.trim().eq_ignore_ascii_case("p2p")
    })
}

/// The text of the first ```` ```yaml p2p-policy ```` block of a page, or
/// `None` when the page has none or the block is not closed.
pub fn policy_block(content: &str) -> Option<&str> {
    let (start, end) = block_span(content)?;
    Some(&content[start..end])
}

/// The page without its policy block: the guidance written for agents.
pub fn guidance_text(content: &str) -> String {
    let Some((start, end)) = block_span(content) else {
        return content.trim().to_owned();
    };
    // Drop the fence lines around the block as well.
    let before = content[..start].rsplit_once("```").map_or("", |(before, _)| before);
    let after = content[end..].split_once('\n').map_or("", |(_, after)| after);
    format!("{}\n{}", before.trim_end(), after.trim_start())
        .trim()
        .to_owned()
}

/// Byte range of the block's body (between the fence lines).
fn block_span(content: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    let mut start = None;
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim();
        match start {
            None => {
                if trimmed
                    .strip_prefix("```")
                    .is_some_and(|info| info.split_whitespace().collect::<Vec<_>>().join(" ") == FENCE_INFO)
                {
                    start = Some(offset + line.len());
                }
            }
            Some(start) => {
                if trimmed == "```" {
                    return Some((start, offset));
                }
            }
        }
        offset += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> P2pPolicy {
        P2pPolicy {
            version: 1,
            enabled: true,
            max_accept_mode: AcceptMode::Ask,
            require_answer_review: false,
            server_peers: ServerPeers::default(),
            discovery: Discovery::default(),
            calls: vec![CallRule {
                from: vec![TeamRole::Writer, TeamRole::Admin],
                to: vec![TeamRole::Agent, TeamRole::Writer, TeamRole::Admin],
                kinds: vec![MessageKind::Ask, MessageKind::Status],
            }],
            limits: Limits::default(),
            data: Data::default(),
            retention: Retention::default(),
            summaries: Summaries::default(),
        }
    }

    #[test]
    fn calls_are_denied_unless_a_rule_allows_them() {
        let p = policy();
        assert!(p.problems().is_empty(), "{:?}", p.problems());
        assert!(p.allows_call(TeamRole::Writer, TeamRole::Agent, MessageKind::Ask));
        assert!(
            !p.allows_call(TeamRole::Agent, TeamRole::Writer, MessageKind::Ask),
            "from"
        );
        assert!(
            !p.allows_call(TeamRole::Writer, TeamRole::Reader, MessageKind::Ask),
            "to"
        );
        assert!(
            !p.allows_call(TeamRole::Writer, TeamRole::Agent, MessageKind::Notify),
            "kind"
        );
        let disabled = P2pPolicy {
            enabled: false,
            ..policy()
        };
        assert!(!disabled.allows_call(TeamRole::Writer, TeamRole::Agent, MessageKind::Ask));
        assert!(!disabled.visible_to(TeamRole::Admin));
        let none = P2pPolicy {
            calls: vec![],
            ..policy()
        };
        assert!(!none.allows_call(TeamRole::Admin, TeamRole::Admin, MessageKind::Ask));
    }

    #[test]
    fn accept_mode_is_capped() {
        let p = policy();
        assert_eq!(p.effective_accept_mode(AcceptMode::Team), AcceptMode::Ask);
        assert_eq!(p.effective_accept_mode(AcceptMode::Off), AcceptMode::Off);
    }

    #[test]
    fn bad_values_are_reported() {
        let mut p = policy();
        p.version = 2;
        p.limits.max_hops = 9;
        p.limits.max_message_bytes = MAX_MESSAGE_BYTES + 1;
        p.retention.exchange_metadata_days = 0;
        p.calls.push(CallRule {
            from: vec![TeamRole::Admin],
            to: vec![],
            kinds: vec![],
        });
        p.discovery.visible_to.clear();
        p.server_peers.users = vec!["Alice@Example.com".to_owned(), "not-an-email".to_owned()];
        p.data.deny_outbound_patterns = vec!["(".to_owned(), "a*".to_owned(), "x".repeat(257), "ok[0-9]+".to_owned()];
        p.summaries.title_prefix = "Wiki: ".to_owned();
        let problems = p.problems();
        assert_eq!(problems.len(), 12, "{problems:#?}");
        for needle in [
            "version",
            "max_hops",
            "max_message_bytes",
            "exchange_metadata_days",
            "calls[1]",
            "visible_to",
            "Alice@Example.com",
            "not-an-email",
            "patterns[0]",
            "patterns[1]",
            "patterns[2]",
            "title_prefix",
        ] {
            assert!(problems.iter().any(|p| p.contains(needle)), "{needle}: {problems:#?}");
        }
    }

    #[test]
    fn policy_page_title_is_recognized() {
        for title in ["Policy: P2P", "policy:p2p", " Policy : P2p ", "Ｐolicy： P2P"] {
            assert!(is_policy_page_title(title), "{title}");
        }
        for title in ["Policy: P2P rules", "Policy: Travel", "Wiki: P2P", "P2P", "Роlісу: P2P"] {
            assert!(!is_policy_page_title(title), "{title}");
        }
    }

    #[test]
    fn block_is_found_and_text_is_what_remains() {
        let page = "> intro\n\n```yaml   p2p-policy\nversion: 1\nenabled: true\n```\n\n## For agents\n\n- be kind\n";
        assert_eq!(policy_block(page), Some("version: 1\nenabled: true\n"));
        assert_eq!(guidance_text(page), "> intro\n## For agents\n\n- be kind");
        // Only the first block counts; other fences are left alone.
        let two = format!("{page}\n```yaml p2p-policy\nversion: 9\n```\n");
        assert_eq!(policy_block(&two), Some("version: 1\nenabled: true\n"));
        for none in [
            "",
            "no block",
            "```yaml\nversion: 1\n```",
            "```yaml p2p-policy\nversion: 1\n",
        ] {
            assert_eq!(policy_block(none), None, "{none:?}");
        }
        assert_eq!(guidance_text("just text\n"), "just text");
    }

    #[test]
    fn template_has_a_block() {
        assert!(policy_block(TEMPLATE).is_some_and(|b| b.contains("max_accept_mode: ask")));
        assert!(guidance_text(TEMPLATE).contains("## For agents"));
    }
}
