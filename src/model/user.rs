use num_enum::{FromPrimitive, IntoPrimitive};
use sqlx::types::time::OffsetDateTime;

pub struct RbUser {
    pub id: i32,
    pub email: String,
    pub upass: String,
    pub urole: RbUserRole,
    pub nickname: String,
    pub bio: Option<String>,
    pub ctime_at: OffsetDateTime,
}

#[derive(FromPrimitive, IntoPrimitive, Clone, Copy, Eq, PartialEq)]
#[repr(i16)]
pub enum RbUserRole {
    Banned = 0,
    User = 1,
    Moderator = 2,
    Admin = 3,

    #[num_enum(catch_all)]
    Invalid(i16),
}

impl RbUserRole {
    fn is_moderator(&self) -> bool {
        matches!(self, Self::Moderator | Self::Admin)
    }

    fn is_admin(&self) -> bool {
        matches!(self, Self::Admin)
    }

    fn is_active(&self) -> bool {
        !matches!(self, Self::Banned)
    }

    fn is_valid(&self) -> bool {
        !matches!(self, Self::Invalid(_))
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
