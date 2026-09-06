struct Params {
    request_stride_words: u32,
    response_stride_words: u32,
    request_count: u32,
    route_count: u32,
    fallback_response_offset: u32,
    bad_request_response_offset: u32,
    method_not_allowed_response_offset: u32,
    _padding: u32,
};

struct RequestMeta {
    input_len: u32,
    word_offset: u32,
    _padding_0: u32,
    _padding_1: u32,
};

struct ResponseMeta {
    output_len: u32,
    status: u32,
    flags: u32,
    _padding: u32,
};

struct StringMeta {
    byte_offset: u32,
    byte_len: u32,
    scalar_len: u32,
    _padding: u32,
};

struct Writer {
    request_index: u32,
    cursor: u32,
    flags: u32,
    pending: u32,
};

struct RequestTarget {
    path_start: u32,
    path_len: u32,
    query_start: u32,
    query_len: u32,
    path_hash: u32,
    input_len: u32,
    valid: u32,
    _padding: u32,
};

@group(0) @binding(0)
var<uniform> params: Params;

@group(0) @binding(1)
var<storage, read> request_meta: array<RequestMeta>;

@group(0) @binding(2)
var<storage, read> input_words: array<u32>;

@group(0) @binding(3)
var<storage, read_write> response_meta: array<ResponseMeta>;

@group(0) @binding(4)
var<storage, read_write> output_words: array<u32>;

@group(0) @binding(5)
var<storage, read> string_meta: array<StringMeta>;

@group(0) @binding(6)
var<storage, read> string_words: array<u32>;

@group(0) @binding(7)
var<storage, read> router_words: array<u32>;

const FNV_OFFSET_BASIS: u32 = 2166136261u;
const FNV_PRIME: u32 = 16777619u;

const ROUTE_PATH_HASH: u32 = 0u;
const ROUTE_PATH_LEN: u32 = 1u;
const ROUTE_PATH_STRING: u32 = 2u;
const ROUTE_RESPONSE_OFFSET: u32 = 3u;

const RESPONSE_STATUS: u32 = 0u;
const RESPONSE_REASON_STRING: u32 = 1u;
const RESPONSE_CONTENT_TYPE_STRING: u32 = 2u;
const RESPONSE_PROGRAM_OFFSET: u32 = 3u;
const RESPONSE_PROGRAM_LEN: u32 = 4u;

const BODY_OP_CODE: u32 = 0u;
const BODY_OP_ARG_0: u32 = 1u;
const BODY_OP_ARG_1: u32 = 2u;

const RESPONSE_FLAG_OUTPUT_OVERFLOW: u32 = 1u;
const RESPONSE_FLAG_INVALID_PROGRAM: u32 = 4u;

fn request_byte(request_index: u32, byte_index: u32) -> u32 {
    let word_index = request_meta[request_index].word_offset + byte_index / 4u;
    let shift = (byte_index & 3u) * 8u;
    return (input_words[word_index] >> shift) & 255u;
}

// Call only for four bytes inside the request, not for a partial tail.
fn request_word(request_index: u32, byte_index: u32) -> u32 {
    let word_index = request_meta[request_index].word_offset + byte_index / 4u;
    let shift = (byte_index & 3u) * 8u;
    let low = input_words[word_index];
    if (shift == 0u) {
        return low;
    }
    return (low >> shift) | (input_words[word_index + 1u] << (32u - shift));
}

fn string_byte(string_id: u32, byte_index: u32) -> u32 {
    let absolute_index = string_meta[string_id].byte_offset + byte_index;
    let word = string_words[absolute_index / 4u];
    let shift = (absolute_index & 3u) * 8u;
    return (word >> shift) & 255u;
}

// The immutable arena is byte-packed; strings need not start on a word boundary.
fn string_word(string_id: u32, byte_index: u32) -> u32 {
    let absolute_index = string_meta[string_id].byte_offset + byte_index;
    let word_index = absolute_index / 4u;
    let shift = (absolute_index & 3u) * 8u;
    let low = string_words[word_index];
    if (shift == 0u) {
        return low;
    }
    return (low >> shift) | (string_words[word_index + 1u] << (32u - shift));
}

fn route_word(route_index: u32, field: u32) -> u32 {
    return router_words[route_index * ROUTE_STRIDE_WORDS + field];
}

fn response_word(response_offset: u32, field: u32) -> u32 {
    return router_words[response_offset + field];
}

fn body_op_word(program_offset: u32, operation_index: u32, field: u32) -> u32 {
    return router_words[program_offset + operation_index * BODY_OP_WORDS + field];
}

fn writer_new(request_index: u32) -> Writer {
    return Writer(request_index, 0u, 0u, 0u);
}

