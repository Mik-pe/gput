struct Params {
    request_stride_words: u32,
    response_stride_words: u32,
    request_count: u32,
    _padding: u32,
};

struct RequestMeta {
    input_len: u32,
    _padding_0: u32,
    _padding_1: u32,
    _padding_2: u32,
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
    _padding: u32,
};

struct Utf8Scalar {
    code_point: u32,
    byte_width: u32,
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

const FNV_OFFSET_BASIS: u32 = 2166136261u;
const FNV_PRIME: u32 = 16777619u;

const ROOT_PATH_HASH: u32 = 705468254u;
const HEALTH_PATH_HASH: u32 = 1923151932u;
const HELLO_PATH_HASH: u32 = 4088401502u;
const UTF8_PATH_HASH: u32 = 1582453113u;

const RESPONSE_ROOT_ID: u32 = 0u;
const RESPONSE_HEALTH_ID: u32 = 1u;
const RESPONSE_HELLO_ID: u32 = 2u;
const RESPONSE_UTF8_ID: u32 = 3u;
const RESPONSE_BAD_REQUEST_ID: u32 = 4u;
const RESPONSE_METHOD_NOT_ALLOWED_ID: u32 = 5u;
const RESPONSE_NOT_FOUND_ID: u32 = 6u;

const RESPONSE_FLAG_OUTPUT_OVERFLOW: u32 = 1u;
const RESPONSE_FLAG_INVALID_UTF8: u32 = 2u;

fn request_byte(request_index: u32, byte_index: u32) -> u32 {
    let word_index = request_index * params.request_stride_words + byte_index / 4u;
    let shift = (byte_index & 3u) * 8u;
    return (input_words[word_index] >> shift) & 255u;
}

fn string_byte(string_id: u32, byte_index: u32) -> u32 {
    let absolute_index = string_meta[string_id].byte_offset + byte_index;
    let word = string_words[absolute_index / 4u];
    let shift = (absolute_index & 3u) * 8u;
    return (word >> shift) & 255u;
}

fn writer_new(request_index: u32) -> Writer {
    return Writer(request_index, 0u, 0u, 0u);
}

fn writer_fail(writer: ptr<function, Writer>, flag: u32) {
    (*writer).flags = (*writer).flags | flag;
}

fn writer_push_byte(writer: ptr<function, Writer>, byte: u32) {
    if ((*writer).flags != 0u) {
        return;
    }

    let capacity = params.response_stride_words * 4u;
    if ((*writer).cursor >= capacity) {
        writer_fail(writer, RESPONSE_FLAG_OUTPUT_OVERFLOW);
        return;
    }

    let absolute_index = (*writer).request_index * capacity + (*writer).cursor;
    let word_index = absolute_index / 4u;
    let shift = (absolute_index & 3u) * 8u;
    let mask = 255u << shift;
    output_words[word_index] =
        (output_words[word_index] & ~mask) | ((byte & 255u) << shift);
    (*writer).cursor = (*writer).cursor + 1u;
}

fn writer_push_string(writer: ptr<function, Writer>, string_id: u32) {
    let byte_len = string_meta[string_id].byte_len;
    for (var byte_index = 0u; byte_index < byte_len; byte_index = byte_index + 1u) {
        writer_push_byte(writer, string_byte(string_id, byte_index));
    }
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

fn writer_push_code_point(writer: ptr<function, Writer>, code_point: u32) {
    if (code_point <= 0x7fu) {
        writer_push_byte(writer, code_point);
        return;
    }

    if (code_point <= 0x7ffu) {
        writer_push_byte(writer, 0xc0u | (code_point >> 6u));
        writer_push_byte(writer, 0x80u | (code_point & 0x3fu));
        return;
    }

    if (code_point >= 0xd800u && code_point <= 0xdfffu) {
        writer_fail(writer, RESPONSE_FLAG_INVALID_UTF8);
        return;
    }

    if (code_point <= 0xffffu) {
        writer_push_byte(writer, 0xe0u | (code_point >> 12u));
        writer_push_byte(writer, 0x80u | ((code_point >> 6u) & 0x3fu));
        writer_push_byte(writer, 0x80u | (code_point & 0x3fu));
        return;
    }

    if (code_point <= 0x10ffffu) {
        writer_push_byte(writer, 0xf0u | (code_point >> 18u));
        writer_push_byte(writer, 0x80u | ((code_point >> 12u) & 0x3fu));
        writer_push_byte(writer, 0x80u | ((code_point >> 6u) & 0x3fu));
        writer_push_byte(writer, 0x80u | (code_point & 0x3fu));
        return;
    }

    writer_fail(writer, RESPONSE_FLAG_INVALID_UTF8);
}

fn invalid_utf8_scalar() -> Utf8Scalar {
    return Utf8Scalar(0xfffdu, 1u, 0u, 0u);
}

fn is_utf8_continuation(byte: u32) -> bool {
    return (byte & 0xc0u) == 0x80u;
}

fn decode_utf8_string(string_id: u32, byte_index: u32) -> Utf8Scalar {
    let byte_len = string_meta[string_id].byte_len;
    if (byte_index >= byte_len) {
        return invalid_utf8_scalar();
    }

    let byte_0 = string_byte(string_id, byte_index);
    if (byte_0 <= 0x7fu) {
        return Utf8Scalar(byte_0, 1u, 1u, 0u);
    }

    if (byte_0 >= 0xc2u && byte_0 <= 0xdfu) {
        if (byte_index + 1u >= byte_len) {
            return invalid_utf8_scalar();
        }
        let byte_1 = string_byte(string_id, byte_index + 1u);
        if (!is_utf8_continuation(byte_1)) {
            return invalid_utf8_scalar();
        }
        let code_point = ((byte_0 & 0x1fu) << 6u) | (byte_1 & 0x3fu);
        return Utf8Scalar(code_point, 2u, 1u, 0u);
    }

    if (byte_0 >= 0xe0u && byte_0 <= 0xefu) {
        if (byte_index + 2u >= byte_len) {
            return invalid_utf8_scalar();
        }
        let byte_1 = string_byte(string_id, byte_index + 1u);
        let byte_2 = string_byte(string_id, byte_index + 2u);
        if (!is_utf8_continuation(byte_2)) {
            return invalid_utf8_scalar();
        }

        let second_is_valid =
            (byte_0 == 0xe0u && byte_1 >= 0xa0u && byte_1 <= 0xbfu)
            || (byte_0 == 0xedu && byte_1 >= 0x80u && byte_1 <= 0x9fu)
            || (
                byte_0 != 0xe0u
                && byte_0 != 0xedu
                && is_utf8_continuation(byte_1)
            );
        if (!second_is_valid) {
            return invalid_utf8_scalar();
        }

        let code_point = ((byte_0 & 0x0fu) << 12u)
            | ((byte_1 & 0x3fu) << 6u)
            | (byte_2 & 0x3fu);
        return Utf8Scalar(code_point, 3u, 1u, 0u);
    }

    if (byte_0 >= 0xf0u && byte_0 <= 0xf4u) {
        if (byte_index + 3u >= byte_len) {
            return invalid_utf8_scalar();
        }
        let byte_1 = string_byte(string_id, byte_index + 1u);
        let byte_2 = string_byte(string_id, byte_index + 2u);
        let byte_3 = string_byte(string_id, byte_index + 3u);
        if (!is_utf8_continuation(byte_2) || !is_utf8_continuation(byte_3)) {
            return invalid_utf8_scalar();
        }

        let second_is_valid =
            (byte_0 == 0xf0u && byte_1 >= 0x90u && byte_1 <= 0xbfu)
            || (byte_0 == 0xf4u && byte_1 >= 0x80u && byte_1 <= 0x8fu)
            || (byte_0 >= 0xf1u && byte_0 <= 0xf3u && is_utf8_continuation(byte_1));
        if (!second_is_valid) {
            return invalid_utf8_scalar();
        }

        let code_point = ((byte_0 & 0x07u) << 18u)
            | ((byte_1 & 0x3fu) << 12u)
            | ((byte_2 & 0x3fu) << 6u)
            | (byte_3 & 0x3fu);
        return Utf8Scalar(code_point, 4u, 1u, 0u);
    }

    return invalid_utf8_scalar();
}

fn writer_push_utf8_string(writer: ptr<function, Writer>, string_id: u32) {
    let string_info = string_meta[string_id];
    var byte_index = 0u;
    var scalar_count = 0u;

    loop {
        if (byte_index >= string_info.byte_len) {
            break;
        }

        let scalar = decode_utf8_string(string_id, byte_index);
        if (scalar.valid == 0u) {
            writer_fail(writer, RESPONSE_FLAG_INVALID_UTF8);
            return;
        }

        writer_push_code_point(writer, scalar.code_point);
        byte_index = byte_index + scalar.byte_width;
        scalar_count = scalar_count + 1u;
    }

    if (scalar_count != string_info.scalar_len) {
        writer_fail(writer, RESPONSE_FLAG_INVALID_UTF8);
    }
}

fn writer_finish(writer: Writer, status: u32) {
    if (writer.flags != 0u) {
        response_meta[writer.request_index] = ResponseMeta(0u, 500u, writer.flags, 0u);
        return;
    }

    response_meta[writer.request_index] = ResponseMeta(writer.cursor, status, 0u, 0u);
}

fn is_get_request(request_index: u32, input_len: u32) -> bool {
    if (input_len < 4u) {
        return false;
    }

    return request_byte(request_index, 0u) == 71u
        && request_byte(request_index, 1u) == 69u
        && request_byte(request_index, 2u) == 84u
        && request_byte(request_index, 3u) == 32u;
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

fn response_status_string(response_id: u32) -> u32 {
    switch response_id {
        case RESPONSE_ROOT_ID, RESPONSE_HEALTH_ID, RESPONSE_HELLO_ID, RESPONSE_UTF8_ID: {
            return STRING_STATUS_OK;
        }
        case RESPONSE_BAD_REQUEST_ID: { return STRING_STATUS_BAD_REQUEST; }
        case RESPONSE_METHOD_NOT_ALLOWED_ID: { return STRING_STATUS_METHOD_NOT_ALLOWED; }
        case RESPONSE_NOT_FOUND_ID: { return STRING_STATUS_NOT_FOUND; }
        default: { return STRING_STATUS_NOT_FOUND; }
    }
}

fn response_status(response_id: u32) -> u32 {
    switch response_id {
        case RESPONSE_ROOT_ID, RESPONSE_HEALTH_ID, RESPONSE_HELLO_ID, RESPONSE_UTF8_ID: {
            return 200u;
        }
        case RESPONSE_BAD_REQUEST_ID: { return 400u; }
        case RESPONSE_METHOD_NOT_ALLOWED_ID: { return 405u; }
        case RESPONSE_NOT_FOUND_ID: { return 404u; }
        default: { return 404u; }
    }
}

fn response_content_type(response_id: u32) -> u32 {
    if (response_id == RESPONSE_ROOT_ID) {
        return STRING_CONTENT_TYPE_JSON;
    }
    return STRING_CONTENT_TYPE_TEXT;
}

fn response_body(response_id: u32) -> u32 {
    switch response_id {
        case RESPONSE_ROOT_ID: { return STRING_BODY_ROOT; }
        case RESPONSE_HEALTH_ID: { return STRING_BODY_HEALTH; }
        case RESPONSE_HELLO_ID: { return STRING_BODY_HELLO; }
        case RESPONSE_UTF8_ID: { return STRING_BODY_UTF8; }
        case RESPONSE_BAD_REQUEST_ID: { return STRING_BODY_BAD_REQUEST; }
        case RESPONSE_METHOD_NOT_ALLOWED_ID: { return STRING_BODY_METHOD_NOT_ALLOWED; }
        case RESPONSE_NOT_FOUND_ID: { return STRING_BODY_NOT_FOUND; }
        default: { return STRING_BODY_NOT_FOUND; }
    }
}

fn write_response(request_index: u32, response_id: u32) {
    let status_string_id = response_status_string(response_id);
    let content_type_id = response_content_type(response_id);
    let body_id = response_body(response_id);
    let body_len = string_meta[body_id].byte_len;
    var writer = writer_new(request_index);

    writer_push_string(&writer, STRING_HTTP_VERSION);
    writer_push_string(&writer, status_string_id);
    writer_push_string(&writer, STRING_HEADER_CONTENT_TYPE);
    writer_push_string(&writer, content_type_id);
    writer_push_string(&writer, STRING_HEADER_CONTENT_LENGTH);
    writer_push_decimal(&writer, body_len);
    writer_push_string(&writer, STRING_HEADER_TAIL);

    if (response_id == RESPONSE_UTF8_ID) {
        writer_push_utf8_string(&writer, body_id);
    } else {
        writer_push_string(&writer, body_id);
    }

    writer_finish(writer, response_status(response_id));
}

@compute @workgroup_size(64)
fn process_requests(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let request_index = global_id.x;
    if (request_index >= params.request_count) {
        return;
    }

    let input_len = request_meta[request_index].input_len;
    if (input_len < 4u) {
        write_response(request_index, RESPONSE_BAD_REQUEST_ID);
        return;
    }

    if (!is_get_request(request_index, input_len)) {
        write_response(request_index, RESPONSE_METHOD_NOT_ALLOWED_ID);
        return;
    }

    if (input_len <= 4u || request_byte(request_index, 4u) != 47u) {
        write_response(request_index, RESPONSE_BAD_REQUEST_ID);
        return;
    }

    var cursor = 4u;
    var path_hash = FNV_OFFSET_BASIS;
    var path_len = 0u;
    var hashing_path = true;
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

        if (hashing_path) {
            if (byte == 63u) {
                hashing_path = false;
            } else {
                path_hash = (path_hash ^ byte) * FNV_PRIME;
                path_len = path_len + 1u;
            }
        }

        cursor = cursor + 1u;
    }

    if (!found_target_end || path_len == 0u) {
        write_response(request_index, RESPONSE_BAD_REQUEST_ID);
        return;
    }

    let version_start = cursor + 1u;
    if (!has_supported_http_version(request_index, version_start, input_len)) {
        write_response(request_index, RESPONSE_BAD_REQUEST_ID);
        return;
    }

    var response_id = RESPONSE_NOT_FOUND_ID;
    if (path_len == 1u && path_hash == ROOT_PATH_HASH) {
        response_id = RESPONSE_ROOT_ID;
    } else if (path_len == 7u && path_hash == HEALTH_PATH_HASH) {
        response_id = RESPONSE_HEALTH_ID;
    } else if (path_len == 6u && path_hash == HELLO_PATH_HASH) {
        response_id = RESPONSE_HELLO_ID;
    } else if (path_len == 5u && path_hash == UTF8_PATH_HASH) {
        response_id = RESPONSE_UTF8_ID;
    }

    write_response(request_index, response_id);
}
