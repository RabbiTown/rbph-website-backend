use md5::{Digest, Md5};
use num_enum::{FromPrimitive, IntoPrimitive, TryFromPrimitive};
use serde::{Deserialize, Serialize};

#[derive(
    Serialize,
    Deserialize,
    TryFromPrimitive,
    IntoPrimitive,
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
)]
#[repr(i16)]
#[serde(rename_all = "snake_case")]
pub enum AvatarProvider {
    #[default]
    Cravatar = 0,
    Catavatar = 1,
}

pub fn avatar_url(email: &str, provider: AvatarProvider) -> String {
    let normalized = email.trim().to_lowercase();
    let hash = Md5::digest(normalized.as_bytes());
    match provider {
        AvatarProvider::Cravatar => {
            format!("https://cn.cravatar.com/avatar/{hash:x}.png?d=identicon")
        }
        AvatarProvider::Catavatar => {
            format!("https://puzzle.cat/api/users/avatar/public/{hash:x}")
        }
    }
}

#[derive(Serialize, Deserialize, FromPrimitive, IntoPrimitive, Clone, Copy, Eq, PartialEq)]
#[repr(i16)]
#[serde(into = "i16")]
pub enum RbUserRole {
    Banned = 0,
    User = 1,
    Moderator = 2,
    Admin = 3,
    Root = 4,

    #[num_enum(catch_all)]
    Invalid(i16),
}

impl RbUserRole {
    pub fn is_valid(&self) -> bool {
        !matches!(self, Self::Invalid(_))
    }

    pub fn is_moderator(&self) -> bool {
        matches!(self, Self::Moderator | Self::Admin | Self::Root)
    }

    pub fn is_admin(&self) -> bool {
        matches!(self, Self::Admin | Self::Root)
    }

    pub fn is_root(&self) -> bool {
        matches!(self, Self::Root)
    }

    pub fn can_change_role(&self, current: Option<Self>, requested: Self) -> bool {
        if current == Some(requested) {
            return true;
        }
        if current == Some(Self::Root) || requested == Self::Root {
            return false;
        }
        self.is_root() || (current.is_none_or(|role| role < Self::Admin) && requested < Self::Admin)
    }

    pub fn can_manage_credentials(&self, target: Self) -> bool {
        target < *self
    }
}

impl PartialOrd for RbUserRole {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self.is_valid() && other.is_valid() {
            let lhs: i16 = (*self).into();
            let rhs: i16 = (*other).into();
            Some(lhs.cmp(&rhs))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AvatarProvider, RbUserRole, avatar_url};

    #[test]
    fn builds_avatar_urls_from_normalized_email() {
        assert_eq!(
            avatar_url(" Test@Example.com ", AvatarProvider::Cravatar),
            "https://cn.cravatar.com/avatar/55502f40dc8b7c769880b10874abc9d0.png?d=identicon"
        );
        assert_eq!(
            avatar_url(" Test@Example.com ", AvatarProvider::Catavatar),
            "https://puzzle.cat/api/users/avatar/public/55502f40dc8b7c769880b10874abc9d0"
        );
    }

    #[test]
    fn administrators_only_manage_non_admin_roles() {
        assert!(RbUserRole::Admin.can_change_role(None, RbUserRole::Moderator));
        assert!(RbUserRole::Admin.can_change_role(Some(RbUserRole::Moderator), RbUserRole::User,));
        assert!(!RbUserRole::Admin.can_change_role(None, RbUserRole::Admin));
        assert!(
            !RbUserRole::Admin.can_change_role(Some(RbUserRole::Admin), RbUserRole::Moderator,)
        );
        assert!(
            !RbUserRole::Admin.can_change_role(Some(RbUserRole::Moderator), RbUserRole::Admin,)
        );
        assert!(RbUserRole::Admin.can_manage_credentials(RbUserRole::Moderator));
        assert!(!RbUserRole::Admin.can_manage_credentials(RbUserRole::Admin));
        assert!(!RbUserRole::Admin.can_manage_credentials(RbUserRole::Root));
    }

    #[test]
    fn root_can_manage_admin_roles() {
        assert!(RbUserRole::Root.can_change_role(None, RbUserRole::Admin));
        assert!(RbUserRole::Root.can_change_role(Some(RbUserRole::Admin), RbUserRole::Moderator,));
        assert!(!RbUserRole::Root.can_change_role(None, RbUserRole::Root));
        assert!(!RbUserRole::Root.can_change_role(Some(RbUserRole::User), RbUserRole::Root,));
        assert!(!RbUserRole::Root.can_change_role(Some(RbUserRole::Root), RbUserRole::Admin,));
        assert!(RbUserRole::Root.can_manage_credentials(RbUserRole::Admin));
        assert!(!RbUserRole::Root.can_manage_credentials(RbUserRole::Root));
    }
}
