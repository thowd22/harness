//! Wire-format compatibility across dependency upgrades.
//!
//! Helpers already deployed on remote hosts keep running the zstd and
//! base64 crates they were built with, so a client built with newer crates
//! must still read what they send and send what they can read. The golden
//! frames below were produced by the helper protocol encoder built with
//! zstd 0.13.3 (libzstd 1.5.7) and base64 0.22.1; they must keep decoding,
//! and the current encoder must keep producing frames of the same shape.

use harness_remote::protocol::{
    DEFAULT_COMPRESS_MIN, DEFAULT_ZSTD_LEVEL, MAX_FRAME, Message, b64, methods,
};
use harness_remote::wire::{Encoder, Frame, Framing, WireFrame, Zstd, read_wire_frame};
use serde_json::{Value, json};
use tokio::io::BufReader;

/// Protocol 2 zstd frame for [`message`] carrying [`text_payload`].
const GOLDEN_ZSTD_FRAME: &str = "7e3639203630303020313135207a7374640a28b52ffd60b5164d0300d286171b\
    70697526285b323611523786629faa6a710e021bd2002756cf1a5e454cc27145b546e469a576b90160f5986\
    92c611e063368251b6214e80feb7e9eae2f743ad4bb851a623f7f0d7b4d23c926f67aa597b7ebe70164707d\
    c21ab74b870101006dd1ad5e4501";

/// Protocol 1 line for [`message`] carrying every byte value once.
const GOLDEN_LINE: &str = concat!(
    r#"{"jsonrpc":"2.0","method":"exec.output","params":{"data":""#,
    "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+P0BB",
    "QkNERUZHSElKS0xNTk9QUVJTVFVWV1hZWltcXV5fYGFiY2RlZmdoaWprbG1ub3BxcnN0dXZ3eHl6e3x9fn+AgYKD",
    "hIWGh4iJiouMjY6PkJGSk5SVlpeYmZqbnJ2en6ChoqOkpaanqKmqq6ytrq+wsbKztLW2t7i5uru8vb6/wMHCw8TF",
    "xsfIycrLzM3Oz9DR0tPU1dbX2Nna29zd3t/g4eLj5OXm5+jp6uvs7e7v8PHy8/T19vf4+fr7/P3+/w==",
    r#"","job":"j","seq":1}}"#,
    "\n"
);

/// zstd frame magic number (RFC 8878, little endian 0xFD2FB528).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

fn message() -> Message {
    Message::notification(methods::EXEC_OUTPUT, json!({"job": "j", "seq": 1}))
}

fn text_payload() -> Vec<u8> {
    let line = b"fn main() { println!(\"hello, world\"); }\n";
    line.iter().copied().cycle().take(6000).collect()
}

fn all_bytes() -> Vec<u8> {
    (0..=255u8).collect()
}

fn zstd_framing() -> Framing {
    Framing::Binary {
        zstd: Some(Zstd {
            level: DEFAULT_ZSTD_LEVEL,
            min_size: DEFAULT_COMPRESS_MIN,
        }),
    }
}

fn unhex(s: &str) -> Vec<u8> {
    let s: String = s.split_whitespace().collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

async fn decode(bytes: &[u8]) -> WireFrame {
    let mut r = BufReader::new(bytes);
    read_wire_frame(&mut r, MAX_FRAME).await.unwrap().unwrap()
}

/// Splits a binary frame into its header line and body.
fn split(frame: &[u8]) -> (String, &[u8]) {
    let nl = frame.iter().position(|&b| b == b'\n').unwrap();
    (
        String::from_utf8(frame[..nl].to_vec()).unwrap(),
        &frame[nl + 1..],
    )
}

#[tokio::test]
async fn zstd_frames_from_older_helpers_still_decode() {
    let back = decode(&unhex(GOLDEN_ZSTD_FRAME)).await;
    assert_eq!(back.data, Some(text_payload()));
    assert_eq!(Message::parse(&back.json).unwrap(), message());
}

#[tokio::test]
async fn zstd_frames_keep_the_shape_older_helpers_read() {
    let out = Encoder::new(zstd_framing())
        .encode(&Frame::with_data(message(), text_payload()))
        .unwrap();
    let golden = unhex(GOLDEN_ZSTD_FRAME);
    let (header, body) = split(&out);
    let (golden_header, golden_body) = split(&golden);

    // Same header fields; the wire length may differ only if libzstd's
    // output does, and it must still match the body.
    let fields: Vec<&str> = header.split(' ').collect();
    let golden_fields: Vec<&str> = golden_header.split(' ').collect();
    assert_eq!(fields.len(), 4, "{header}");
    assert_eq!(fields[..2], golden_fields[..2], "{header}");
    assert_eq!(fields[3], "zstd", "{header}");
    assert_eq!(fields[2].parse::<usize>().unwrap(), body.len(), "{header}");

    // One standard zstd frame (no skippable or magicless frames, no
    // dictionary) that a plain one-shot decompress bounded by the declared
    // size turns back into the body. That is exactly what older helpers do.
    assert_eq!(body[..4], ZSTD_MAGIC);
    assert_eq!(golden_body[..4], ZSTD_MAGIC);
    let descriptor = body[4];
    assert_eq!(descriptor & 0b11, 0, "frame must not need a dictionary");
    assert_eq!(descriptor & 0b1000, 0, "reserved bit set");
    let raw_len = message().to_line().len() + text_payload().len();
    let raw = zstd::bulk::decompress(body, raw_len).unwrap();
    assert_eq!(raw.len(), raw_len);
    assert_eq!(&raw[raw_len - text_payload().len()..], &text_payload()[..]);
}

#[tokio::test]
async fn base64_lines_from_older_helpers_still_decode() {
    let back = decode(GOLDEN_LINE.as_bytes()).await;
    assert_eq!(back.data, None);
    let v: Value = serde_json::from_str(&back.json).unwrap();
    let data = v["params"]["data"].as_str().unwrap();
    // Decode through the protocol's serde helper, as message types do.
    let bytes = b64::deserialize(Value::String(data.into())).unwrap();
    assert_eq!(bytes, all_bytes());
}

#[test]
fn base64_lines_are_unchanged() {
    let out = Encoder::lines()
        .encode(&Frame::with_data(message(), all_bytes()))
        .unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), GOLDEN_LINE);
}
