use super::{
    AccountUsagePayload, grpc_web_status, parse_openai_usage, parse_xai_usage, safe_format_key,
    unix_now,
};
use serde_json::json;

#[test]
fn formats_long_key() {
    let key = "sk-proj-1234567890ABCDE";
    assert_eq!(safe_format_key(key), "sk-proj-***ABCDE");
}

#[test]
fn short_key_returns_stars() {
    let key = "sk-proj-12345";
    assert_eq!(safe_format_key(key), "***");
}

#[test]
fn parses_openai_subscription_windows() {
    let snapshot = parse_openai_usage(&AccountUsagePayload::Json(json!({
        "plan_type": "pro",
        "rate_limit": {
            "primary_window": { "used_percent": 80.0, "reset_at": 1_800_000_000 },
            "secondary_window": { "used_percent": 25.5, "reset_at": 1_800_100_000 }
        }
    })))
    .expect("usage should parse");

    assert_eq!(snapshot.provider, "openai");
    assert_eq!(snapshot.plan.as_deref(), Some("pro"));
    assert_eq!(snapshot.windows.len(), 2);
    assert_eq!(snapshot.windows[0].id, "session");
    assert_eq!(snapshot.windows[0].used_percent, 80.0);
    assert_eq!(snapshot.windows[1].id, "weekly");
    assert_eq!(snapshot.windows[1].used_percent, 25.5);
}

#[test]
fn parses_xai_subscription_percentage_and_reset() {
    let reset_at = unix_now() + 3_600;
    let mut protobuf = vec![0x0d];
    protobuf.extend_from_slice(&100.0_f32.to_bits().to_le_bytes());
    protobuf.push(0x10);
    append_varint(&mut protobuf, reset_at as u64);

    let mut body = vec![0];
    body.extend_from_slice(&(protobuf.len() as u32).to_be_bytes());
    body.extend_from_slice(&protobuf);
    let snapshot = parse_xai_usage(&AccountUsagePayload::Bytes(body)).expect("usage should parse");

    assert_eq!(snapshot.provider, "xai");
    assert_eq!(snapshot.windows[0].used_percent, 100.0);
    assert_eq!(snapshot.windows[0].resets_at, Some(reset_at));
}

#[test]
fn parses_omitted_xai_percentage_as_zero_usage() {
    let reset_at = unix_now() + 3_600;
    let mut timestamp = vec![0x08];
    append_varint(&mut timestamp, reset_at as u64);
    let mut protobuf = vec![
        0x0a,
        (timestamp.len() + 2) as u8,
        0x2a,
        timestamp.len() as u8,
    ];
    protobuf.extend_from_slice(&timestamp);

    let mut body = vec![0];
    body.extend_from_slice(&(protobuf.len() as u32).to_be_bytes());
    body.extend_from_slice(&protobuf);
    let snapshot = parse_xai_usage(&AccountUsagePayload::Bytes(body)).expect("usage should parse");

    assert_eq!(snapshot.provider, "xai");
    assert_eq!(snapshot.windows[0].used_percent, 0.0);
    assert_eq!(snapshot.windows[0].resets_at, Some(reset_at));
}

#[test]
fn reads_grpc_status_from_trailer_frame() {
    let trailers = b"grpc-status: 16\r\ngrpc-message: unauthenticated\r\n";
    let mut body = vec![0x80];
    body.extend_from_slice(&(trailers.len() as u32).to_be_bytes());
    body.extend_from_slice(trailers);

    assert_eq!(grpc_web_status(&body), Some(16));
}

fn append_varint(bytes: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            break;
        }
    }
}
