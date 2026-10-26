//! Minimal built-in sign-in landing page, the default `NANACE_FRONTEND_URL`.
//! Lets users sign in and create access tokens before a full frontend exists.

use actix_web::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
};
use actix_web::{HttpResponse, web};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/auth/complete", web::get().to(page))
        .route("/auth/complete.js", web::get().to(script))
        .route("/auth/complete.css", web::get().to(stylesheet));
}

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
                   form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

fn asset(content_type: &'static str, body: &'static str) -> HttpResponse {
    HttpResponse::Ok()
        .insert_header((CONTENT_TYPE, content_type))
        .insert_header((CONTENT_SECURITY_POLICY, CSP))
        // The page URL carries a one-time login code.
        .insert_header((REFERRER_POLICY, "no-referrer"))
        .insert_header((X_CONTENT_TYPE_OPTIONS, "nosniff"))
        .insert_header((CACHE_CONTROL, "no-store"))
        .body(body)
}

async fn page() -> HttpResponse {
    asset("text/html; charset=utf-8", include_str!("../../assets/complete.html"))
}

async fn script() -> HttpResponse {
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../../assets/complete.js"),
    )
}

async fn stylesheet() -> HttpResponse {
    asset("text/css; charset=utf-8", include_str!("../../assets/complete.css"))
}
