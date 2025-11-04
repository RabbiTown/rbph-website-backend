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
use sqlx::PgPool;

use crate::{db, error::RbError, model::user::RbUserRole, module::session};

pub struct PrivilegeMiddleware {
    required: RbUserRole,
}

impl PrivilegeMiddleware {
    pub fn new(required: RbUserRole) -> Self {
        Self { required }
    }
}

impl<S: 'static, B> Transform<S, ServiceRequest> for PrivilegeMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = InnerPrivilegeMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(InnerPrivilegeMiddleware {
            service: Rc::new(service),
            required: self.required,
        }))
    }
}

pub struct InnerPrivilegeMiddleware<S> {
    service: Rc<S>,
    required: RbUserRole,
}

impl<S, B> Service<ServiceRequest> for InnerPrivilegeMiddleware<S>
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
        let required = self.required;

        let kv_pool = match req.app_data::<web::Data<deadpool_redis::Pool>>() {
            Some(data) => data.clone(),
            None => {
                return Box::pin(async {
                    Ok(req.into_response(
                        RbError::internal("redis pool not found")
                            .resp()
                            .map_into_right_body(),
                    ))
                });
            }
        };

        let db_pool = match req.app_data::<web::Data<PgPool>>() {
            Some(data) => data.clone(),
            None => {
                return Box::pin(async {
                    Ok(req.into_response(
                        RbError::internal("sql pool not found")
                            .resp()
                            .map_into_right_body(),
                    ))
                });
            }
        };

        let sess = req.get_session();

        Box::pin(async move {
            match session::verify(&kv_pool, &sess).await {
                Ok(true) => {}
                Ok(false) => {
                    sess.purge();
                    return Ok(req.into_response(RbError::unauth().resp().map_into_right_body()));
                }
                Err(e) => {
                    return Ok(req.into_response(e.error_response().map_into_right_body()));
                }
            };

            let user_id = sess.get::<i32>("user_id").ok().flatten();

            if let Some(uid) = user_id {
                match db::user::get_user_role_by_id(&db_pool, uid).await {
                    Ok(Some(role)) => {
                        if role >= required {
                            return Ok(srv.call(req).await?.map_into_left_body());
                        } else {
                            return Ok(
                                req.into_response(RbError::forbid().resp().map_into_right_body())
                            );
                        }
                    }
                    Err(e) => {
                        return Ok(req.into_response(e.error_response().map_into_right_body()));
                    }
                    _ => {}
                }
            }

            Ok(req.into_response(RbError::unauth().resp().map_into_right_body()))
        })
    }
}
