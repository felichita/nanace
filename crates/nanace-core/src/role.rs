use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("unknown role `{0}`")]
pub struct UnknownRole(String);

/// Per-team role. Ordered so that `role >= TeamRole::Writer` reads naturally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    Reader,
    Writer,
    Admin,
}

impl TeamRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Writer => "writer",
            Self::Admin => "admin",
        }
    }

    pub fn can_write(self) -> bool {
        self >= Self::Writer
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
        assert!(TeamRole::Writer.can_write());
        assert!(TeamRole::Admin.can_write());
        assert!(!TeamRole::Writer.can_manage_members());
        assert!(TeamRole::Admin.can_manage_members());
    }

    #[test]
    fn roles_round_trip_and_reject_unknown() {
        for role in [TeamRole::Reader, TeamRole::Writer, TeamRole::Admin] {
            assert_eq!(role.as_str().parse::<TeamRole>().unwrap(), role);
        }
        assert!("Admin".parse::<TeamRole>().is_err());
        assert!("owner".parse::<OrgRole>().is_err());
        assert_eq!("admin".parse::<OrgRole>().unwrap(), OrgRole::Admin);
    }
}
