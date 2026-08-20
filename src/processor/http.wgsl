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

const FNV_OFFSET_BASIS: u32 = 2166136261u;
const FNV_PRIME: u32 = 16777619u;

const ROOT_PATH_HASH: u32 = 705468254u;
const HEALTH_PATH_HASH: u32 = 1923151932u;
const HELLO_PATH_HASH: u32 = 4088401502u;

const RESPONSE_ROOT_ID: u32 = 0u;
const RESPONSE_HEALTH_ID: u32 = 1u;
const RESPONSE_HELLO_ID: u32 = 2u;
const RESPONSE_BAD_REQUEST_ID: u32 = 3u;
const RESPONSE_METHOD_NOT_ALLOWED_ID: u32 = 4u;
const RESPONSE_NOT_FOUND_ID: u32 = 5u;

const RESPONSE_ROOT_BYTE_LEN: u32 = 209u;
const RESPONSE_ROOT_WORD_LEN: u32 = 53u;
const RESPONSE_ROOT: array<u32, 53> = array<u32, 53>(
    0x50545448u,
    0x312e312fu,
    0x30303220u,
    0x0d4b4f20u,
    0x6e6f430au,
    0x746e6574u,
    0x7079542du,
    0x61203a65u,
    0x696c7070u,
    0x69746163u,
    0x6a2f6e6fu,
    0x0d6e6f73u,
    0x6e6f430au,
    0x746e6574u,
    0x6e654c2du,
    0x3a687467u,
    0x0d343820u,
    0x6e6f430au,
    0x7463656eu,
    0x3a6e6f69u,
    0x6f6c6320u,
    0x0a0d6573u,
    0x76726553u,
    0x203a7265u,
    0x74757067u,
    0x2d580a0du,
    0x74757047u,
    0x6361422du,
    0x646e656bu,
    0x7067203au,
    0x0d0a0d75u,
    0x6e227b0au,
    0x22656d61u,
    0x7067223au,
    0x2c227475u,
    0x63616222u,
    0x646e656bu,
    0x67223a22u,
    0x2c227570u,
    0x73656d22u,
    0x65676173u,
    0x47223a22u,
    0x64205445u,
    0x61707369u,
    0x65686374u,
    0x68742064u,
    0x67756f72u,
    0x20612068u,
    0x706d6f63u,
    0x20657475u,
    0x64616873u,
    0x7d227265u,
    0x0000000au,
);

fn copy_root(output_base: u32) {
    for (var word_index = 0u; word_index < RESPONSE_ROOT_WORD_LEN; word_index = word_index + 1u) {
        output_words[output_base + word_index] = RESPONSE_ROOT[word_index];
    }
}

const RESPONSE_HEALTH_BYTE_LEN: u32 = 136u;
const RESPONSE_HEALTH_WORD_LEN: u32 = 34u;
const RESPONSE_HEALTH: array<u32, 34> = array<u32, 34>(
    0x50545448u,
    0x312e312fu,
    0x30303220u,
    0x0d4b4f20u,
    0x6e6f430au,
    0x746e6574u,
    0x7079542du,
    0x74203a65u,
    0x2f747865u,
    0x69616c70u,
    0x63203b6eu,
    0x73726168u,
    0x753d7465u,
    0x382d6674u,
    0x6f430a0du,
    0x6e65746eu,
    0x654c2d74u,
    0x6874676eu,
    0x0d33203au,
    0x6e6f430au,
    0x7463656eu,
    0x3a6e6f69u,
    0x6f6c6320u,
    0x0a0d6573u,
    0x76726553u,
    0x203a7265u,
    0x74757067u,
    0x2d580a0du,
    0x74757047u,
    0x6361422du,
    0x646e656bu,
    0x7067203au,
    0x0d0a0d75u,
    0x0a6b6f0au,
);

fn copy_health(output_base: u32) {
    for (var word_index = 0u; word_index < RESPONSE_HEALTH_WORD_LEN; word_index = word_index + 1u) {
        output_words[output_base + word_index] = RESPONSE_HEALTH[word_index];
    }
}

const RESPONSE_HELLO_BYTE_LEN: u32 = 162u;
const RESPONSE_HELLO_WORD_LEN: u32 = 41u;
const RESPONSE_HELLO: array<u32, 41> = array<u32, 41>(
    0x50545448u,
    0x312e312fu,
    0x30303220u,
    0x0d4b4f20u,
    0x6e6f430au,
    0x746e6574u,
    0x7079542du,
    0x74203a65u,
    0x2f747865u,
    0x69616c70u,
    0x63203b6eu,
    0x73726168u,
    0x753d7465u,
    0x382d6674u,
    0x6f430a0du,
    0x6e65746eu,
    0x654c2d74u,
    0x6874676eu,
    0x3832203au,
    0x6f430a0du,
    0x63656e6eu,
    0x6e6f6974u,
    0x6c63203au,
    0x0d65736fu,
    0x7265530au,
    0x3a726576u,
    0x75706720u,
    0x580a0d74u,
    0x7570472du,
    0x61422d74u,
    0x6e656b63u,
    0x67203a64u,
    0x0a0d7570u,
    0x65680a0du,
    0x206f6c6cu,
    0x6d6f7266u,
    0x63206120u,
    0x75706d6fu,
    0x73206574u,
    0x65646168u,
    0x00000a72u,
);

