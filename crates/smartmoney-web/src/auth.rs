//! Simple token protection.
//!
//! If a dashboard token is configured, a request is allowed when it carries
//! the `sm_token` cookie or a `?token=` query parameter (which sets the cookie
//! and redirects to the clean URL). Without a token the server only binds to
//! loopback addresses (enforced in `serve`).

use crate::AppState;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};

pub const COOKIE: &str = "sm_token";

/// Constant-time comparison to avoid leaking the token through timing.
fn eq_ct(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn cookie_token(req: &Request) -> Option<String> {
    req.headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_owned())
}

fn query_token(req: &Request) -> Option<String> {
    req.uri()
        .query()?
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == "token")
        .map(|(_, v)| v.to_owned())
}

/// Path and query without the `token` parameter.
fn clean_target(req: &Request) -> String {
    let path = req.uri().path();
    let rest: Vec<&str> = req
        .uri()
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|kv| !kv.is_empty() && !kv.starts_with("token="))
        .collect();
    if rest.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", rest.join("&"))
    }
}

pub async fn require_token(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(expected) = state.token.as_deref() else {
        return next.run(req).await;
    };
    if req.uri().path() == "/healthz" {
        return next.run(req).await;
    }
    if cookie_token(&req).is_some_and(|t| eq_ct(t.as_bytes(), expected.as_bytes())) {
        return next.run(req).await;
    }
    if let Some(t) = query_token(&req) {
        if eq_ct(t.as_bytes(), expected.as_bytes()) {
            let cookie =
                format!("{COOKIE}={expected}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000");
            let mut resp = Redirect::to(&clean_target(&req)).into_response();
            if let Ok(v) = cookie.parse() {
                resp.headers_mut().insert(header::SET_COOKIE, v);
            }
            return resp;
        }
    }
    (
        StatusCode::UNAUTHORIZED,
        axum::response::Html(crate::views::login_page().into_string()),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq() {
        assert!(eq_ct(b"abc", b"abc"));
        assert!(!eq_ct(b"abc", b"abd"));
        assert!(!eq_ct(b"abc", b"abcd"));
    }

    #[test]
    fn strips_token_from_target() {
        let req = Request::builder()
            .uri("/wallet/0xa?token=s3cret&min=40")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(query_token(&req).as_deref(), Some("s3cret"));
        assert_eq!(clean_target(&req), "/wallet/0xa?min=40");
    }

    #[test]
    fn reads_cookie() {
        let req = Request::builder()
            .uri("/")
            .header(header::COOKIE, "a=1; sm_token=xyz; b=2")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(cookie_token(&req).as_deref(), Some("xyz"));
    }
}
