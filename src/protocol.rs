use std::fmt::Write as _;

pub const MIN_GPU_RESPONSE_SLOT_BYTES: usize = 256;

pub(crate) const GPU_ROOT_BODY: &str = "{\"name\":\"gput\",\"backend\":\"gpu\",\"message\":\"GET dispatched through a compute shader\"}\n";
pub(crate) const CPU_ROOT_BODY: &str =
    "{\"name\":\"gput\",\"backend\":\"cpu\",\"message\":\"GET handled by the CPU baseline\"}\n";
pub(crate) const HEALTH_BODY: &str = "ok\n";
pub(crate) const GPU_HELLO_BODY: &str = "hello from a compute shader\n";
pub(crate) const CPU_HELLO_BODY: &str = "hello from the CPU baseline\n";
pub(crate) const UTF8_BODY: &str = "räksmörgås kostar €5, hälsar UTF-8-ugglan 🦉🦀\n";
pub(crate) const NOT_FOUND_BODY: &str = "not found\n";
pub(crate) const BAD_REQUEST_BODY: &str = "bad request\n";
pub(crate) const METHOD_NOT_ALLOWED_BODY: &str = "method not allowed\n";

pub fn request_timeout_response() -> Vec<u8> {
    build_response(
        408,
        "Request Timeout",
        "text/plain; charset=utf-8",
        b"request timeout\n",
        "network",
    )
}

pub fn payload_too_large_response() -> Vec<u8> {
    build_response(
        413,
        "Content Too Large",
        "text/plain; charset=utf-8",
        b"request too large\n",
        "network",
    )
}

pub fn service_unavailable_response() -> Vec<u8> {
    build_response(
        503,
        "Service Unavailable",
        "text/plain; charset=utf-8",
        b"server overloaded\n",
        "network",
    )
}

pub fn internal_server_error_response() -> Vec<u8> {
    build_response(
        500,
        "Internal Server Error",
        "text/plain; charset=utf-8",
        b"processor failure\n",
        "network",
    )
}

pub fn incomplete_request_response() -> Vec<u8> {
    build_response(
        400,
        "Bad Request",
        "text/plain; charset=utf-8",
        b"incomplete request\n",
        "network",
    )
}

pub(crate) fn build_response(
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
    fn content_length_matches_multibyte_body_bytes() {
        let response = build_response(
            200,
            "OK",
            "text/plain; charset=utf-8",
            UTF8_BODY.as_bytes(),
            "cpu",
        );
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
        assert_eq!(body, UTF8_BODY.as_bytes());
    }
}
