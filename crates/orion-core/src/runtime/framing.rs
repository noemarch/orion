//! Length-prefixed JSON frames for the runtime socket.
//!
//! The wire format is a `u32` little-endian byte length followed by that many
//! bytes of UTF-8 JSON. The length counts the JSON bytes only.

use std::io::{self, Read, Write};

use serde_json::Value;

/// The protocol version this build speaks.
pub const PROTOCOL_VERSION: u32 = crate::engine::PROTOCOL_VERSION;

/// The largest frame this build will read, so a corrupt length cannot make it
/// allocate the machine's memory.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Writes one frame: its length, then its body.
pub fn write_frame(writer: &mut impl Write, frame: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(frame)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let length = body.len() as u32;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

/// Reads one frame. `Ok(None)` means the peer closed the connection cleanly.
pub fn read_frame(reader: &mut impl Read) -> io::Result<Option<Value>> {
    let mut prefix = [0u8; 4];
    let mut read = 0usize;
    while read < 4 {
        match reader.read(&mut prefix[read..]) {
            Ok(0) => {
                if read == 0 {
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "short frame prefix",
                ));
            }
            Ok(n) => read += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    let length = u32::from_le_bytes(prefix) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame length exceeds MAX_FRAME_BYTES",
        ));
    }
    let mut body = vec![0u8; length];
    let mut read = 0usize;
    while read < length {
        match reader.read(&mut body[read..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "short frame body",
                ));
            }
            Ok(n) => read += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    let value: Value = serde_json::from_slice(&body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_frame_survives_the_round_trip() {
        let frame = serde_json::json!({ "type": "ping", "id": 7, "payload": [1, 2, 3] });
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &frame).expect("write");
        let mut cursor = Cursor::new(buffer);
        let read = read_frame(&mut cursor).expect("read").expect("frame");
        assert_eq!(read, frame);
    }

    #[test]
    fn a_frame_that_is_too_long_is_refused() {
        let mut buffer = Vec::new();
        let length = (MAX_FRAME_BYTES + 1) as u32;
        buffer.extend_from_slice(&length.to_le_bytes());
        let mut cursor = Cursor::new(buffer);
        let result = read_frame(&mut cursor);
        assert!(result.is_err(), "an over-long frame must be refused");
    }

    #[test]
    fn an_empty_stream_is_a_closed_connection() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let result = read_frame(&mut cursor).expect("read");
        assert!(result.is_none(), "empty input is a clean close");
    }
}
