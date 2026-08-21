use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};

use crate::protocol::{
    BAD_REQUEST_BODY, CPU_HELLO_BODY, CPU_ROOT_BODY, GPU_HELLO_BODY, GPU_ROOT_BODY, HEALTH_BODY,
    METHOD_NOT_ALLOWED_BODY, NOT_FOUND_BODY, UTF8_BODY, build_response,
};

pub(crate) const ROUTE_STRIDE_WORDS: u32 = 4;
pub(crate) const RESPONSE_DESCRIPTOR_WORDS: u32 = 5;
pub(crate) const BODY_OP_WORDS: u32 = 3;

pub(crate) const BODY_OP_LITERAL: u32 = 0;
pub(crate) const BODY_OP_PATH: u32 = 1;
pub(crate) const BODY_OP_QUERY: u32 = 2;
pub(crate) const BODY_OP_BACKEND: u32 = 3;
pub(crate) const BODY_OP_REQUEST_BYTES: u32 = 4;
pub(crate) const BODY_OP_PATH_HASH: u32 = 5;
pub(crate) const BODY_OP_BACKEND_VARIANT: u32 = 6;

const JSON_CONTENT_TYPE: &str = "application/json";
const TEXT_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    code: u16,
    reason: &'static str,
}

impl Status {
    pub const OK: Self = Self::new(200, "OK");
    pub const BAD_REQUEST: Self = Self::new(400, "Bad Request");
    pub const NOT_FOUND: Self = Self::new(404, "Not Found");
    pub const METHOD_NOT_ALLOWED: Self = Self::new(405, "Method Not Allowed");

    pub const fn new(code: u16, reason: &'static str) -> Self {
        Self { code, reason }
    }

    pub const fn code(self) -> u16 {
        self.code
    }

    pub const fn reason(self) -> &'static str {
        self.reason
    }
}

#[derive(Debug, Clone, Default)]
pub struct Body {
    segments: Vec<BodySegment>,
}

impl Body {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn literal(mut self, literal: impl Into<String>) -> Self {
        let literal = literal.into();
        if !literal.is_empty() {
            self.segments.push(BodySegment::Literal(literal));
        }
        self
    }

    pub fn push(self, literal: impl Into<String>) -> Self {
        self.literal(literal)
    }

    pub fn path(mut self, max_bytes: usize) -> Self {
        self.segments.push(BodySegment::Path { max_bytes });
        self
    }

    pub fn query(mut self, max_bytes: usize) -> Self {
        self.segments.push(BodySegment::Query { max_bytes });
        self
    }

    pub fn backend(mut self) -> Self {
        self.segments.push(BodySegment::Backend);
        self
    }

    pub fn request_bytes(mut self) -> Self {
        self.segments.push(BodySegment::RequestBytes);
        self
    }

    pub fn path_hash(mut self) -> Self {
        self.segments.push(BodySegment::PathHash);
        self
    }

    pub fn backend_variant(mut self, cpu: impl Into<String>, gpu: impl Into<String>) -> Self {
        self.segments.push(BodySegment::BackendVariant {
            cpu: cpu.into(),
            gpu: gpu.into(),
        });
        self
    }
}

impl From<&str> for Body {
    fn from(value: &str) -> Self {
        Self::new().push(value)
    }
}

impl From<String> for Body {
    fn from(value: String) -> Self {
        Self::new().push(value)
    }
}

#[derive(Debug, Clone)]
enum BodySegment {
    Literal(String),
    Path { max_bytes: usize },
    Query { max_bytes: usize },
    Backend,
    RequestBytes,
    PathHash,
    BackendVariant { cpu: String, gpu: String },
}

#[derive(Debug, Clone)]
pub struct Response {
    status: Status,
    content_type: String,
    body: Body,
}

impl Response {
    pub fn text(body: impl Into<Body>) -> Self {
        Self {
            status: Status::OK,
            content_type: TEXT_CONTENT_TYPE.to_owned(),
            body: body.into(),
        }
    }