fn writer_fail(writer: ptr<function, Writer>, flag: u32) {
    (*writer).flags = (*writer).flags | flag;
}

// Each invocation owns its entire response slot. Assemble partial words locally;
// never load old output bytes, including when reusing a slot for a shorter response.
fn writer_push_byte(writer: ptr<function, Writer>, byte: u32) {
    if ((*writer).flags != 0u) {
        return;
    }
    let capacity = params.response_stride_words * 4u;
    if ((*writer).cursor >= capacity) {
        writer_fail(writer, RESPONSE_FLAG_OUTPUT_OVERFLOW);
        return;
    }

    let shift = ((*writer).cursor & 3u) * 8u;
    (*writer).pending = (*writer).pending | ((byte & 255u) << shift);
    (*writer).cursor = (*writer).cursor + 1u;
    if (((*writer).cursor & 3u) == 0u) {
        let word_index = (*writer).request_index * params.response_stride_words
            + (*writer).cursor / 4u - 1u;
        output_words[word_index] = (*writer).pending;
        (*writer).pending = 0u;
    }
}

fn writer_push_word(writer: ptr<function, Writer>, word: u32) {
    if ((*writer).flags != 0u) {
        return;
    }
    let capacity = params.response_stride_words * 4u;
    if (capacity - (*writer).cursor < 4u) {
        writer_fail(writer, RESPONSE_FLAG_OUTPUT_OVERFLOW);
        return;
    }

    let shift = ((*writer).cursor & 3u) * 8u;
    let word_index = (*writer).request_index * params.response_stride_words
        + (*writer).cursor / 4u;
    output_words[word_index] = (*writer).pending | (word << shift);
    (*writer).cursor = (*writer).cursor + 4u;
    (*writer).pending = 0u;
    if (shift != 0u) {
        (*writer).pending = word >> (32u - shift);
    }
}

fn writer_push_string(writer: ptr<function, Writer>, string_id: u32) {
    // Rust String validates UTF-8 before upload. Copy its bytes, rather than
    // decoding and re-encoding the same immutable scalars on every request.
    let byte_len = string_meta[string_id].byte_len;
    var byte_index = 0u;
    while (byte_len - byte_index >= 4u) {
        writer_push_word(writer, string_word(string_id, byte_index));
        byte_index = byte_index + 4u;
    }
    while (byte_index < byte_len) {
        writer_push_byte(writer, string_byte(string_id, byte_index));
        byte_index = byte_index + 1u;
    }
}

fn writer_push_request_range(
    writer: ptr<function, Writer>,
    request_index: u32,
    byte_start: u32,
    byte_len: u32,
) {
    var byte_index = 0u;
    while (byte_len - byte_index >= 4u) {
        writer_push_word(writer, request_word(request_index, byte_start + byte_index));
        byte_index = byte_index + 4u;
    }
    while (byte_index < byte_len) {
        writer_push_byte(writer, request_byte(request_index, byte_start + byte_index));
        byte_index = byte_index + 1u;
    }
}

fn decimal_width(value: u32) -> u32 {
    var width = 1u;
    var remaining = value;
    while (remaining >= 10u) {
        remaining = remaining / 10u;
        width = width + 1u;
    }
    return width;
}

fn writer_push_decimal(writer: ptr<function, Writer>, value: u32) {
    var divisor = 1u;
    while (value / divisor >= 10u) {
        divisor = divisor * 10u;
    }

    loop {
        writer_push_byte(writer, 48u + (value / divisor) % 10u);
        if (divisor == 1u) {
            break;
        }
        divisor = divisor / 10u;
    }
}

fn writer_finish(writer: Writer, status: u32) {
    if (writer.flags != 0u) {
        response_meta[writer.request_index] = ResponseMeta(0u, 500u, writer.flags, 0u);
        return;
    }
    if ((writer.cursor & 3u) != 0u) {
        let word_index = writer.request_index * params.response_stride_words + writer.cursor / 4u;
        output_words[word_index] = writer.pending;
    }
    response_meta[writer.request_index] = ResponseMeta(writer.cursor, status, 0u, 0u);
}

fn is_get_request(request_index: u32, input_len: u32) -> bool {
    if (input_len < 4u) {
        return false;
    }
    return request_word(request_index, 0u) == 0x20544547u;
}