fn copy_hello(output_base: u32) {
    for (var word_index = 0u; word_index < RESPONSE_HELLO_WORD_LEN; word_index = word_index + 1u) {
        output_words[output_base + word_index] = RESPONSE_HELLO[word_index];
    }
}

const RESPONSE_BAD_REQUEST_BYTE_LEN: u32 = 155u;
const RESPONSE_BAD_REQUEST_WORD_LEN: u32 = 39u;
const RESPONSE_BAD_REQUEST: array<u32, 39> = array<u32, 39>(
    0x50545448u,
    0x312e312fu,
    0x30303420u,
    0x64614220u,
    0x71655220u,
    0x74736575u,
    0x6f430a0du,
    0x6e65746eu,
    0x79542d74u,
    0x203a6570u,
    0x74786574u,
    0x616c702fu,
    0x203b6e69u,
    0x72616863u,
    0x3d746573u,
    0x2d667475u,
    0x430a0d38u,
    0x65746e6fu,
    0x4c2d746eu,
    0x74676e65u,
    0x31203a68u,
    0x430a0d32u,
    0x656e6e6fu,
    0x6f697463u,
    0x63203a6eu,
    0x65736f6cu,
    0x65530a0du,
    0x72657672u,
    0x7067203au,
    0x0a0d7475u,
    0x70472d58u,
    0x422d7475u,
    0x656b6361u,
    0x203a646eu,
    0x0d757067u,
    0x620a0d0au,
    0x72206461u,
    0x65757165u,
    0x000a7473u,
);

fn copy_bad_request(output_base: u32) {
    for (var word_index = 0u; word_index < RESPONSE_BAD_REQUEST_WORD_LEN; word_index = word_index + 1u) {
        output_words[output_base + word_index] = RESPONSE_BAD_REQUEST[word_index];
    }
}

const RESPONSE_METHOD_NOT_ALLOWED_BYTE_LEN: u32 = 169u;
const RESPONSE_METHOD_NOT_ALLOWED_WORD_LEN: u32 = 43u;
const RESPONSE_METHOD_NOT_ALLOWED: array<u32, 43> = array<u32, 43>(
    0x50545448u,
    0x312e312fu,
    0x35303420u,
    0x74654d20u,
    0x20646f68u,
    0x20746f4eu,
    0x6f6c6c41u,
    0x0d646577u,
    0x6e6f430au,
    0x746e6574u,
    0x7079542du,
    0x74203a65u,
    0x2f747865u,
    0x69616c70u,
    0x63203b6eu,
    0x73726168u,
    0x753d7465u,
    0x382d6674u,
    0x6f430a0du,
    0x6e65746eu,
    0x654c2d74u,
    0x6874676eu,
    0x3931203au,
    0x6f430a0du,
    0x63656e6eu,
    0x6e6f6974u,
    0x6c63203au,
    0x0d65736fu,
    0x7265530au,
    0x3a726576u,
    0x75706720u,
    0x580a0d74u,
    0x7570472du,
    0x61422d74u,
    0x6e656b63u,
    0x67203a64u,
    0x0a0d7570u,
    0x656d0a0du,
    0x646f6874u,
    0x746f6e20u,
    0x6c6c6120u,
    0x6465776fu,
    0x0000000au,
);

fn copy_method_not_allowed(output_base: u32) {
    for (var word_index = 0u; word_index < RESPONSE_METHOD_NOT_ALLOWED_WORD_LEN; word_index = word_index + 1u) {
        output_words[output_base + word_index] = RESPONSE_METHOD_NOT_ALLOWED[word_index];
    }
}

