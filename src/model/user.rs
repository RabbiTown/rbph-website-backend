use md5::{Digest, Md5};
use num_enum::{FromPrimitive, IntoPrimitive};
use serde::{Deserialize, Serialize};

pub fn avatar_url(email: &str) -> String {
    let normalized = email.trim().to_lowercase();
    let hash = Md5::digest(normalized.as_bytes());
    format!("https://cn.cravatar.com/avatar/{hash:x}.png?d=identicon")
}

#[derive(Serialize, Deserialize, FromPrimitive, IntoPrimitive, Clone, Copy, Eq, PartialEq)]
#[repr(i16)]
#[serde(into = "i16")]
pub enum RbUserRole {
    Banned = 0,
    User = 1,
    Moderator = 2,
    Admin = 3,

    #[num_enum(catch_all)]
    Invalid(i16),
}

impl RbUserRole {
    pub fn is_valid(&self) -> bool {
        !matches!(self, Self::Invalid(_))
    }

    pub fn is_moderator(&self) -> bool {
        matches!(self, Self::Moderator | Self::Admin)
    }

    pub fn is_admin(&self) -> bool {
        matches!(self, Self::Admin)
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