fn has_supported_http_version(request_index: u32, version_start: u32, input_len: u32) -> bool {
    if (version_start + 8u >= input_len) {
        return false;
    }

    let version_matches = request_byte(request_index, version_start + 0u) == 72u
        && request_byte(request_index, version_start + 1u) == 84u
        && request_byte(request_index, version_start + 2u) == 84u
        && request_byte(request_index, version_start + 3u) == 80u
        && request_byte(request_index, version_start + 4u) == 47u
        && request_byte(request_index, version_start + 5u) == 49u
        && request_byte(request_index, version_start + 6u) == 46u
        && (
            request_byte(request_index, version_start + 7u) == 48u
            || request_byte(request_index, version_start + 7u) == 49u
        );

    if (!version_matches) {
        return false;
    }

    let terminator = request_byte(request_index, version_start + 8u);
    if (terminator == 10u) {
        return true;
    }

    return terminator == 13u
        && version_start + 9u < input_len
        && request_byte(request_index, version_start + 9u) == 10u;
}

fn invalid_target(input_len: u32) -> RequestTarget {
    return RequestTarget(0u, 0u, 0u, 0u, 0u, input_len, 0u, 0u);
}

fn parse_get_target(request_index: u32, input_len: u32) -> RequestTarget {
    if (input_len <= 4u || request_byte(request_index, 4u) != 47u) {
        return invalid_target(input_len);
    }

    var cursor = 4u;
    var path_hash = FNV_OFFSET_BASIS;
    var path_len = 0u;
    var query_start = 0u;
    var query_len = 0u;
    var in_query = false;
    var found_target_end = false;

    loop {
        if (cursor >= input_len) {
            break;
        }

        let byte = request_byte(request_index, cursor);
        if (byte == 32u) {
            found_target_end = true;
            break;
        }

        if (!in_query) {
            if (byte == 63u) {
                in_query = true;
                query_start = cursor + 1u;
            } else {
                path_hash = (path_hash ^ byte) * FNV_PRIME;
                path_len = path_len + 1u;
            }
        } else {
            query_len = query_len + 1u;
        }

        cursor = cursor + 1u;
    }

    if (!found_target_end || path_len == 0u) {
        return invalid_target(input_len);
    }

    let version_start = cursor + 1u;
    if (!has_supported_http_version(request_index, version_start, input_len)) {
        return invalid_target(input_len);
    }

    if (!in_query) {
        query_start = cursor;
    }

    return RequestTarget(
        4u,
        path_len,
        query_start,
        query_len,
        path_hash,
        input_len,
        1u,
        0u,
    );
}

fn route_matches(request_index: u32, request_target: RequestTarget, route_index: u32) -> bool {
    if (route_word(route_index, ROUTE_PATH_HASH) != request_target.path_hash
        || route_word(route_index, ROUTE_PATH_LEN) != request_target.path_len)
    {
        return false;
    }

    let route_string_id = route_word(route_index, ROUTE_PATH_STRING);
    var byte_index = 0u;
    while (request_target.path_len - byte_index >= 4u) {
        if (request_word(request_index, request_target.path_start + byte_index)
            != string_word(route_string_id, byte_index))
        {
            return false;
        }
        byte_index = byte_index + 4u;
    }
    while (byte_index < request_target.path_len) {
        if (request_byte(request_index, request_target.path_start + byte_index)
            != string_byte(route_string_id, byte_index))
        {
            return false;
        }
        byte_index = byte_index + 1u;
    }
    return true;
}

fn find_response_offset(request_index: u32, request_target: RequestTarget) -> u32 {
    var lower = 0u;
    var upper = params.route_count;

    while (lower < upper) {
        let middle = lower + (upper - lower) / 2u;
        let route_hash = route_word(middle, ROUTE_PATH_HASH);
        if (route_hash < request_target.path_hash) {
            lower = middle + 1u;
        } else {
            upper = middle;
        }
    }

    var route_index = lower;
    loop {
        if (route_index >= params.route_count) {
            break;
        }
        if (route_word(route_index, ROUTE_PATH_HASH) != request_target.path_hash) {
            break;
        }
        if (route_matches(request_index, request_target, route_index)) {
            return route_word(route_index, ROUTE_RESPONSE_OFFSET);
        }
        route_index = route_index + 1u;
    }

    return params.fallback_response_offset;
}

