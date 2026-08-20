use std::fmt::Write as _;

pub const MAX_GPU_RESPONSE_BYTES: usize = 256;

const JSON_CONTENT_TYPE: &str = "application/json";
const TEXT_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestRoute {
    Root,
    Health,
    Hello,
    NotFound,
    BadRequest,
    MethodNotAllowed,
}

pub fn route_request(request: &[u8], backend: &'static str) -> Vec<u8> {
    match parse_request_route(request) {
        RequestRoute::Root => {
            let body = format!(
                "{{\"name\":\"gput\",\"backend\":\"{backend}\",\"message\":\"{}\"}}\n",
                match backend {
                    "gpu" => "GET dispatched through a compute shader",
                    _ => "GET handled by the CPU baseline",
                }
            );
            build_response(200, "OK", JSON_CONTENT_TYPE, body.as_bytes(), backend)
        }
        RequestRoute::Health => {
            build_response(200, "OK", TEXT_CONTENT_TYPE, b"ok\n", backend)
        }
        RequestRoute::Hello => {
            let body: &[u8] = if backend == "gpu" {
                b"hello from a compute shader\n"
            } else {
                b"hello from the CPU baseline\n"
            };
            build_response(200, "OK", TEXT_CONTENT_TYPE, body, backend)
        }
        RequestRoute::NotFound => build_response(
            404,
            "Not Found",
            TEXT_CONTENT_TYPE,
            b"not found\n",
            backend,
        ),
        RequestRoute::BadRequest => build_response(
            400,
            "Bad Request",
            TEXT_CONTENT_TYPE,
            b"bad request\n",
            backend,
        ),
        RequestRoute::MethodNotAllowed => build_response(
            405,
            "Method Not Allowed",
            TEXT_CONTENT_TYPE,
            b"method not allowed\n",
            backend,
        ),
    }
}

pub fn request_timeout_response() -> Vec<u8> {
    build_response(
        408,
        "Request Timeout",
        TEXT_CONTENT_TYPE,
        b"request timeout\n",
        "network",
    )
}

pub fn payload_too_large_response() -> Vec<u8> {
    build_response(
        413,
        "Content Too Large",
        TEXT_CONTENT_TYPE,
        b"request too large\n",
        "network",
    )
}

pub fn service_unavailable_response() -> Vec<u8> {
    build_response(
        503,
        "Service Unavailable",
        TEXT_CONTENT_TYPE,
        b"server overloaded\n",
        "network",
    )
}

pub fn internal_server_error_response() -> Vec<u8> {
    build_response(
        500,
        "Internal Server Error",
        TEXT_CONTENT_TYPE,
        b"processor failure\n",
        "network",
    )
}

pub fn incomplete_request_response() -> Vec<u8> {
    build_response(
        400,
        "Bad Request",
        TEXT_CONTENT_TYPE,
        b"incomplete request\n",
        "network",
    )
}

fn parse_request_route(request: &[u8]) -> RequestRoute {
    let Some(line_end) = request.iter().position(|byte| *byte == b'\n') else {
        return RequestRoute::BadRequest;
    };

    let request_line = request[..line_end]
        .strip_suffix(b"\r")
        .unwrap_or(&request[..line_end]);
    let mut parts = request_line.split(|byte| *byte == b' ');

    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return RequestRoute::BadRequest;
    };

    if method != b"GET" {
        return RequestRoute::MethodNotAllowed;
    }

    if version != b"HTTP/1.1" && version != b"HTTP/1.0" {
        return RequestRoute::BadRequest;
    }

    if !target.starts_with(b"/") {
        return RequestRoute::BadRequest;
    }

    let path = target.split(|byte| *byte == b'?').next().unwrap_or(target);

    match path {
        b"/" => RequestRoute::Root,
        b"/health" => RequestRoute::Health,
        b"/hello" => RequestRoute::Hello,
        _ => RequestRoute::NotFound,
    }
}

fn build_response(
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
    backend: &str,
) -> Vec<u8> {
    let mut headers = String::with_capacity(160);
    write!(
        headers,
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         Server: gput\r\n\
         X-Gput-Backend: {backend}\r\n\
         \r\n",
        body.len()
    )
    .expect("writing to a String cannot fail");

    let mut response = Vec::with_capacity(headers.len() + body.len());
    response.extend_from_slice(headers.as_bytes());
    response.extend_from_slice(body);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_health_with_query_string() {
        let response = route_request(b"GET /health?probe=1 HTTP/1.1\r\nHost: x\r\n\r\n", "cpu");

        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with(b"\r\n\r\nok\n"));
    }

    #[test]
    fn rejects_non_get_methods() {
        let response = route_request(b"POST / HTTP/1.1\r\n\r\n", "cpu");

        assert!(response.starts_with(b"HTTP/1.1 405 Method Not Allowed\r\n"));
    }

    #[test]
    fn rejects_malformed_versions() {
        let response = route_request(b"GET / GPUT/6.6\r\n\r\n", "cpu");

        assert!(response.starts_with(b"HTTP/1.1 400 Bad Request\r\n"));
    }

    #[test]
    fn content_length_matches_body() {
        let response = route_request(b"GET /hello HTTP/1.1\r\n\r\n", "cpu");
        let separator = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("response has a header separator");
        let headers = std::str::from_utf8(&response[..separator]).expect("ASCII headers");
        let body = &response[separator + 4..];
        let content_length = headers
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .expect("content length header")
            .parse::<usize>()
            .expect("numeric content length");

        assert_eq!(content_length, body.len());
    }
}
