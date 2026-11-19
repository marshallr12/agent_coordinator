//! The request preamble read before any token is minted. A connection only
//! costs a mint once it has sent a version 1 request line and a bundle
//! length within the helper's limit; everything read is then replayed to
//! `serve_one`, which validates the request in full.
use coordinator_local::candidate_push::PROTOCOL_VERSION;
use serde_json::Value;
use std::io::{self, Cursor, Read, Write};

/// The longest request line read before minting: the bound `serve_one`
/// applies to the same line, so a longer one is refused either way.
const MAX_REQUEST_LINE: usize = 4 * 1024;

/// The bytes a client sent before minting, and whether they form a whole
/// preamble (a request line and the 8-byte bundle length).
#[derive(Debug)]
pub struct Preamble {
    bytes: Vec<u8>,
    complete: bool,
}

impl Preamble {
    /// Reads up to a newline (at most [`MAX_REQUEST_LINE`] bytes before it)
    /// and then 8 length bytes, stopping early at end of stream, a read
    /// error (such as the socket timeout) or an overlong line.
    pub fn read<R: Read>(stream: &mut R) -> Self {
        let mut bytes = Vec::new();
        let complete = read_line(stream, &mut bytes) && read_length(stream, &mut bytes);
        Self { bytes, complete }
    }

    /// Whether the preamble justifies a mint: complete, a JSON request line
    /// naming [`PROTOCOL_VERSION`], and a bundle length within `max_bundle`.
    pub fn worth_minting(&self, max_bundle: u64) -> bool {
        if !self.complete {
            return false;
        }
        let (line, length) = self.bytes.split_at(self.bytes.len() - 8);
        let version = serde_json::from_slice::<Value>(&line[..line.len() - 1])
            .ok()
            .and_then(|request| request["version"].as_u64());
        let length = u64::from_be_bytes(length.try_into().unwrap_or([0xff; 8]));
        version == Some(u64::from(PROTOCOL_VERSION)) && length <= max_bundle
    }

    /// A stream that yields this preamble and then, if it is complete, the
    /// rest of `stream`; an incomplete one ends after its bytes, so the
    /// buffered bytes alone decide the refusal. Writes go to `stream`.
    pub fn replay<S: Read + Write>(self, stream: S) -> Replayed<S> {
        Replayed {
            buffered: Cursor::new(self.bytes),
            live: self.complete,
            stream,
        }
    }
}

/// Reads bytes up to and including a newline into `bytes`; false when the
/// stream ends, fails, or the line exceeds [`MAX_REQUEST_LINE`].
fn read_line<R: Read>(stream: &mut R, bytes: &mut Vec<u8>) -> bool {
    let mut byte = [0_u8; 1];
    while bytes.len() <= MAX_REQUEST_LINE {
        if stream.read_exact(&mut byte).is_err() {
            return false;
        }
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            return true;
        }
    }
    false
}

/// Appends the 8-byte bundle length to `bytes`; false when fewer arrive.
fn read_length<R: Read>(stream: &mut R, bytes: &mut Vec<u8>) -> bool {
    let mut length = [0_u8; 8];
    let read = stream.read_exact(&mut length).is_ok();
    if read {
        bytes.extend_from_slice(&length);
    }
    read
}

/// A preamble replayed in front of the live stream.
pub struct Replayed<S> {
    buffered: Cursor<Vec<u8>>,
    live: bool,
    stream: S,
}

impl<S: Read> Read for Replayed<S> {
    /// Serves buffered bytes first, then the live stream (or end of stream
    /// for an incomplete preamble).
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let buffered = self.buffered.read(buffer)?;
        if buffered > 0 || buffer.is_empty() || !self.live {
            return Ok(buffered);
        }
        self.stream.read(buffer)
    }
}

impl<S: Write> Write for Replayed<S> {
    /// Writes to the live stream.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.write(bytes)
    }

    /// Flushes the live stream.
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request line naming `version` followed by the big-endian `length`.
    fn preamble(version: u32, length: u64) -> Vec<u8> {
        let mut bytes =
            format!("{{\"version\":{version},\"revision\":\"r\",\"tree\":\"t\"}}\n").into_bytes();
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes
    }

    /// Reads a preamble from `bytes` and reports whether it is worth a mint
    /// under a 100-byte bundle limit.
    fn worth(bytes: &[u8]) -> bool {
        Preamble::read(&mut Cursor::new(bytes)).worth_minting(100)
    }

    #[test]
    fn only_a_whole_version_1_preamble_within_the_limit_is_worth_minting() {
        assert!(worth(&preamble(1, 100)));
        assert!(!worth(b""));
        assert!(!worth(b"not a request\n\0\0\0\0\0\0\0\0"));
        assert!(!worth(&preamble(2, 1)));
        assert!(!worth(&preamble(1, 101)));
        assert!(!worth(&preamble(1, 1)[..20]));
        let whole = preamble(1, 1);
        assert!(!worth(&whole[..whole.len() - 1]));
        let long = [vec![b' '; MAX_REQUEST_LINE + 1], preamble(1, 1)].concat();
        assert!(!worth(&long));
    }

    /// A Read + Write test stream: reads from `input`, collects writes.
    struct Duplex {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Duplex {
        /// Reads from the input.
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.input.read(buffer)
        }
    }

    impl Write for Duplex {
        /// Collects the bytes.
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        /// Nothing to flush.
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Reads a preamble from a duplex holding `bytes`, replays it, and
    /// returns everything the replay yields.
    fn replayed(bytes: &[u8]) -> Vec<u8> {
        let mut duplex = Duplex {
            input: Cursor::new(bytes.to_vec()),
            output: Vec::new(),
        };
        let preamble = Preamble::read(&mut duplex);
        let mut replay = preamble.replay(duplex);
        let mut all = Vec::new();
        replay.read_to_end(&mut all).unwrap();
        replay.write_all(b"reply").unwrap();
        assert_eq!(replay.stream.output, b"reply");
        all
    }

    #[test]
    fn a_complete_preamble_replays_before_the_live_stream() {
        let bytes = [preamble(1, 3), b"abc".to_vec()].concat();
        assert_eq!(replayed(&bytes), bytes);
    }

    #[test]
    fn an_incomplete_preamble_replays_only_what_was_read() {
        let bytes = b"no newline here";
        assert_eq!(replayed(bytes), bytes);
        let garbage = b"x".repeat(MAX_REQUEST_LINE + 10);
        assert_eq!(replayed(&garbage), &garbage[..=MAX_REQUEST_LINE]);
    }
}
