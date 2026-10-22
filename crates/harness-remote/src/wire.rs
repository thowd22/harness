//! Framing of the helper protocol on the byte stream.
//!
//! **Protocol 1** sends every message as one JSON line, with binary
//! payloads (file contents, process output) base64-encoded in a `data`
//! field.
//!
//! **Protocol 2** keeps messages without a binary payload and smaller than
//! the compression threshold as plain JSON lines, byte for byte the same as
//! protocol 1. Every other message is a *binary frame*: one ASCII header
//! line followed by a length-prefixed body.
//!
//! ```text
//! ~<json_len> <data_len|-> <wire_len> <raw|zstd>\n
//! <wire_len bytes of body>
//! ```
//!
//! The decoded body is `json_len` bytes of the JSON-RPC message (without
//! its `data` field) followed by `data_len` bytes of raw payload (`-`: the
//! message has no payload). With `zstd`, the body is one zstd frame that
//! must decompress to exactly `json_len + data_len` bytes; with `raw`,
//! `wire_len` equals that sum. The declared decoded size is checked against
//! the frame limit ([`MAX_FRAME`]) *before* anything is decompressed, and
//! decompression is bounded by it, so a small compressed frame cannot
//! expand into a decompression bomb.
//!
//! The sender compresses a body of at least the negotiated minimum
//! ([`DEFAULT_COMPRESS_MIN`] by default) at the negotiated zstd level
//! ([`DEFAULT_ZSTD_LEVEL`]), and sends it raw when zstd does not make it
//! smaller (already compressed or random data). Compression is only
//! negotiated when the client offers it, which it does not when the SSH
//! channel already compresses.
//!
//! [`read_wire_frame`] decodes both protocols (a protocol 1 line is a valid
//! protocol 2 frame), so a reader never has to switch modes; only the
//! [`Encoder`] changes after the `hello` exchange.

use std::io;

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

#[cfg(doc)]
use crate::protocol::{DEFAULT_COMPRESS_MIN, DEFAULT_ZSTD_LEVEL};
use crate::protocol::{MAX_FRAME, Message, b64, read_line_limited};

/// First byte of a protocol 2 binary frame header.
pub const BINARY_MARK: u8 = b'~';

/// Longest accepted binary frame header line.
const MAX_HEADER: usize = 128;

/// A message plus its out-of-band binary payload.
///
/// The payload is the message's logical `data` field (file contents for
/// `fs.read` results and `fs.write`/`fs.replace_atomic` requests, bytes of
/// `exec.output` notifications). Protocol 1 encodes it as base64 inside the
/// JSON; protocol 2 sends it raw after the JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// The JSON-RPC message, without the payload.
    pub message: Message,
    /// The payload, if the message has one.
    pub data: Option<Vec<u8>>,
}

impl Frame {
    /// A frame without payload.
    pub fn new(message: Message) -> Self {
        Self {
            message,
            data: None,
        }
    }

    /// A frame carrying `data`.
    pub fn with_data(message: Message, data: Vec<u8>) -> Self {
        Self {
            message,
            data: Some(data),
        }
    }

    /// Approximate encoded size, for buffer accounting.
    pub fn approx_len(&self) -> usize {
        128 + self.data.as_ref().map_or(0, Vec::len)
    }
}

/// A frame as read from the stream: the JSON text (not yet parsed, so a
/// bad message can be reported and skipped) and the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireFrame {
    /// The JSON-RPC message text.
    pub json: String,
    /// Binary payload of a protocol 2 frame (`None` for JSON lines and
    /// frames declaring no payload).
    pub data: Option<Vec<u8>>,
}

/// zstd settings of a protocol 2 connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zstd {
    /// Compression level.
    pub level: i32,
    /// Bodies smaller than this are sent uncompressed.
    pub min_size: usize,
}

/// How a connection's outgoing frames are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// Protocol 1: JSON lines, payloads as base64.
    Lines,
    /// Protocol 2: binary payloads, optionally zstd-compressed.
    Binary {
        /// Compression, if negotiated.
        zstd: Option<Zstd>,
    },
}

/// Encodes outgoing frames for one connection.
pub struct Encoder {
    framing: Framing,
    compressor: Option<zstd::bulk::Compressor<'static>>,
    max_frame: usize,
}

impl std::fmt::Debug for Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encoder")
            .field("framing", &self.framing)
            .field("max_frame", &self.max_frame)
            .finish_non_exhaustive()
    }
}

impl Encoder {
    /// An encoder for `framing`.
    pub fn new(framing: Framing) -> Self {
        let compressor = match framing {
            Framing::Binary { zstd: Some(z) } => zstd::bulk::Compressor::new(z.level).ok(),
            _ => None,
        };
        Self {
            framing,
            compressor,
            max_frame: MAX_FRAME,
        }
    }

