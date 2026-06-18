use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use super::TransportError;

const DEFAULT_MAX_SIZE: usize = 16 * 1024 * 1024;

/// `Content-Length`-framed codec shared by stdio and TCP (ADR 0011).
///
/// `Decoder::Item` is the raw JSON envelope body as `Bytes` — envelope
/// parsing into `RawMessage` happens one layer up in
/// `crate::transport::envelope`.
#[derive(Debug)]
pub struct ContentLengthCodec {
    state: State,
    max_size: usize,
}

#[derive(Debug)]
enum State {
    Headers,
    Body { length: usize },
}

impl Default for ContentLengthCodec {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SIZE)
    }
}

impl ContentLengthCodec {
    pub fn new(max_size: usize) -> Self {
        Self {
            state: State::Headers,
            max_size,
        }
    }
}

impl Decoder for ContentLengthCodec {
    type Item = Bytes;
    type Error = TransportError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Bytes>, TransportError> {
        loop {
            match self.state {
                State::Headers => {
                    let Some(headers_end) = find_header_end(src) else {
                        return Ok(None);
                    };
                    let headers = src.split_to(headers_end);
                    src.advance(4);
                    let length = parse_content_length(&headers)?;
                    if length > self.max_size {
                        return Err(TransportError::OversizedMessage {
                            length,
                            limit: self.max_size,
                        });
                    }
                    self.state = State::Body { length };
                }
                State::Body { length } => {
                    if src.len() < length {
                        src.reserve(length - src.len());
                        return Ok(None);
                    }
                    let body = src.split_to(length).freeze();
                    self.state = State::Headers;
                    return Ok(Some(body));
                }
            }
        }
    }
}

impl Encoder<Bytes> for ContentLengthCodec {
    type Error = TransportError;

    fn encode(&mut self, item: Bytes, dst: &mut BytesMut) -> Result<(), TransportError> {
        let header = format!("Content-Length: {}\r\n\r\n", item.len());
        dst.reserve(header.len() + item.len());
        dst.put_slice(header.as_bytes());
        dst.put_slice(&item);
        Ok(())
    }
}

fn find_header_end(src: &[u8]) -> Option<usize> {
    src.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_content_length(headers: &[u8]) -> Result<usize, TransportError> {
    let s = std::str::from_utf8(headers)
        .map_err(|e| TransportError::Malformed(format!("non-UTF-8 headers: {e}")))?;

    for line in s.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            return value
                .trim()
                .parse()
                .map_err(|e| TransportError::Malformed(format!("invalid Content-Length: {e}")));
        }
    }

    Err(TransportError::Malformed(
        "missing Content-Length header".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_single_message() {
        let mut codec = ContentLengthCodec::default();
        let mut buf = BytesMut::from(&b"Content-Length: 17\r\n\r\n{\"jsonrpc\":\"2.0\"}"[..]);
        let body = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(&body[..], br#"{"jsonrpc":"2.0"}"#);
        assert!(buf.is_empty());
    }

    #[test]
    fn waits_for_full_body() {
        let mut codec = ContentLengthCodec::default();
        let mut buf = BytesMut::from(&b"Content-Length: 17\r\n\r\n{\"jsonrpc\""[..]);
        assert!(codec.decode(&mut buf).unwrap().is_none());
        buf.extend_from_slice(b":\"2.0\"}");
        let body = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(&body[..], br#"{"jsonrpc":"2.0"}"#);
    }

    #[test]
    fn ignores_extra_headers() {
        let mut codec = ContentLengthCodec::default();
        let mut buf = BytesMut::from(
            &b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: 17\r\n\r\n{\"jsonrpc\":\"2.0\"}"[..],
        );
        let body = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(&body[..], br#"{"jsonrpc":"2.0"}"#);
    }

    #[test]
    fn rejects_oversized() {
        let mut codec = ContentLengthCodec::new(8);
        let mut buf = BytesMut::from(&b"Content-Length: 17\r\n\r\n"[..]);
        let err = codec.decode(&mut buf).unwrap_err();
        assert!(matches!(err, TransportError::OversizedMessage { .. }));
    }

    #[test]
    fn encodes_with_header() {
        let mut codec = ContentLengthCodec::default();
        let mut buf = BytesMut::new();
        codec
            .encode(Bytes::from_static(br#"{"jsonrpc":"2.0"}"#), &mut buf)
            .unwrap();
        assert_eq!(&buf[..], b"Content-Length: 17\r\n\r\n{\"jsonrpc\":\"2.0\"}");
    }
}
