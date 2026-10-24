use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use http::HeaderMap;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub fn token_matches(candidate: &str, token: &str) -> bool {
    let expected = Sha256::digest(token.as_bytes());
    let supplied = Sha256::digest(candidate.as_bytes());
    bool::from(expected.ct_eq(&supplied)) & !token.is_empty()
}

// Keep upstream's case, whitespace, first-header and first-prefixed-protocol
// semantics. A malformed WS credential must not fall through to a later one.
pub fn authorized(headers: &HeaderMap, token: &str) -> bool {
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let ws = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(',')
                .map(str::trim)
                .find_map(|p| p.strip_prefix("codeg-token."))
        })
        .and_then(|v| URL_SAFE_NO_PAD.decode(v).ok())
        .and_then(|v| String::from_utf8(v).ok());
    token_matches(bearer.unwrap_or_default(), token)
        | token_matches(ws.as_deref().unwrap_or_default(), token)
}

/// Public exceptions from upstream v0.39.0, bed89314, web/router.rs.
pub fn protected_path(path: &str) -> bool {
    if matches!(path, "/ws/events" | "/ws/browser-tunnel") {
        return true;
    }
    let Some(api) = path.strip_prefix("/api/") else {
        return path == "/api";
    };
    if matches!(
        api,
        "get_system_language_settings" | "cli_hello" | "forge_webhook"
    ) {
        return false;
    }
    for prefix in ["workspace_download/", "backup_download/"] {
        if let Some(ticket) = api.strip_prefix(prefix) {
            if !ticket.is_empty() && !ticket.contains('/') {
                return false;
            }
        }
    }
    if let Some(rest) = api.strip_prefix("office-watch-proxy/") {
        if !rest.split('/').next().unwrap_or_default().is_empty() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

    #[test]
    fn fixed_size_digest_comparison_rejects_length_and_boundary_changes() {
        assert!(token_matches("example-value", "example-value"));
        for candidate in [
            "",
            "example",
            "Example-value",
            "example-valuE",
            "example-value-longer",
            "例子",
        ] {
            assert!(!token_matches(candidate, "example-value"));
        }
        assert!(!token_matches("", ""));
    }

    #[test]
    fn bearer_is_exact_and_empty_configuration_fails_closed() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer example-value".parse().unwrap());
        assert!(authorized(&headers, "example-value"));
        for token in ["", "example", "example-value!", "EXAMPLE-value"] {
            assert!(!authorized(&headers, token));
        }
        for value in ["bearer example-value", "Bearer  example-value", "Bearer "] {
            headers.insert("authorization", value.parse().unwrap());
            assert!(!authorized(&headers, "example-value"));
            assert!(!authorized(&headers, ""));
        }
    }

    #[test]
    fn websocket_token_uses_first_prefixed_protocol_and_can_follow_bad_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer wrong".parse().unwrap());
        let encoded = URL_SAFE_NO_PAD.encode("example/value+token");
        headers.insert(
            "sec-websocket-protocol",
            format!("codeg-events, codeg-token.{encoded}")
                .parse()
                .unwrap(),
        );
        assert!(authorized(&headers, "example/value+token"));
        for value in [
            format!("codeg-token.@@@, codeg-token.{encoded}"),
            "codeg-token._w".into(),
            "codeg-token.YQ==".into(),
            "codeg-events".into(),
        ] {
            headers.insert("sec-websocket-protocol", value.parse().unwrap());
            assert!(!authorized(&headers, "example/value+token"));
        }
    }

    #[test]
    fn matches_v039_protected_routes_and_public_exceptions() {
        for path in [
            "/api",
            "/api/",
            "/api/health",
            "/api/unknown",
            "/api/cli_run_get",
            "/ws/events",
            "/ws/browser-tunnel",
            "/api/workspace_download",
            "/api/workspace_download/a/b",
        ] {
            assert!(protected_path(path), "{path}");
        }
        for path in [
            "/",
            "/_next/static/app.js",
            "/apiculture",
            "/ws/other",
            "/api/get_system_language_settings",
            "/api/cli_hello",
            "/api/forge_webhook",
            "/api/workspace_download/opaque-ticket",
            "/api/backup_download/opaque-ticket",
            "/api/office-watch-proxy/8888",
            "/api/office-watch-proxy/8888/",
            "/api/office-watch-proxy/8888/api/edit",
        ] {
            assert!(!protected_path(path), "{path}");
        }
    }
}