    /// Protocol 1 framing (every connection starts with it).
    pub fn lines() -> Self {
        Self::new(Framing::Lines)
    }

    /// The framing.
    pub fn framing(&self) -> Framing {
        self.framing
    }

    /// Refuse to encode frames larger than `max` (default [`MAX_FRAME`]),
    /// since the peer would reject them.
    pub fn with_max_frame(mut self, max: usize) -> Self {
        self.max_frame = max;
        self
    }

    /// The bytes to write for `frame`.
    pub fn encode(&mut self, frame: &Frame) -> io::Result<Vec<u8>> {
        match self.framing {
            Framing::Lines => self.encode_line(frame),
            Framing::Binary { zstd } => self.encode_binary(frame, zstd),
        }
    }

    fn encode_line(&self, frame: &Frame) -> io::Result<Vec<u8>> {
        let mut v = frame.message.to_value();
        if let Some(data) = &frame.data {
            let key = match &frame.message {
                Message::Response { .. } => "result",
                _ => "params",
            };
            if let Some(obj) = v.get_mut(key) {
                if !obj.is_object() {
                    *obj = Value::Object(Default::default());
                }
                if let Some(o) = obj.as_object_mut() {
                    o.insert("data".into(), Value::String(b64::encode(data)));
                }
            }
        }
        let mut out = v.to_string().into_bytes();
        if out.len() > self.max_frame {
            return Err(too_large());
        }
        out.push(b'\n');
        Ok(out)
    }

    fn encode_binary(&mut self, frame: &Frame, zstd: Option<Zstd>) -> io::Result<Vec<u8>> {
        let json = frame.message.to_line();
        let data_len = frame.data.as_ref().map_or(0, Vec::len);
        let raw_len = json.len() + data_len;
        if raw_len > self.max_frame {
            return Err(too_large());
        }
        let compress = zstd.filter(|z| raw_len >= z.min_size);
        if frame.data.is_none() && compress.is_none() {
            let mut out = json.into_bytes();
            out.push(b'\n');
            return Ok(out);
        }
        let mut body = Vec::with_capacity(raw_len);
        body.extend_from_slice(json.as_bytes());
        if let Some(d) = &frame.data {
            body.extend_from_slice(d);
        }
        let (codec, body) = match (compress, self.compressor.as_mut()) {
            (Some(_), Some(c)) => match c.compress(&body) {
                Ok(z) if z.len() < body.len() => ("zstd", z),
                _ => ("raw", body),
            },
            _ => ("raw", body),
        };
        let data_field = match &frame.data {
            Some(d) => d.len().to_string(),
            None => "-".into(),
        };
        let header = format!(
            "{}{} {} {} {}\n",
            BINARY_MARK as char,
            json.len(),
            data_field,
            body.len(),
            codec
        );
        let mut out = Vec::with_capacity(header.len() + body.len());
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(&body);
        Ok(out)
    }
}

fn too_large() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "protocol frame too large")
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Read one frame of either protocol. `Ok(None)` at end of stream.
///
/// A JSON line longer than `max_frame`, or a binary frame whose declared
/// decoded size or wire size exceeds it, is an error; a zstd body that
/// decompresses to anything but the declared size is an error too.
pub async fn read_wire_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max_frame: usize,
) -> io::Result<Option<WireFrame>> {
    let first = match reader.fill_buf().await?.first() {
        None => return Ok(None),
        Some(b) => *b,
    };
    if first != BINARY_MARK {
        return Ok(read_line_limited(reader, max_frame)
            .await?
            .map(|json| WireFrame { json, data: None }));
    }
    let header = read_line_limited(reader, MAX_HEADER)
        .await
        .map_err(|_| bad("binary frame header too long"))?
        .ok_or_else(|| bad("missing binary frame header"))?;
    let h = Header::parse(&header)?;
    let raw_len = h
        .json_len
        .checked_add(h.data_len.unwrap_or(0))
        .ok_or_else(too_large)?;
    if raw_len > max_frame || h.wire_len > max_frame {
        return Err(too_large());
    }
    if !h.zstd && h.wire_len != raw_len {
        return Err(bad("raw frame length does not match its header"));
    }
    let mut wire = vec![0u8; h.wire_len];
    reader.read_exact(&mut wire).await?;
    let body = if h.zstd {
        // Bounded by the declared size, which was checked above.
        let out = zstd::bulk::decompress(&wire, raw_len)
            .map_err(|e| bad(format!("bad zstd frame: {e}")))?;
        if out.len() != raw_len {
            return Err(bad("zstd frame size does not match its header"));
        }
        out
    } else {
        wire
    };
    let (json, data) = body.split_at(h.json_len);
    let json = String::from_utf8(json.to_vec()).map_err(|_| bad("frame is not UTF-8"))?;
    Ok(Some(WireFrame {
        json,
        data: h.data_len.map(|_| data.to_vec()),
    }))
}

