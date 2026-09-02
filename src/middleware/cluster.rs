use std::{
    future::{Ready, ready},
    rc::Rc,
};

use actix_web::{
    Error, ResponseError,
    body::EitherBody,
    dev::{Service, ServiceRequest, ServiceResponse, Transform, forward_ready},
    web,
};
use futures_util::future::LocalBoxFuture;

use crate::{AppState, error::RbError};

pub struct ClusterReadinessMiddleware;

impl<S: 'static, B> Transform<S, ServiceRequest> for ClusterReadinessMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = InnerClusterReadinessMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(InnerClusterReadinessMiddleware {
            service: Rc::new(service),
        }))
    }
}

pub struct InnerClusterReadinessMiddleware<S> {
    service: Rc<S>,
}

impl<S, B> Service<ServiceRequest> for InnerClusterReadinessMiddleware<S>
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
        let service = self.service.clone();
        Box::pin(async move {
            if matches!(req.path(), "/health/live" | "/health/ready") {
                return Ok(service.call(req).await?.map_into_left_body());
            }

            let app = req.app_data::<web::Data<AppState>>().unwrap();
            if app.cluster_membership.is_ready() {
                Ok(service.call(req).await?.map_into_left_body())
            } else {
                Ok(req.into_response(
                    RbError::service_unavailable()
                        .error_response()
                        .map_into_right_body(),
                ))
            }
        })
    }
}
