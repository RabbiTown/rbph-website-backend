use actix_session::SessionExt;
use actix_web::{Error, FromRequest, HttpRequest, dev::Payload};
use futures_util::future::{Ready, ready};

use crate::error::RbError;

pub struct AuthUser {
    pub uid: i32,
}

impl FromRequest for AuthUser {
    type Error = Error;
    type Future = Ready<Result<Self, Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        let sess = req.get_session();

        match sess.get::<i32>("user_id") {
            Ok(Some(uid)) => ready(Ok(AuthUser { uid })),
            Ok(None) | Err(_) => {
                let _ = sess.purge();
                ready(Err(RbError::unauth().into()))
            }
        }
    }
}