struct Header {
    json_len: usize,
    data_len: Option<usize>,
    wire_len: usize,
    zstd: bool,
}

impl Header {
    fn parse(line: &str) -> io::Result<Header> {
        let rest = line
            .strip_prefix(BINARY_MARK as char)
            .ok_or_else(|| bad("bad binary frame header"))?;
        let parts: Vec<&str> = rest.split(' ').collect();
        let [json_len, data_len, wire_len, codec] = parts[..] else {
            return Err(bad("bad binary frame header"));
        };
        let num = |s: &str| {
            s.parse::<usize>()
                .map_err(|_| bad("bad number in binary frame header"))
        };
        let zstd = match codec {
            "zstd" => true,
            "raw" => false,
            other => return Err(bad(format!("unknown frame codec {other:?}"))),
        };
        Ok(Header {
            json_len: num(json_len)?,
            data_len: if data_len == "-" {
                None
            } else {
                Some(num(data_len)?)
            },
            wire_len: num(wire_len)?,
            zstd,
        })
    }
}

/// An item for a connection's writer task.
#[derive(Debug)]
pub(crate) enum Outbound {
    /// Encode and send a frame.
    Frame(Frame),
    /// Use this encoder for every following frame (sent right after the
    /// `hello` answer, so the switch is ordered with the frames).
    Switch(Encoder),
}

impl From<Frame> for Outbound {
    fn from(f: Frame) -> Self {
        Outbound::Frame(f)
    }
}

