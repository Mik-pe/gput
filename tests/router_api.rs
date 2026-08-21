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