fn response_body_len(response_offset: u32, request_target: RequestTarget) -> u32 {
    let program_offset = response_word(response_offset, RESPONSE_PROGRAM_OFFSET);
    let operation_count = response_word(response_offset, RESPONSE_PROGRAM_LEN);
    var body_len = 0u;

    for (
        var operation_index = 0u;
        operation_index < operation_count;
        operation_index = operation_index + 1u
    ) {
        let opcode = body_op_word(program_offset, operation_index, BODY_OP_CODE);
        let arg_0 = body_op_word(program_offset, operation_index, BODY_OP_ARG_0);
        let arg_1 = body_op_word(program_offset, operation_index, BODY_OP_ARG_1);

        switch opcode {
            case BODY_OP_LITERAL: {
                body_len = body_len + string_meta[arg_0].byte_len;
            }
            case BODY_OP_PATH: {
                body_len = body_len + min(request_target.path_len, arg_0);
            }
            case BODY_OP_QUERY: {
                body_len = body_len + min(request_target.query_len, arg_0);
            }
            case BODY_OP_BACKEND: {
                body_len = body_len + string_meta[STRING_BACKEND_GPU].byte_len;
            }
            case BODY_OP_REQUEST_BYTES: {
                body_len = body_len + decimal_width(request_target.input_len);
            }
            case BODY_OP_PATH_HASH: {
                body_len = body_len + decimal_width(request_target.path_hash);
            }
            case BODY_OP_BACKEND_VARIANT: {
                body_len = body_len + string_meta[arg_1].byte_len;
            }
            default: {}
        }
    }

    return body_len;
}

fn write_response_body(
    writer: ptr<function, Writer>,
    request_index: u32,
    response_offset: u32,
    request_target: RequestTarget,
) {
    let program_offset = response_word(response_offset, RESPONSE_PROGRAM_OFFSET);
    let operation_count = response_word(response_offset, RESPONSE_PROGRAM_LEN);

    for (
        var operation_index = 0u;
        operation_index < operation_count;
        operation_index = operation_index + 1u
    ) {
        let opcode = body_op_word(program_offset, operation_index, BODY_OP_CODE);
        let arg_0 = body_op_word(program_offset, operation_index, BODY_OP_ARG_0);
        let arg_1 = body_op_word(program_offset, operation_index, BODY_OP_ARG_1);

        switch opcode {
            case BODY_OP_LITERAL: {
                writer_push_string(writer, arg_0);
            }
            case BODY_OP_PATH: {
                writer_push_request_range(
                    writer,
                    request_index,
                    request_target.path_start,
                    min(request_target.path_len, arg_0),
                );
            }
            case BODY_OP_QUERY: {
                writer_push_request_range(
                    writer,
                    request_index,
                    request_target.query_start,
                    min(request_target.query_len, arg_0),
                );
            }
            case BODY_OP_BACKEND: {
                writer_push_string(writer, STRING_BACKEND_GPU);
            }
            case BODY_OP_REQUEST_BYTES: {
                writer_push_decimal(writer, request_target.input_len);
            }
            case BODY_OP_PATH_HASH: {
                writer_push_decimal(writer, request_target.path_hash);
            }
            case BODY_OP_BACKEND_VARIANT: {
                writer_push_string(writer, arg_1);
            }
            default: {
                writer_fail(writer, RESPONSE_FLAG_INVALID_PROGRAM);
            }
        }
    }
}

fn write_response(
    request_index: u32,
    response_offset: u32,
    request_target: RequestTarget,
) {
    let status = response_word(response_offset, RESPONSE_STATUS);
    let reason_string_id = response_word(response_offset, RESPONSE_REASON_STRING);
    let content_type_string_id = response_word(response_offset, RESPONSE_CONTENT_TYPE_STRING);
    let body_len = response_body_len(response_offset, request_target);
    var writer = writer_new(request_index);

    writer_push_string(&writer, STRING_HTTP_VERSION);
    writer_push_decimal(&writer, status);
    writer_push_byte(&writer, 32u);
    writer_push_string(&writer, reason_string_id);
    writer_push_string(&writer, STRING_HEADER_CONTENT_TYPE);
    writer_push_string(&writer, content_type_string_id);
    writer_push_string(&writer, STRING_HEADER_CONTENT_LENGTH);
    writer_push_decimal(&writer, body_len);
    writer_push_string(&writer, STRING_HEADER_TAIL);
    write_response_body(&writer, request_index, response_offset, request_target);
    writer_finish(writer, status);
}

@compute @workgroup_size(128)
fn process_requests(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let request_index = global_id.x;
    if (request_index >= params.request_count) {
        return;
    }

    let input_len = request_meta[request_index].input_len;
    if (input_len < 4u) {
        write_response(
            request_index,
            params.bad_request_response_offset,
            invalid_target(input_len),
        );
        return;
    }

    if (!is_get_request(request_index, input_len)) {
        write_response(
            request_index,
            params.method_not_allowed_response_offset,
            invalid_target(input_len),
        );
        return;
    }

    let request_target = parse_get_target(request_index, input_len);
    if (request_target.valid == 0u) {
        write_response(request_index, params.bad_request_response_offset, request_target);
        return;
    }

    let response_offset = find_response_offset(request_index, request_target);
    write_response(request_index, response_offset, request_target);
}
