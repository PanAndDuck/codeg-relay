use http::{HeaderMap, HeaderName};

pub fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let named: Vec<HeaderName> = headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in named {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

pub fn websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get("upgrade")
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
        && headers
            .get_all("connection")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|v| v.trim().eq_ignore_ascii_case("upgrade"))
}

pub fn set_websocket_upgrade(headers: &mut HeaderMap) {
    headers.insert("connection", http::HeaderValue::from_static("upgrade"));
    headers.insert("upgrade", http::HeaderValue::from_static("websocket"));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removes_all_connection_nominated_and_standard_hop_headers() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("connection", "keep-alive, X-Remove"),
            ("keep-alive", "timeout=5"),
            ("x-remove", "value"),
            ("te", "trailers"),
            ("trailer", "x-tail"),
            ("transfer-encoding", "chunked"),
            ("upgrade", "websocket"),
            ("proxy-authorization", "private"),
            ("proxy-authenticate", "private"),
            ("proxy-connection", "keep-alive"),
            ("content-type", "application/json"),
            ("authorization", "Bearer example"),
        ] {
            headers.insert(name, value.parse().unwrap());
        }
        headers.append("connection", "X-Also-Remove".parse().unwrap());
        headers.insert("x-also-remove", "value".parse().unwrap());
        strip_hop_by_hop(&mut headers);
        assert_eq!(headers.len(), 2);
        assert!(headers.contains_key("authorization"));
        assert!(headers.contains_key("content-type"));
    }
}
