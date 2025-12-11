use actix_session::SessionExt;
use actix_web::{Error, FromRequest, HttpMessage, HttpRequest, dev::Payload};
use futures_util::future::{Ready, ready};

use crate::{db::puzzle::PuzzleUserInfo, error::RbError, model::user::RbUserRole};

pub struct AuthUser {
    pub uid: i32,
    pub role: RbUserRole,
    pub puzzle: Option<PuzzleUserInfo>,
}

impl FromRequest for AuthUser {
    type Error = Error;
    type Future = Ready<Result<Self, Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        let sess = req.get_session();

        match sess.get::<i32>("user_id") {
            Ok(Some(uid)) => ready(Ok(AuthUser {
                uid,
                role: *req
                    .extensions()
                    .get::<RbUserRole>()
                    .unwrap_or(&RbUserRole::Banned),
                puzzle: req.extensions().get::<PuzzleUserInfo>().cloned(),
            })),
            Ok(None) | Err(_) => {
                sess.purge();
                ready(Err(RbError::unauth().into()))
            }
        }
    }
}
