use gput::{
    Router,
    processor::{CpuProcessor, GpuProcessor, Processor, ProcessorLimits},
    response::{Body, Response},
    routing::get,
};

fn gpu_header(mut response: Vec<u8>) -> Vec<u8> {
    let separator = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .expect("HTTP header separator");
    let headers = std::str::from_utf8(&response[..separator]).expect("ASCII headers");
    let backend = headers
        .find("X-Gput-Backend: cpu")
        .expect("CPU reference identifies its backend")
        + "X-Gput-Backend: ".len();
    response[backend..backend + 3].copy_from_slice(b"gpu");
    response
}

fn assert_content_length(response: &[u8]) {
    let separator = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .expect("HTTP header separator");
    let headers = std::str::from_utf8(&response[..separator]).expect("ASCII headers");
    let length: usize = headers
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .expect("Content-Length")
        .parse()
        .expect("decimal length");
    assert_eq!(length, response.len() - separator - 4);
    assert!(!headers.contains("Connection: close"));
}

fn request(path: &str) -> Vec<u8> {
    format!("GET {path} HTTP/1.1\r\nHost: test\r\n\r\n").into_bytes()
}

#[test]
fn oversized_response_is_rejected_before_requesting_a_gpu() {
    let router = Router::new().route("/large", get(Response::text("x".repeat(1_024))));
    let result = GpuProcessor::with_router(
        ProcessorLimits {
            max_batch_size: 1,
            max_request_bytes: 1_024,
            response_slot_bytes: 256,
        },
        router,
    );
    let error = result.err().expect("response cannot fit");
    assert!(error.to_string().contains("response slot"));
}

#[test]
#[ignore = "requires a wgpu adapter; PR CI runs this explicitly on Lavapipe"]
fn packed_gpu_writer_matches_cpu_across_alignments_and_reused_slots() {
    let mut router = Router::new();
    let mut inputs = Vec::new();
    for padding in 0..4 {
        for length in [0, 1, 2, 3, 4, 5, 7, 8, 9, 31, 32, 33, 255, 256, 257, 1_024] {
            let path = format!("/static/{padding}/{length}");
            let body = Body::new()
                .push("p".repeat(padding))
                .push("x".repeat(length))
                .push("å€🦉🦀\0");
            router = router.route(&path, get(Response::text(body)));
            inputs.push(request(&path));
        }

        let path = format!("/dynamic{}", "p".repeat(padding));
        let body = Body::new()
            .push("p".repeat(padding))
            .path(64)
            .push("|")
            .query(7)
            .push("|")
            .request_bytes()
            .push("|")
            .path_hash()
            .backend_variant("å€🦉", "å€🦉");
        router = router.route(&path, get(Response::text(body)));
        for length in 0..16 {
            inputs.push(request(&format!("{path}?{}å€🦉", "q".repeat(length))));
        }
    }

    // These distinct paths have the same FNV-1a hash (525512704).
    // The word comparison must still distinguish them after the index lookup.
    for (path, body) in [
        ("/collision/19103e79ecf7a9c6", "first collision"),
        ("/collision/674d06731c68db44", "second collision"),
    ] {
        router = router.route(path, get(Response::text(body)));
        inputs.push(request(path));
    }
    router = router
        .route("/empty", get(Response::text("")))
        .route("/short", get(Response::text("!")))
        .route(
            "/backend",
            get(Response::text(
                Body::new().backend().push(":").backend_variant("cpu body", "🦉"),
            )),
        );
    inputs.extend([
        request("/empty"),
        request("/short"),
        request("/missing"),
        b"GET / HTTP/9.9\r\n\r\n".to_vec(),
        b"POST / HTTP/1.1\r\n\r\n".to_vec(),
        b"GET".to_vec(),
    ]);

    let mut cpu = CpuProcessor::with_router(router.clone()).expect("CPU router compiles");
    let mut gpu = GpuProcessor::with_router(
        ProcessorLimits {
            max_batch_size: 257,
            max_request_bytes: 1_024,
            response_slot_bytes: 4_096,
        },
        router,
    )
    .expect("an actual GPU dispatch is required; no skip or CPU fallback");

    for (round, count) in [257, 1, 2, 127, 128, 129, 3].into_iter().enumerate() {
        let requests: Vec<_> = (0..count)
            .map(|index| inputs[(index + round * 17) % inputs.len()].as_slice())
            .collect();
        let expected = cpu.process_batch(&requests).expect("CPU batch");
        let actual = gpu.process_batch(&requests).expect("GPU batch");
        assert_eq!(actual.len(), count);
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert_content_length(actual);
            assert_eq!(actual, &gpu_header(expected), "round {round}, request {index}");
        }
    }

    let short = request("/short");
    let expected = cpu.process_batch(&[&short]).expect("CPU short response");
    let actual = gpu.process_batch(&[&short]).expect("reused GPU slot");
    assert_eq!(actual[0], gpu_header(expected[0].clone()));

    let backend = request("/backend");
    let actual = gpu.process_batch(&[&backend]).expect("backend operations");
    assert_content_length(&actual[0]);
    assert!(actual[0].ends_with("\r\n\r\ngpu:🦉".as_bytes()));
    assert!(gpu.process_batch(&[]).expect("empty batch").is_empty());
}
