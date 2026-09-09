use actix_cors::Cors;
use actix_web::http::header;

use crate::config::CorsConfig;

/// Configuration is validated before the HTTP server starts.
pub fn build(config: &CorsConfig) -> Cors {
    let mut cors = Cors::default()
        .allowed_methods(["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"])
        .allowed_headers([header::ACCEPT, header::CONTENT_TYPE])
        .expose_headers([header::RETRY_AFTER])
        .supports_credentials()
        .block_on_origin_mismatch(true)
        .max_age(600);
    for origin in config.normalized_origins().expect("validated CORS origins") {
        cors = cors.allowed_origin(&origin);
    }
    cors
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{App, HttpResponse, middleware::Condition, test, web};

    fn config() -> CorsConfig {
        CorsConfig {
            allowed_origins: vec!["https://beta.example.com".into()],
        }
    }

    #[actix_web::test]
    async fn preflight_validates_origin_method_and_headers_without_entering_handler() {
        // No AppState is installed: entering readiness checks would fail.
        let app = test::init_service(
            App::new()
                .wrap(crate::middleware::cluster::ClusterReadinessMiddleware)
                .wrap(build(&config()))
                .default_service(web::to(|| async {
                    HttpResponse::InternalServerError().finish()
                })),
        )
        .await;
        for (origin, method, headers, expected) in [
            ("https://beta.example.com", "POST", "content-type", 200),
            ("https://other.example.com", "POST", "content-type", 400),
            ("https://beta.example.com", "TRACE", "content-type", 400),
            ("https://beta.example.com", "POST", "x-unapproved", 400),
        ] {
            let request = test::TestRequest::default()
                .method(actix_web::http::Method::OPTIONS)
                .insert_header((header::ORIGIN, origin))
                .insert_header((header::ACCESS_CONTROL_REQUEST_METHOD, method))
                .insert_header((header::ACCESS_CONTROL_REQUEST_HEADERS, headers))
                .to_request();
            let response = test::call_service(&app, request).await;
            assert_eq!(response.status().as_u16(), expected);
            if expected == 200 {
                assert_eq!(
                    response
                        .headers()
                        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                        .unwrap(),
                    origin
                );
                assert_eq!(
                    response
                        .headers()
                        .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
                        .unwrap(),
                    "true"
                );
                assert_eq!(
                    response
                        .headers()
                        .get(header::ACCESS_CONTROL_MAX_AGE)
                        .unwrap(),
                    "600"
                );
            }
        }
    }

    #[actix_web::test]
    async fn error_responses_have_cors_headers() {
        for status in [400, 401, 429, 503] {
            let app = test::init_service(App::new().wrap(build(&config())).default_service(
                web::to(move || async move {
                    HttpResponse::build(actix_web::http::StatusCode::from_u16(status).unwrap())
                        .insert_header((header::RETRY_AFTER, "60"))
                        .finish()
                }),
            ))
            .await;
            let request = test::TestRequest::get()
                .insert_header((header::ORIGIN, "https://beta.example.com"))
                .to_request();
            let response = test::call_service(&app, request).await;
            assert_eq!(response.status().as_u16(), status);
            assert_eq!(
                response
                    .headers()
                    .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                    .unwrap(),
                "https://beta.example.com"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
                    .unwrap(),
                "true"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::ACCESS_CONTROL_EXPOSE_HEADERS)
                    .unwrap(),
                "retry-after"
            );
            assert!(
                response
                    .headers()
                    .get(header::VARY)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("Origin")
            );
            let rejected = test::call_service(
                &app,
                test::TestRequest::get()
                    .insert_header((header::ORIGIN, "https://other.example.com"))
                    .to_request(),
            )
            .await;
            assert_eq!(rejected.status(), 400);
        }
    }

    #[actix_web::test]
    async fn absent_origin_and_disabled_cors_preserve_existing_behavior() {
        for enabled in [true, false] {
            let app = test::init_service(
                App::new()
                    .wrap(Condition::new(enabled, build(&config())))
                    .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
            )
            .await;
            let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
            assert_eq!(response.status(), 200);
            assert!(
                !response
                    .headers()
                    .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            );
            if !enabled {
                let response = test::call_service(
                    &app,
                    test::TestRequest::get()
                        .insert_header((header::ORIGIN, "https://other.example.com"))
                        .to_request(),
                )
                .await;
                assert_eq!(response.status(), 200);
                assert!(
                    !response
                        .headers()
                        .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                );
            }
        }
    }
    #[actix_web::test]
    async fn websocket_handshake_checks_origin() {
        let app = test::init_service(App::new().wrap(build(&config())).route(
            "/api/sync",
            web::get().to(
                |req: actix_web::HttpRequest, payload: web::Payload| async move {
                    let (response, _session, _stream) = actix_ws::handle(&req, payload)?;
                    Ok::<_, actix_web::Error>(response)
                },
            ),
        ))
        .await;
        for (origin, status) in [
            ("https://beta.example.com", 101),
            ("https://other.example.com", 400),
        ] {
            let response = test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/api/sync")
                    .insert_header((header::ORIGIN, origin))
                    .insert_header((header::CONNECTION, "upgrade"))
                    .insert_header((header::UPGRADE, "websocket"))
                    .insert_header((header::SEC_WEBSOCKET_VERSION, "13"))
                    .insert_header((header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ=="))
                    .to_request(),
            )
            .await;
            assert_eq!(response.status().as_u16(), status);
        }
    }
}