/// Write frames from `rx` to `writer` until the channel closes or a write
/// fails. Frames that cannot be encoded are logged and skipped.
pub(crate) async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut rx: mpsc::UnboundedReceiver<Outbound>,
) {
    let mut enc = Encoder::lines();
    while let Some(item) = rx.recv().await {
        let frame = match item {
            Outbound::Switch(e) => {
                enc = e;
                continue;
            }
            Outbound::Frame(f) => f,
        };
        let bytes = match enc.encode(&frame) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(target: "harness::remote", "frame not sent: {e}");
                continue;
            }
        };
        if writer.write_all(&bytes).await.is_err() {
            break;
        }
        if rx.is_empty() && writer.flush().await.is_err() {
            break;
        }
    }
    let _ = writer.flush().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{DEFAULT_COMPRESS_MIN, DEFAULT_ZSTD_LEVEL, methods};
    use serde_json::json;
    use tokio::io::BufReader;

    fn zstd_default() -> Framing {
        Framing::Binary {
            zstd: Some(Zstd {
                level: DEFAULT_ZSTD_LEVEL,
                min_size: DEFAULT_COMPRESS_MIN,
            }),
        }
    }

    fn text(n: usize) -> Vec<u8> {
        let line = b"fn main() { println!(\"hello, world\"); }\n";
        line.iter().copied().cycle().take(n).collect()
    }

    fn note(data: Vec<u8>) -> Frame {
        Frame::with_data(
            Message::notification(methods::EXEC_OUTPUT, json!({"job": "j", "seq": 1})),
            data,
        )
    }

    async fn decode(bytes: &[u8], max: usize) -> io::Result<Option<WireFrame>> {
        let mut r = BufReader::new(bytes);
        read_wire_frame(&mut r, max).await
    }

    fn header(bytes: &[u8]) -> String {
        let end = bytes.iter().position(|&b| b == b'\n').unwrap();
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    }

    #[tokio::test]
    async fn large_frames_are_zstd_compressed_small_ones_are_not() {
        let mut enc = Encoder::new(zstd_default());
        // A body of 4 KiB or more is compressed.
        let big = note(text(DEFAULT_COMPRESS_MIN));
        let out = enc.encode(&big).unwrap();
        assert!(header(&out).ends_with(" zstd"), "{}", header(&out));
        assert!(out.len() < DEFAULT_COMPRESS_MIN / 2, "{} bytes", out.len());
        let back = decode(&out, MAX_FRAME).await.unwrap().unwrap();
        assert_eq!(back.data.as_deref(), big.data.as_deref());
        assert_eq!(Message::parse(&back.json).unwrap(), big.message);

        // A smaller one travels as-is: raw payload bytes, no base64.
        let small = note(text(1000));
        let out = enc.encode(&small).unwrap();
        assert!(header(&out).ends_with(" raw"), "{}", header(&out));
        assert!(
            out.windows(1000)
                .any(|w| w == small.data.as_deref().unwrap())
        );
        let back = decode(&out, MAX_FRAME).await.unwrap().unwrap();
        assert_eq!(back.data, small.data);

        // A small message without payload is a plain JSON line.
        let ping = Frame::new(Message::request(3, methods::PING, json!({})));
        let out = enc.encode(&ping).unwrap();
        assert_eq!(out, format!("{}\n", ping.message.to_line()).into_bytes());
    }

    #[tokio::test]
    async fn large_json_without_payload_is_compressed_too() {
        let mut enc = Encoder::new(zstd_default());
        let paths: Vec<String> = (0..500).map(|i| format!("src/module_{i}.rs")).collect();
        let f = Frame::new(Message::Response {
            id: 9,
            result: Ok(json!({ "paths": paths })),
        });
        let out = enc.encode(&f).unwrap();
        let h = header(&out);
        assert!(h.contains(" - ") && h.ends_with("zstd"), "{h}");
        let back = decode(&out, MAX_FRAME).await.unwrap().unwrap();
        assert_eq!(back.data, None);
        assert_eq!(Message::parse(&back.json).unwrap(), f.message);
    }

    #[tokio::test]
    async fn incompressible_payloads_are_sent_raw() {
        let mut enc = Encoder::new(zstd_default());
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let random: Vec<u8> = (0..64 * 1024)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let out = enc.encode(&note(random.clone())).unwrap();
        assert!(header(&out).ends_with(" raw"));
        let back = decode(&out, MAX_FRAME).await.unwrap().unwrap();
        assert_eq!(back.data.unwrap(), random);
    }

    #[tokio::test]
    async fn no_compression_without_negotiation() {
        let mut enc = Encoder::new(Framing::Binary { zstd: None });
        let out = enc.encode(&note(text(100_000))).unwrap();
        assert!(header(&out).ends_with(" raw"));
        assert_eq!(
            out.len(),
            header(&out).len() + 1 + 100_000 + note(vec![]).message.to_line().len()
        );
    }

    #[tokio::test]
    async fn line_framing_is_protocol_1() {
        let mut enc = Encoder::lines();
        let f = Frame::with_data(
            Message::Response {
                id: 4,
                result: Ok(json!({})),
            },
            vec![0, 159, 146, 150],
        );
        let out = enc.encode(&f).unwrap();
        let line = String::from_utf8(out).unwrap();
        assert!(line.ends_with('\n'));
        let v: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(v["result"]["data"], b64::encode(&[0, 159, 146, 150]));
        // The universal reader reads it, the payload stays in the JSON.
        let back = decode(line.as_bytes(), MAX_FRAME).await.unwrap().unwrap();
        assert_eq!(back.data, None);
        assert_eq!(back.json, line.trim_end());
        // Even a big one is never compressed.
        let out = enc.encode(&note(text(100_000))).unwrap();
        assert_eq!(out[0], b'{');
    }

    #[tokio::test]
    async fn frame_limit_applies_to_the_decompressed_size() {
        // A bomb: 8 MiB of zeros compress to a few hundred bytes.
        let zeros = vec![0u8; 8 << 20];
        let mut enc = Encoder::new(zstd_default());
        let bomb = enc.encode(&note(zeros)).unwrap();
        assert!(bomb.len() < 4096, "{} bytes", bomb.len());
        assert!(decode(&bomb, 16 << 20).await.unwrap().is_some());
        let err = decode(&bomb, 1 << 20).await.unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");

        // A header that understates the decompressed size cannot make the
        // decoder expand past it.
        let body = zstd::bulk::compress(&vec![b'a'; 4 << 20], 1).unwrap();
        let mut lying = format!("~2 100 {} zstd\n", body.len()).into_bytes();
        lying.extend_from_slice(&body);
        let err = decode(&lying, MAX_FRAME).await.unwrap_err();
        assert!(err.to_string().contains("zstd"), "{err}");

        // A declared size above MAX_FRAME is refused before reading the body.
        let claim = format!("~2 {} 10 zstd\n", MAX_FRAME).into_bytes();
        let err = decode(&claim, MAX_FRAME).await.unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
        let claim = format!("~2 0 {} raw\n", MAX_FRAME + 1).into_bytes();
        assert!(decode(&claim, MAX_FRAME).await.is_err());
    }

    #[tokio::test]
    async fn malformed_headers_are_errors() {
        for h in [
            "~1 2\n",
            "~x 0 1 raw\n",
            "~1 0 1 lz4\n",
            "~1 0 5 raw\n{",
            &format!("~{}\n", "9".repeat(200)),
        ] {
            assert!(decode(h.as_bytes(), MAX_FRAME).await.is_err(), "{h:?}");
        }
    }

    #[test]
    fn encoder_refuses_frames_over_the_limit() {
        let mut enc = Encoder::new(zstd_default()).with_max_frame(1024);
        assert!(enc.encode(&note(vec![0; 2048])).is_err());
        let mut enc = Encoder::lines().with_max_frame(1024);
        assert!(enc.encode(&note(vec![0; 2048])).is_err());
    }
}
