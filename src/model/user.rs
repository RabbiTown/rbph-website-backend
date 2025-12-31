use num_enum::{FromPrimitive, IntoPrimitive};
use serde::{Deserialize, Serialize};

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
