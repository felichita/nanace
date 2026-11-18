use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("unknown role `{0}`")]
pub struct UnknownRole(String);

/// Per-team role. Ordered so that `role >= TeamRole::Writer` reads naturally.
/// `Agent` is for service accounts: it creates pages and edits its own, so it
/// sits below `Writer` and every `>= Writer` check keeps excluding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    Reader,
    Agent,
    Writer,
    Admin,
}

impl TeamRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Agent => "agent",
            Self::Writer => "writer",
            Self::Admin => "admin",
        }
    }

    /// May edit any ordinary page of the team, and trash and restore pages.
    pub fn can_write(self) -> bool {
        self >= Self::Writer
    }

    /// May create pages, and edit at least the ones it created.
    pub fn can_create(self) -> bool {
        self >= Self::Agent
    }

    pub fn can_manage_members(self) -> bool {
        self == Self::Admin
    }
}

impl FromStr for TeamRole {
    type Err = UnknownRole;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "reader" => Ok(Self::Reader),
            "agent" => Ok(Self::Agent),
            "writer" => Ok(Self::Writer),
            "admin" => Ok(Self::Admin),
            other => Err(UnknownRole(other.to_owned())),
        }
    }
}

impl fmt::Display for TeamRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Organization-level role. Org admins manage teams and memberships; they do
/// not get implicit access to team content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgRole {
    Member,
    Admin,
}

impl OrgRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Admin => "admin",
        }
    }
}

impl FromStr for OrgRole {
    type Err = UnknownRole;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "member" => Ok(Self::Member),
            "admin" => Ok(Self::Admin),
            other => Err(UnknownRole(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_role_permissions() {
        assert!(!TeamRole::Reader.can_write());
        assert!(!TeamRole::Agent.can_write());
        assert!(TeamRole::Agent.can_create() && !TeamRole::Reader.can_create());
        assert!(TeamRole::Agent < TeamRole::Writer && TeamRole::Agent > TeamRole::Reader);
        assert!(!TeamRole::Agent.can_manage_members());
        assert!(TeamRole::Writer.can_write());
        assert!(TeamRole::Admin.can_write());
        assert!(!TeamRole::Writer.can_manage_members());
        assert!(TeamRole::Admin.can_manage_members());
    }

    #[test]
    fn roles_round_trip_and_reject_unknown() {
        for role in [TeamRole::Reader, TeamRole::Agent, TeamRole::Writer, TeamRole::Admin] {
            assert_eq!(role.as_str().parse::<TeamRole>().unwrap(), role);
        }
        assert!("Admin".parse::<TeamRole>().is_err());
        assert!("owner".parse::<OrgRole>().is_err());
        assert_eq!("admin".parse::<OrgRole>().unwrap(), OrgRole::Admin);
    }
}