const RESPONSE_NOT_FOUND_BYTE_LEN: u32 = 151u;
const RESPONSE_NOT_FOUND_WORD_LEN: u32 = 38u;
const RESPONSE_NOT_FOUND: array<u32, 38> = array<u32, 38>(
    0x50545448u,
    0x312e312fu,
    0x34303420u,
    0x746f4e20u,
    0x756f4620u,
    0x0a0d646eu,
    0x746e6f43u,
    0x2d746e65u,
    0x65707954u,
    0x6574203au,
    0x702f7478u,
    0x6e69616cu,
    0x6863203bu,
    0x65737261u,
    0x74753d74u,
    0x0d382d66u,
    0x6e6f430au,
    0x746e6574u,
    0x6e654c2du,
    0x3a687467u,
    0x0d303120u,
    0x6e6f430au,
    0x7463656eu,
    0x3a6e6f69u,
    0x6f6c6320u,
    0x0a0d6573u,
    0x76726553u,
    0x203a7265u,
    0x74757067u,
    0x2d580a0du,
    0x74757047u,
    0x6361422du,
    0x646e656bu,
    0x7067203au,
    0x0d0a0d75u,
    0x746f6e0au,
    0x756f6620u,
    0x000a646eu,
);

fn copy_not_found(output_base: u32) {
    for (var word_index = 0u; word_index < RESPONSE_NOT_FOUND_WORD_LEN; word_index = word_index + 1u) {
        output_words[output_base + word_index] = RESPONSE_NOT_FOUND[word_index];
    }
}

fn request_byte(request_index: u32, byte_index: u32) -> u32 {
    let word_index = request_index * params.request_stride_words + byte_index / 4u;
    let shift = (byte_index & 3u) * 8u;
    return (input_words[word_index] >> shift) & 255u;
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

    let terminator = request_byte(request_index, version_start + 8u);
    return version_matches && (terminator == 13u || terminator == 10u);
}

fn response_byte_len(response_id: u32) -> u32 {
    switch response_id {
        case RESPONSE_ROOT_ID: { return RESPONSE_ROOT_BYTE_LEN; }
        case RESPONSE_HEALTH_ID: { return RESPONSE_HEALTH_BYTE_LEN; }
        case RESPONSE_HELLO_ID: { return RESPONSE_HELLO_BYTE_LEN; }
        case RESPONSE_BAD_REQUEST_ID: { return RESPONSE_BAD_REQUEST_BYTE_LEN; }
        case RESPONSE_METHOD_NOT_ALLOWED_ID: { return RESPONSE_METHOD_NOT_ALLOWED_BYTE_LEN; }
        case RESPONSE_NOT_FOUND_ID: { return RESPONSE_NOT_FOUND_BYTE_LEN; }
        default: { return RESPONSE_NOT_FOUND_BYTE_LEN; }
    }
}

fn response_word_len(response_id: u32) -> u32 {
    switch response_id {
        case RESPONSE_ROOT_ID: { return RESPONSE_ROOT_WORD_LEN; }
        case RESPONSE_HEALTH_ID: { return RESPONSE_HEALTH_WORD_LEN; }
        case RESPONSE_HELLO_ID: { return RESPONSE_HELLO_WORD_LEN; }
        case RESPONSE_BAD_REQUEST_ID: { return RESPONSE_BAD_REQUEST_WORD_LEN; }
        case RESPONSE_METHOD_NOT_ALLOWED_ID: { return RESPONSE_METHOD_NOT_ALLOWED_WORD_LEN; }
        case RESPONSE_NOT_FOUND_ID: { return RESPONSE_NOT_FOUND_WORD_LEN; }
        default: { return RESPONSE_NOT_FOUND_WORD_LEN; }
    }
}

fn response_status(response_id: u32) -> u32 {
    switch response_id {
        case RESPONSE_ROOT_ID: { return 200u; }
        case RESPONSE_HEALTH_ID: { return 200u; }
        case RESPONSE_HELLO_ID: { return 200u; }
        case RESPONSE_BAD_REQUEST_ID: { return 400u; }
        case RESPONSE_METHOD_NOT_ALLOWED_ID: { return 405u; }
        case RESPONSE_NOT_FOUND_ID: { return 404u; }
        default: { return 404u; }
    }
}

fn write_response(request_index: u32, response_id: u32) {
    let output_base = request_index * params.response_stride_words;
    let word_len = response_word_len(response_id);

    if (word_len > params.response_stride_words) {
        response_meta[request_index] = ResponseMeta(0u, 500u, 1u, 0u);
        return;
    }

    switch response_id {
        case RESPONSE_ROOT_ID: { copy_root(output_base); }
        case RESPONSE_HEALTH_ID: { copy_health(output_base); }
        case RESPONSE_HELLO_ID: { copy_hello(output_base); }
        case RESPONSE_BAD_REQUEST_ID: { copy_bad_request(output_base); }
        case RESPONSE_METHOD_NOT_ALLOWED_ID: { copy_method_not_allowed(output_base); }
        case RESPONSE_NOT_FOUND_ID: { copy_not_found(output_base); }
        default: { copy_not_found(output_base); }
    }

    response_meta[request_index] = ResponseMeta(
        response_byte_len(response_id),
        response_status(response_id),
        0u,
        0u,
    );
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
    }

    write_response(request_index, response_id);
}
