use gput::{
    Router,
    processor::{CpuProcessor, Processor},
    response::{Body, Response},
    routing::get,
};

#[test]
fn public_router_api_renders_request_data_from_the_compiled_program() {
    let router = Router::new().route(
        "/inspect",
        get(Response::text(
            Body::new()
                .push("path=")
                .path(64)
                .push(";query=")
                .query(64)
                .push(";backend=")
                .backend(),
        )),
    );
    let mut processor = CpuProcessor::with_router(router).expect("router compiles");

    let request: &[u8] = b"GET /inspect?owl=yes HTTP/1.1\r\nHost: test\r\n\r\n";
    let responses = processor
        .process_batch(&[request])
        .expect("request is processed");

    assert_eq!(responses.len(), 1);
    assert!(responses[0].starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert!(responses[0].ends_with(b"\r\n\r\npath=/inspect;query=owl=yes;backend=cpu"));
}

#[test]
fn shorthand_and_explicit_routes_compile_to_identical_responses() {
    let short = Router::new()
        .get("/borrowed", "å€🦉")
        .get("/owned", "owned".to_owned())
        .get("/dynamic", gput::Body::new().push("query=").query(3))
        .get("/json", gput::Response::json("{\"ok\":true}"))
        .get(
            "/missing",
            gput::Response::text("gone").status(gput::Status::NOT_FOUND),
        )
        .route("/method", get("method router"));
    let explicit = Router::new()
        .route("/borrowed", get(Response::text("å€🦉")))
        .route("/owned", get(Response::text("owned")))
        .route(
            "/dynamic",
            get(Response::text(Body::new().push("query=").query(3))),
        )
        .route("/json", get(Response::json("{\"ok\":true}")))
        .route(
            "/missing",
            get(Response::text("gone").status(gput::Status::NOT_FOUND)),
        )
        .route("/method", get(Response::text("method router")));
    let requests: Vec<_> = [
        "/borrowed",
        "/owned",
        "/dynamic?12345",
        "/json",
        "/missing",
        "/method",
    ]
    .into_iter()
    .map(|path| format!("GET {path} HTTP/1.1\r\n\r\n").into_bytes())
    .collect();
    let requests: Vec<_> = requests.iter().map(Vec::as_slice).collect();
    let mut short = CpuProcessor::with_router(short).expect("shorthand compiles");
    let mut explicit = CpuProcessor::with_router(explicit).expect("explicit router compiles");
    assert_eq!(
        short.process_batch(&requests).expect("shorthand responses"),
        explicit.process_batch(&requests).expect("explicit responses")
    );
}

#[test]
fn shorthand_preserves_duplicate_route_validation() {
    let result = CpuProcessor::with_router(
        Router::new()
            .get("/same", "first")
            .route("/same", get("second")),
    );
    assert!(result.is_err());
}
