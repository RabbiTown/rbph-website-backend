use actix_session::SessionExt;
use actix_web::{Error, FromRequest, HttpMessage, HttpRequest, dev::Payload};
use futures_util::future::{Ready, ready};

use crate::{
    db::game::GameUserInfo,
    error::{RbError, RbInternalError},
    model::user::RbUserRole,
};

pub struct AuthUser {
    pub uid: i32,
    pub role: Option<RbUserRole>,
    pub game: Option<GameUserInfo>,
}

impl AuthUser {
    /// Assumes role is injected by middleware, otherwise raise an error.
    pub fn req_role(&self) -> Result<RbUserRole, RbInternalError> {
        self.role.ok_or("Missing user role".into())
    }

    /// Assumes game is injected by middleware, otherwise raise an error.
    pub fn req_team_id(&self) -> Result<Option<i32>, RbInternalError> {
        self.game
            .as_ref()
            .map(|g| g.team_id)
            .ok_or("Missing game info".into())
    }
}

impl FromRequest for AuthUser {
    type Error = Error;
    type Future = Ready<Result<Self, Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        let sess = req.get_session();

        match sess.get::<i32>("user_id") {
            Ok(Some(uid)) => ready(Ok(AuthUser {
                uid,
                role: req.extensions().get::<RbUserRole>().cloned(),
                game: req.extensions().get::<GameUserInfo>().cloned(),
            })),
            Ok(None) | Err(_) => {
                sess.purge();
                ready(Err(RbError::unauth().into()))
            }
        }
    }
}