    pub fn json(body: impl Into<Body>) -> Self {
        Self {
            status: Status::OK,
            content_type: JSON_CONTENT_TYPE.to_owned(),
            body: body.into(),
        }
    }

    pub fn html(body: impl Into<Body>) -> Self {
        Self {
            status: Status::OK,
            content_type: "text/html; charset=utf-8".to_owned(),
            body: body.into(),
        }
    }

    pub fn status(mut self, status: Status) -> Self {
        self.status = status;
        self
    }

    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = content_type.into();
        self
    }
}

#[derive(Debug, Clone)]
pub struct MethodRouter {
    get: Response,
}

pub mod routing {
    use super::{MethodRouter, Response};

    pub fn get(response: Response) -> MethodRouter {
        MethodRouter { get: response }
    }
}

pub mod response {
    pub use super::{Body, Response, Status};
}

#[derive(Debug, Clone)]
struct RouteDefinition {
    path: String,
    response: Response,
}

#[derive(Debug, Clone)]
pub struct Router {
    routes: Vec<RouteDefinition>,
    fallback: Response,
    bad_request: Response,
    method_not_allowed: Response,
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Router {
    pub fn new() -> Self {
        Self {
            routes: Vec::new(),
            fallback: Response::text(NOT_FOUND_BODY).status(Status::NOT_FOUND),
            bad_request: Response::text(BAD_REQUEST_BODY).status(Status::BAD_REQUEST),
            method_not_allowed: Response::text(METHOD_NOT_ALLOWED_BODY)
                .status(Status::METHOD_NOT_ALLOWED),
        }
    }

    pub fn route(mut self, path: impl Into<String>, method_router: MethodRouter) -> Self {
        self.routes.push(RouteDefinition {
            path: path.into(),
            response: method_router.get,
        });
        self
    }

    pub fn fallback(mut self, response: Response) -> Self {
        self.fallback = response;
        self
    }

    pub fn bad_request(mut self, response: Response) -> Self {
        self.bad_request = response;
        self
    }

    pub fn method_not_allowed(mut self, response: Response) -> Self {
        self.method_not_allowed = response;
        self
    }

    pub(crate) fn compile(self) -> Result<CompiledRouter> {
        RouterCompiler::new().compile(self)
    }
}

pub fn builtin_router() -> Router {
    use routing::get;

    Router::new()
        .route(
            "/",
            get(Response::json(
                Body::new().backend_variant(CPU_ROOT_BODY, GPU_ROOT_BODY),
            )),
        )
        .route("/health", get(Response::text(HEALTH_BODY)))
        .route(
            "/hello",
            get(Response::text(
                Body::new().backend_variant(CPU_HELLO_BODY, GPU_HELLO_BODY),
            )),
        )
        .route("/utf8", get(Response::text(UTF8_BODY)))
        .route(
            "/inspect",
            get(Response::text(
                Body::new()
                    .push("path=")
                    .path(48)
                    .push("\nquery=")
                    .query(48)
                    .push("\nrequest_bytes=")
                    .request_bytes()
                    .push("\npath_hash=")
                    .path_hash()
                    .push("\nbackend=")
                    .backend()
                    .push("\n"),
            )),
        )
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ShaderStringIds {
    pub http_version: u32,
    pub header_content_type: u32,
    pub header_content_length: u32,
    pub header_tail: u32,
    pub backend_gpu: u32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GpuRouterLayout {
    pub route_count: u32,
    pub fallback_response_offset: u32,
    pub bad_request_response_offset: u32,
    pub method_not_allowed_response_offset: u32,
}

#[derive(Debug)]
pub(crate) struct CompiledRouter {
    routes: Vec<CompiledRoute>,
    fallback: CompiledResponse,
    bad_request: CompiledResponse,
    method_not_allowed: CompiledResponse,
    strings: Vec<String>,
    router_words: Vec<u32>,
    gpu_layout: GpuRouterLayout,
    shader_string_ids: ShaderStringIds,
    max_gpu_response_bytes: usize,
}

impl CompiledRouter {
    pub fn route_request(&self, request: &[u8], backend: &str) -> Vec<u8> {
        match parse_request(request) {
            RequestParse::Get(parts) => {
                let first_candidate = self
                    .routes
                    .partition_point(|route| route.path_hash < parts.path_hash);
                let response = self.routes[first_candidate..]
                    .iter()
                    .take_while(|route| route.path_hash == parts.path_hash)
                    .find(|route| route.path.as_bytes() == parts.path)
                    .map_or(&self.fallback, |route| &route.response);
                self.render_response(response, parts, request.len(), backend)
            }
            RequestParse::BadRequest => self.render_response(
                &self.bad_request,
                RequestParts::empty(),
                request.len(),
                backend,
            ),
            RequestParse::MethodNotAllowed => self.render_response(
                &self.method_not_allowed,
                RequestParts::empty(),
                request.len(),
                backend,
            ),
        }
    }

    pub fn validate_gpu_response_slot(&self, response_slot_bytes: usize) -> Result<()> {
        if response_slot_bytes < self.max_gpu_response_bytes {
            bail!(
                "router can generate responses up to {} bytes, but the GPU response slot is {response_slot_bytes} bytes; increase --response-slot-bytes",
                self.max_gpu_response_bytes
            );
        }
        Ok(())
    }

    pub fn strings(&self) -> &[String] {
        &self.strings
    }

    pub fn router_words(&self) -> &[u32] {
        &self.router_words
    }

    pub const fn gpu_layout(&self) -> GpuRouterLayout {
        self.gpu_layout
    }

    pub const fn shader_string_ids(&self) -> ShaderStringIds {
        self.shader_string_ids
    }

    pub const fn max_gpu_response_bytes(&self) -> usize {
        self.max_gpu_response_bytes
    }

    fn render_response(
        &self,
        response: &CompiledResponse,
        parts: RequestParts<'_>,
        request_len: usize,
        backend: &str,
    ) -> Vec<u8> {
        let mut body = Vec::with_capacity(response.max_cpu_body_bytes);

        for segment in &response.segments {
            match *segment {
                CompiledSegment::Literal(string_id) => {
                    body.extend_from_slice(self.string(string_id).as_bytes());
                }
                CompiledSegment::Path { max_bytes } => {
                    let len = parts.path.len().min(max_bytes);
                    body.extend_from_slice(&parts.path[..len]);
                }
                CompiledSegment::Query { max_bytes } => {
                    let len = parts.query.len().min(max_bytes);
                    body.extend_from_slice(&parts.query[..len]);
                }
                CompiledSegment::Backend => body.extend_from_slice(backend.as_bytes()),
                CompiledSegment::RequestBytes => {
                    body.extend_from_slice(request_len.to_string().as_bytes());
                }
                CompiledSegment::PathHash => {
                    body.extend_from_slice(parts.path_hash.to_string().as_bytes());
                }
                CompiledSegment::BackendVariant { cpu, gpu } => {
                    let string_id = if backend == "gpu" { gpu } else { cpu };
                    body.extend_from_slice(self.string(string_id).as_bytes());
                }
            }
        }

        build_response(
            response.status,
            self.string(response.reason_string_id),
            self.string(response.content_type_string_id),
            &body,
            backend,
        )
    }

    fn string(&self, id: u32) -> &str {
        &self.strings[id as usize]
    }
}

#[derive(Debug)]
struct CompiledRoute {
    path: String,
    path_hash: u32,
    response: CompiledResponse,
}

#[derive(Debug)]
struct CompiledResponse {
    status: u16,
    reason_string_id: u32,
    content_type_string_id: u32,
    segments: Vec<CompiledSegment>,
    max_gpu_body_bytes: usize,
    max_cpu_body_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
enum CompiledSegment {
    Literal(u32),
    Path { max_bytes: usize },
    Query { max_bytes: usize },
    Backend,
    RequestBytes,
    PathHash,
    BackendVariant { cpu: u32, gpu: u32 },
}

struct RouterCompiler {
    strings: StringTable,
    shader_string_ids: ShaderStringIds,
}

impl RouterCompiler {
    fn new() -> Self {
        let mut strings = StringTable::default();
        let shader_string_ids = ShaderStringIds {
            http_version: strings.intern("HTTP/1.1 "),
            header_content_type: strings.intern("\r\nContent-Type: "),
            header_content_length: strings.intern("\r\nContent-Length: "),
            header_tail: strings
                .intern("\r\nConnection: close\r\nServer: gput\r\nX-Gput-Backend: gpu\r\n\r\n"),
            backend_gpu: strings.intern("gpu"),
        };

        Self {
            strings,
            shader_string_ids,
        }
    }

    fn compile(mut self, router: Router) -> Result<CompiledRouter> {
        if router.routes.len() > u32::MAX as usize {
            bail!("route count does not fit u32");
        }

        let mut seen_paths = HashSet::with_capacity(router.routes.len());
        let mut routes = Vec::with_capacity(router.routes.len());

        for route in router.routes {
            validate_route_path(&route.path)?;
            if !seen_paths.insert(route.path.clone()) {
                bail!("duplicate GET route {:?}", route.path);
            }

            let path_hash = fnv1a(route.path.as_bytes());
            let response = self.compile_response(route.response)?;
            routes.push(CompiledRoute {
                path: route.path,
                path_hash,
                response,
            });
        }

        routes.sort_unstable_by(|left, right| {
            left.path_hash
                .cmp(&right.path_hash)
                .then_with(|| left.path.as_bytes().cmp(right.path.as_bytes()))
        });

        let fallback = self.compile_response(router.fallback)?;
        let bad_request = self.compile_response(router.bad_request)?;
        let method_not_allowed = self.compile_response(router.method_not_allowed)?;

        let route_table_words = routes
            .len()
            .checked_mul(ROUTE_STRIDE_WORDS as usize)
            .context("route table word count overflow")?;
        let mut router_words = vec![0_u32; route_table_words];

        for (route_index, route) in routes.iter().enumerate() {
            let response_offset = emit_response(&mut router_words, &route.response)?;
            let path_string_id = self.strings.intern(route.path.as_str());
            let base = route_index * ROUTE_STRIDE_WORDS as usize;
            router_words[base] = route.path_hash;
            router_words[base + 1] = u32::try_from(route.path.len())
                .context("route path byte length does not fit u32")?;
            router_words[base + 2] = path_string_id;
            router_words[base + 3] = response_offset;
        }

        let fallback_response_offset = emit_response(&mut router_words, &fallback)?;
        let bad_request_response_offset = emit_response(&mut router_words, &bad_request)?;
        let method_not_allowed_response_offset =
            emit_response(&mut router_words, &method_not_allowed)?;

        let max_gpu_response_bytes = routes
            .iter()
            .map(|route| {
                maximum_response_bytes(&self.strings, self.shader_string_ids, &route.response)
            })
            .chain([
                maximum_response_bytes(&self.strings, self.shader_string_ids, &fallback),
                maximum_response_bytes(&self.strings, self.shader_string_ids, &bad_request),
                maximum_response_bytes(&self.strings, self.shader_string_ids, &method_not_allowed),
            ])
            .try_fold(0_usize, |current, next| next.map(|next| current.max(next)))?;

        if max_gpu_response_bytes > u32::MAX as usize {
            bail!("maximum GPU response size does not fit u32");
        }
        if router_words.len() > u32::MAX as usize {
            bail!("GPU router program length does not fit u32");
        }

        let route_count = u32::try_from(routes.len()).context("route count does not fit u32")?;

        Ok(CompiledRouter {
            routes,
            fallback,
            bad_request,
            method_not_allowed,
            strings: self.strings.into_values(),
            router_words,
            gpu_layout: GpuRouterLayout {
                route_count,
                fallback_response_offset,
                bad_request_response_offset,
                method_not_allowed_response_offset,
            },
            shader_string_ids: self.shader_string_ids,
            max_gpu_response_bytes,
        })
    }

    fn compile_response(&mut self, response: Response) -> Result<CompiledResponse> {
        validate_status(response.status)?;
        validate_header_value("content type", &response.content_type)?;

        let reason_string_id = self.strings.intern(response.status.reason());
        let content_type_string_id = self.strings.intern(response.content_type.as_str());
        let mut segments = Vec::with_capacity(response.body.segments.len());
        let mut max_gpu_body_bytes = 0_usize;
        let mut max_cpu_body_bytes = 0_usize;

        for segment in response.body.segments {
            match segment {
                BodySegment::Literal(value) => {
                    let len = value.len();
                    let string_id = self.strings.intern(value);
                    segments.push(CompiledSegment::Literal(string_id));
                    max_gpu_body_bytes = checked_add(max_gpu_body_bytes, len, "GPU body")?;
                    max_cpu_body_bytes = checked_add(max_cpu_body_bytes, len, "CPU body")?;
                }
                BodySegment::Path { max_bytes } => {
                    let max_bytes = validate_dynamic_limit("path", max_bytes)?;
                    segments.push(CompiledSegment::Path { max_bytes });
                    max_gpu_body_bytes = checked_add(max_gpu_body_bytes, max_bytes, "GPU body")?;
                    max_cpu_body_bytes = checked_add(max_cpu_body_bytes, max_bytes, "CPU body")?;
                }
                BodySegment::Query { max_bytes } => {
                    let max_bytes = validate_dynamic_limit("query", max_bytes)?;
                    segments.push(CompiledSegment::Query { max_bytes });
                    max_gpu_body_bytes = checked_add(max_gpu_body_bytes, max_bytes, "GPU body")?;
                    max_cpu_body_bytes = checked_add(max_cpu_body_bytes, max_bytes, "CPU body")?;
                }
                BodySegment::Backend => {
                    segments.push(CompiledSegment::Backend);
                    max_gpu_body_bytes = checked_add(max_gpu_body_bytes, 3, "GPU body")?;
                    max_cpu_body_bytes = checked_add(max_cpu_body_bytes, 3, "CPU body")?;
                }
                BodySegment::RequestBytes => {
                    segments.push(CompiledSegment::RequestBytes);
                    max_gpu_body_bytes = checked_add(max_gpu_body_bytes, 10, "GPU body")?;
                    max_cpu_body_bytes = checked_add(max_cpu_body_bytes, 20, "CPU body")?;
                }
                BodySegment::PathHash => {
                    segments.push(CompiledSegment::PathHash);
                    max_gpu_body_bytes = checked_add(max_gpu_body_bytes, 10, "GPU body")?;
                    max_cpu_body_bytes = checked_add(max_cpu_body_bytes, 10, "CPU body")?;
                }
                BodySegment::BackendVariant { cpu, gpu } => {
                    let cpu_len = cpu.len();
                    let gpu_len = gpu.len();
                    let cpu = self.strings.intern(cpu);
                    let gpu = self.strings.intern(gpu);
                    segments.push(CompiledSegment::BackendVariant { cpu, gpu });
                    max_gpu_body_bytes = checked_add(max_gpu_body_bytes, gpu_len, "GPU body")?;
                    max_cpu_body_bytes =
                        checked_add(max_cpu_body_bytes, cpu_len.max(gpu_len), "CPU body")?;
                }
            }
        }

        Ok(CompiledResponse {
            status: response.status.code(),
            reason_string_id,
            content_type_string_id,
            segments,
            max_gpu_body_bytes,
            max_cpu_body_bytes,
        })
    }
}

#[derive(Default)]
struct StringTable {
    values: Vec<String>,
    ids: HashMap<String, u32>,
}

impl StringTable {
    fn intern(&mut self, value: impl AsRef<str>) -> u32 {
        let value = value.as_ref();
        if let Some(id) = self.ids.get(value) {
            return *id;
        }

        let id =
            u32::try_from(self.values.len()).expect("string table length validated at compile");
        let owned = value.to_owned();
        self.values.push(owned.clone());
        self.ids.insert(owned, id);
        id
    }

    fn get(&self, id: u32) -> &str {
        &self.values[id as usize]
    }

    fn into_values(self) -> Vec<String> {
        self.values
    }
}

fn emit_response(words: &mut Vec<u32>, response: &CompiledResponse) -> Result<u32> {
    let response_offset =
        u32::try_from(words.len()).context("router word offset does not fit u32")?;
    let program_offset = words
        .len()
        .checked_add(RESPONSE_DESCRIPTOR_WORDS as usize)
        .context("response program offset overflow")?;
    let program_offset =
        u32::try_from(program_offset).context("response program offset does not fit u32")?;
    let operation_count =
        u32::try_from(response.segments.len()).context("body operation count does not fit u32")?;

    words.extend_from_slice(&[
        u32::from(response.status),
        response.reason_string_id,
        response.content_type_string_id,
        program_offset,
        operation_count,
    ]);

    for segment in &response.segments {
        let operation = match *segment {
            CompiledSegment::Literal(string_id) => [BODY_OP_LITERAL, string_id, 0],
            CompiledSegment::Path { max_bytes } => [
                BODY_OP_PATH,
                u32::try_from(max_bytes).context("path byte limit does not fit u32")?,
                0,
            ],
            CompiledSegment::Query { max_bytes } => [
                BODY_OP_QUERY,
                u32::try_from(max_bytes).context("query byte limit does not fit u32")?,
                0,
            ],
            CompiledSegment::Backend => [BODY_OP_BACKEND, 0, 0],
            CompiledSegment::RequestBytes => [BODY_OP_REQUEST_BYTES, 0, 0],
            CompiledSegment::PathHash => [BODY_OP_PATH_HASH, 0, 0],
            CompiledSegment::BackendVariant { cpu, gpu } => [BODY_OP_BACKEND_VARIANT, cpu, gpu],
        };
        words.extend_from_slice(&operation);
    }

    Ok(response_offset)
}

fn maximum_response_bytes(
    strings: &StringTable,
    ids: ShaderStringIds,
    response: &CompiledResponse,
) -> Result<usize> {
    let header_bytes = strings
        .get(ids.http_version)
        .len()
        .checked_add(decimal_digits(response.status as usize))
        .and_then(|bytes| bytes.checked_add(1))
        .and_then(|bytes| bytes.checked_add(strings.get(response.reason_string_id).len()))
        .and_then(|bytes| bytes.checked_add(strings.get(ids.header_content_type).len()))
        .and_then(|bytes| bytes.checked_add(strings.get(response.content_type_string_id).len()))
        .and_then(|bytes| bytes.checked_add(strings.get(ids.header_content_length).len()))
        .and_then(|bytes| bytes.checked_add(decimal_digits(response.max_gpu_body_bytes)))
        .and_then(|bytes| bytes.checked_add(strings.get(ids.header_tail).len()))
        .context("GPU response header size overflow")?;

    header_bytes
        .checked_add(response.max_gpu_body_bytes)
        .context("GPU response size overflow")
}

fn validate_route_path(path: &str) -> Result<()> {
    if !path.starts_with('/') {
        bail!("route path {path:?} must start with '/'");
    }
    if path.contains('?') || path.contains('#') {
        bail!("route path {path:?} must not contain a query string or fragment");
    }
    if path
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        bail!("route path {path:?} contains an HTTP separator or control byte");
    }
    if path.len() > u32::MAX as usize {
        bail!("route path byte length does not fit u32");
    }
    Ok(())
}

fn validate_status(status: Status) -> Result<()> {
    if !(100..=999).contains(&status.code()) {
        bail!("HTTP status code {} must have three digits", status.code());
    }
    validate_header_value("status reason", status.reason())
}

fn validate_header_value(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{label} cannot be empty");
    }
    if !value.is_ascii() || value.bytes().any(|byte| byte.is_ascii_control()) {
        bail!("{label} must be printable ASCII without control bytes");
    }
    Ok(())
}

fn validate_dynamic_limit(label: &str, max_bytes: usize) -> Result<usize> {
    if max_bytes > u32::MAX as usize {
        bail!("{label} byte limit does not fit u32");
    }
    Ok(max_bytes)
}

fn checked_add(current: usize, value: usize, label: &str) -> Result<usize> {
    current
        .checked_add(value)
        .with_context(|| format!("{label} size overflow"))
}

fn decimal_digits(value: usize) -> usize {
    value.max(1).ilog10() as usize + 1
}

fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(2_166_136_261_u32, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(16_777_619)
    })
}

