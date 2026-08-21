use std::fmt::Write as _;

pub const MAX_GPU_RESPONSE_BYTES: usize = 256;

const JSON_CONTENT_TYPE: &str = "application/json";
const TEXT_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

pub(crate) const GPU_ROOT_BODY: &str =
    "{\"name\":\"gput\",\"backend\":\"gpu\",\"message\":\"GET dispatched through a compute shader\"}\n";
const CPU_ROOT_BODY: &str =
    "{\"name\":\"gput\",\"backend\":\"cpu\",\"message\":\"GET handled by the CPU baseline\"}\n";
pub(crate) const HEALTH_BODY: &str = "ok\n";
pub(crate) const GPU_HELLO_BODY: &str = "hello from a compute shader\n";
const CPU_HELLO_BODY: &str = "hello from the CPU baseline\n";
pub(crate) const UTF8_BODY: &str =
    "räksmörgås kostar €5, hälsar UTF-8-ugglan 🦉🦀\n";
pub(crate) const NOT_FOUND_BODY: &str = "not found\n";
pub(crate) const BAD_REQUEST_BODY: &str = "bad request\n";
pub(crate) const METHOD_NOT_ALLOWED_BODY: &str = "method not allowed\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestRoute {
    Root,
    Health,
    Hello,
    Utf8,
    NotFound,
    BadRequest,
    MethodNotAllowed,
}

pub fn route_request(request: &[u8], backend: &'static str) -> Vec<u8> {
    match parse_request_route(request) {
        RequestRoute::Root => {
            let body = if backend == "gpu" {
                GPU_ROOT_BODY
            } else {
                CPU_ROOT_BODY
            };
            build_response(200, "OK", JSON_CONTENT_TYPE, body.as_bytes(), backend)
        }
        RequestRoute::Health => build_response(
            200,
            "OK",
            TEXT_CONTENT_TYPE,
            HEALTH_BODY.as_bytes(),
            backend,
        ),
        RequestRoute::Hello => {
            let body = if backend == "gpu" {
                GPU_HELLO_BODY
            } else {
                CPU_HELLO_BODY
            };
            build_response(200, "OK", TEXT_CONTENT_TYPE, body.as_bytes(), backend)
        }
        RequestRoute::Utf8 => build_response(
            200,
            "OK",
            TEXT_CONTENT_TYPE,
            UTF8_BODY.as_bytes(),
            backend,
        ),
        RequestRoute::NotFound => build_response(
            404,
            "Not Found",
            TEXT_CONTENT_TYPE,
            NOT_FOUND_BODY.as_bytes(),
            backend,
        ),
        RequestRoute::BadRequest => build_response(
            400,
            "Bad Request",
            TEXT_CONTENT_TYPE,
            BAD_REQUEST_BODY.as_bytes(),
            backend,
        ),
        RequestRoute::MethodNotAllowed => build_response(
            405,
            "Method Not Allowed",
            TEXT_CONTENT_TYPE,
            METHOD_NOT_ALLOWED_BODY.as_bytes(),
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
        b"/utf8" => RequestRoute::Utf8,
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
    fn routes_multibyte_utf8_body() {
        let response = route_request(b"GET /utf8 HTTP/1.1\r\nHost: x\r\n\r\n", "cpu");

        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with(UTF8_BODY.as_bytes()));
        assert!(UTF8_BODY.len() > UTF8_BODY.chars().count());
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
        let response = route_request(b"GET /utf8 HTTP/1.1\r\n\r\n", "cpu");
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

    #[test]
    fn gpu_builtin_responses_fit_default_slot() {
        for path in ["/", "/health", "/hello", "/utf8", "/missing"] {
            let request = format!("GET {path} HTTP/1.1\r\n\r\n");
            let response = route_request(request.as_bytes(), "gpu");
            assert!(
                response.len() <= MAX_GPU_RESPONSE_BYTES,
                "{path} response is {} bytes",
                response.len()
            );
        }
    }
}
