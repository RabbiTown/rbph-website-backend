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

#[derive(FromPrimitive, IntoPrimitive)]
#[repr(i16)]
pub enum RbUserRole {
    Banned = 0,
    User = 1,
    Moderator = 2,
    Admin = 3,

    #[num_enum(catch_all)]
    Invalid(i16),
}