#[derive(Debug, Clone, Copy)]
struct RequestParts<'a> {
    path: &'a [u8],
    query: &'a [u8],
    path_hash: u32,
}

impl RequestParts<'static> {
    const fn empty() -> Self {
        Self {
            path: &[],
            query: &[],
            path_hash: 0,
        }
    }
}

enum RequestParse<'a> {
    Get(RequestParts<'a>),
    BadRequest,
    MethodNotAllowed,
}

fn parse_request(request: &[u8]) -> RequestParse<'_> {
    let Some(line_end) = request.iter().position(|byte| *byte == b'\n') else {
        return RequestParse::BadRequest;
    };

    let request_line = request[..line_end]
        .strip_suffix(b"\r")
        .unwrap_or(&request[..line_end]);
    let mut parts = request_line.split(|byte| *byte == b' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return RequestParse::BadRequest;
    };

    if method != b"GET" {
        return RequestParse::MethodNotAllowed;
    }
    if version != b"HTTP/1.1" && version != b"HTTP/1.0" {
        return RequestParse::BadRequest;
    }
    if !target.starts_with(b"/") {
        return RequestParse::BadRequest;
    }

    let (path, query) = target
        .iter()
        .position(|byte| *byte == b'?')
        .map_or((target, &[][..]), |query_mark| {
            (&target[..query_mark], &target[query_mark + 1..])
        });

    if path.is_empty() {
        return RequestParse::BadRequest;
    }

    RequestParse::Get(RequestParts {
        path,
        query,
        path_hash: fnv1a(path),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(router: Router) -> CompiledRouter {
        router.compile().expect("router compiles")
    }

    #[test]
    fn routes_query_strings_without_hiding_them_from_the_body_program() {
        let router = compile(Router::new().route(
            "/inspect",
            routing::get(Response::text(Body::new().path(64).push("|").query(64))),
        ));

        let response =
            router.route_request(b"GET /inspect?owl=yes HTTP/1.1\r\nHost: x\r\n\r\n", "cpu");

        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with(b"\r\n\r\n/inspect|owl=yes"));
    }

    #[test]
    fn rejects_duplicate_paths_during_compilation() {
        let result = Router::new()
            .route("/same", routing::get(Response::text("one")))
            .route("/same", routing::get(Response::text("two")))
            .compile();

        assert!(result.is_err());
    }

    #[test]
    fn backend_variants_keep_cpu_and_gpu_contracts_distinct() {
        let router = compile(Router::new().route(
            "/",
            routing::get(Response::text(
                Body::new().backend_variant("cpu body", "gpu body"),
            )),
        ));

        assert!(
            router
                .route_request(b"GET / HTTP/1.1\r\n\r\n", "cpu")
                .ends_with(b"cpu body")
        );
        assert!(
            router
                .route_request(b"GET / HTTP/1.1\r\n\r\n", "gpu")
                .ends_with(b"gpu body")
        );
    }

    #[test]
    fn generated_router_program_has_real_routes_and_bounded_responses() {
        let router = compile(builtin_router());

        assert_eq!(router.gpu_layout().route_count, 5);
        assert!(!router.router_words().is_empty());
        assert!(router.max_gpu_response_bytes() <= 512);
        assert!(
            router
                .routes
                .windows(2)
                .all(|routes| routes[0].path_hash <= routes[1].path_hash)
        );
    }

    #[test]
    fn fnv_hashes_match_the_shader_contract() {
        assert_eq!(fnv1a(b"/"), 705_468_254);
        assert_eq!(fnv1a(b"/health"), 1_923_151_932);
        assert_eq!(fnv1a(b"/hello"), 4_088_401_502);
        assert_eq!(fnv1a(b"/utf8"), 1_582_453_113);
        assert_eq!(fnv1a(b"/inspect"), 3_938_772_546);
    }
}
