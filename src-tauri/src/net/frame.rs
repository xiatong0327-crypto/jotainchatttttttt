//! Length-prefixed frames: u32 big-endian length + UTF-8 JSON body.

use std::io::{ErrorKind, Read, Write};

pub const MAX_FRAME_BYTES: u32 = 1024 * 1024; // 1 MiB hard cap

#[derive(Debug)]
pub enum FrameError {
    /// Socket read timed out / would-block (keep session alive).
    Timeout,
    /// Peer closed or hard I/O failure.
    Io(String),
    TooLarge(u32),
}

impl FrameError {
    pub fn is_timeout(&self) -> bool {
        matches!(self, FrameError::Timeout)
    }
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Timeout => write!(f, "timeout"),
            FrameError::Io(s) => write!(f, "{s}"),
            FrameError::TooLarge(n) => write!(f, "frame length {n} exceeds max {MAX_FRAME_BYTES}"),
        }
    }
}

fn map_io(err: std::io::Error, ctx: &str) -> FrameError {
    match err.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted => {
            FrameError::Timeout
        }
        _ => FrameError::Io(format!("{ctx}: {err}")),
    }
}

pub fn write_frame<W: Write>(writer: &mut W, body: &[u8]) -> Result<(), String> {
    if body.len() as u32 > MAX_FRAME_BYTES {
        return Err(format!(
            "frame too large: {} > {}",
            body.len(),
            MAX_FRAME_BYTES
        ));
    }
    let len = (body.len() as u32).to_be_bytes();
    writer
        .write_all(&len)
        .map_err(|e| format!("write frame len: {e}"))?;
    writer
        .write_all(body)
        .map_err(|e| format!("write frame body: {e}"))?;
    writer.flush().map_err(|e| format!("flush frame: {e}"))?;
    Ok(())
}

pub fn read_frame<R: Read>(reader: &mut R) -> Result<Vec<u8>, FrameError> {
    let mut len_buf = [0u8; 4];
    reader
        .read_exact(&mut len_buf)
        .map_err(|e| map_io(e, "read frame len"))?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len));
    }
    let mut body = vec![0u8; len as usize];
    if len > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|e| map_io(e, "read frame body"))?;
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn roundtrip() {
        let payload = br#"{"type":"text","body":"hi"}"#;
        let mut buf = Vec::new();
        write_frame(&mut buf, payload).unwrap();
        let mut cur = Cursor::new(buf);
        let out = read_frame(&mut cur).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn rejects_oversized_length_prefix() {
        let mut bad = (MAX_FRAME_BYTES + 1).to_be_bytes().to_vec();
        bad.extend_from_slice(&[0u8; 8]);
        let mut cur = Cursor::new(bad);
        match read_frame(&mut cur) {
            Err(FrameError::TooLarge(_)) => {}
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }
}
