use std::{
    future::{Ready, ready},
    rc::Rc,
};

use actix_session::SessionExt;
use actix_web::{
    Error, ResponseError,
    body::EitherBody,
    dev::{Service, ServiceRequest, ServiceResponse, Transform, forward_ready},
    web,
};
use futures_util::future::LocalBoxFuture;

use crate::{AppState, db, error::RbError, model::user::RbUserRole, module::session};

pub struct MaintenanceMiddleware;

impl<S: 'static, B> Transform<S, ServiceRequest> for MaintenanceMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = InnerMaintenanceMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(InnerMaintenanceMiddleware {
            service: Rc::new(service),
        }))
    }
}

pub struct InnerMaintenanceMiddleware<S> {
    service: Rc<S>,
}

impl<S, B> Service<ServiceRequest> for InnerMaintenanceMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let srv = self.service.clone();

        Box::pin(async move {
            let app = req.app_data::<web::Data<AppState>>().unwrap();
            let settings = app.system_settings.read().await.clone();
            if !settings.maintenance_enabled || is_exempt(req.path()) {
                return Ok(srv.call(req).await?.map_into_left_body());
            }

            let sess = req.get_session();
            if session::verify(&app.kv, &sess).await.unwrap_or(false)
                && let Some(user_id) = sess.get::<i32>("user_id").ok().flatten()
                && let Some(auth) = db::user::get_auth_state_by_id(&app.db, user_id).await?
                && auth.role >= RbUserRole::Admin
            {
                return Ok(srv.call(req).await?.map_into_left_body());
            }

            Ok(req.into_response(
                RbError::maintenance(settings.maintenance_message)
                    .error_response()
                    .map_into_right_body(),
            ))
        })
    }
}

fn is_exempt(path: &str) -> bool {
    matches!(
        path,
        "/api/system/status" | "/api/auth/login" | "/api/auth/logout" | "/api/user/info"
    )
}

#[cfg(test)]
mod tests {
    use super::is_exempt;

    #[test]
    fn only_required_routes_bypass_maintenance() {
        assert!(is_exempt("/api/system/status"));
        assert!(is_exempt("/api/auth/login"));
        assert!(is_exempt("/api/auth/logout"));
        assert!(is_exempt("/api/user/info"));
        assert!(!is_exempt("/api/auth/register"));
        assert!(!is_exempt("/api/games/1"));
        assert!(!is_exempt("/api/admin/system-settings"));
    }
}
