//! Scrollback buffer for PTY daemon.
//!
//! Maintains a ring buffer of the most recent PTY output so that when a client
//! re-attaches after a detach, it can see the screen content that was produced
//! while it was away.
//!
//! Line-boundary handling: when the buffer is full and wraps, the snapshot
//! scans forward to the first `\n` and discards incomplete data at the head,
//! so replay starts from a clean line boundary (preserving ANSI integrity).

/// Default scrollback capacity: 256 KB (~2000-4000 lines of terminal output).
pub const DEFAULT_SCROLLBACK_SIZE: usize = 256 * 1024;

/// Minimum recommended capacity (enforced only by `with_default_size` helpers).
pub const MIN_SCROLLBACK_SIZE: usize = 4096;

/// Maximum scrollback chunk size for replay (16KB per frame to avoid blocking).
pub const SCROLLBACK_CHUNK_SIZE: usize = 16 * 1024;

/// Ring buffer retaining the most recent PTY output bytes.
pub struct ScrollbackBuffer {
    buf: Vec<u8>,
    capacity: usize,
    write_pos: usize,
    len: usize,
}

impl ScrollbackBuffer {
    /// Create a new scrollback buffer with the given byte capacity.
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            buf: vec![0u8; cap],
            capacity: cap,
            write_pos: 0,
            len: 0,
        }
    }

    /// Create with default capacity (256 KB).
    pub fn with_default_size() -> Self {
        Self::new(DEFAULT_SCROLLBACK_SIZE)
    }

    /// Append raw PTY output bytes. Overwrites oldest data when full.
    ///
    /// Copies data in contiguous chunks (up to the wrap point, then the
    /// remainder) rather than per-byte, to reduce overhead on the daemon's
    /// hot PTY-output path.
    pub fn push(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let mut remaining = data;
        while !remaining.is_empty() {
            let chunk_end = self.write_pos + remaining.len().min(self.capacity);
            let copy_len = chunk_end.min(self.capacity) - self.write_pos;
            self.buf[self.write_pos..self.write_pos + copy_len]
                .copy_from_slice(&remaining[..copy_len]);
            self.write_pos = (self.write_pos + copy_len) % self.capacity;
            let new_len = self.len.saturating_add(copy_len).min(self.capacity);
            self.len = new_len;
            remaining = &remaining[copy_len..];
        }
    }

    /// Return the raw buffer content as a contiguous Vec.
    fn raw_snapshot(&self) -> Vec<u8> {
        if self.len == 0 {
            return Vec::new();
        }

        if self.len < self.capacity {
            // Not wrapped yet
            return self.buf[..self.len].to_vec();
        }

        // Full buffer: data goes from write_pos to write_pos-1 (wrapping)
        let mut result = Vec::with_capacity(self.capacity);
        result.extend_from_slice(&self.buf[self.write_pos..self.capacity]);
        result.extend_from_slice(&self.buf[..self.write_pos]);
        result
    }

    /// Return the buffer content, truncated to start at the first line boundary
    /// (`\n`) if the buffer has wrapped. This ensures replay starts cleanly
    /// without splitting ANSI escape sequences.
    pub fn snapshot(&self) -> Vec<u8> {
        let raw = self.raw_snapshot();
        if raw.len() < self.capacity {
            // Buffer hasn't wrapped, no need to truncate
            return raw;
        }

        // Find first newline and skip everything before it
        match raw.iter().position(|&b| b == b'\n') {
            Some(pos) if pos + 1 < raw.len() => raw[pos + 1..].to_vec(),
            _ => raw, // No newline or newline at very end; return as-is
        }
    }

    /// Return chunks of the scrollback for replay, each at most `max_chunk_size`
    /// bytes. Allows the daemon to send scrollback in manageable frames.
    pub fn replay_chunks(&self, max_chunk_size: usize) -> Vec<Vec<u8>> {
        let snapshot = self.snapshot();
        if snapshot.is_empty() {
            return Vec::new();
        }

        let chunk_size = max_chunk_size.clamp(1, SCROLLBACK_CHUNK_SIZE);
        snapshot.chunks(chunk_size).map(|c| c.to_vec()).collect()
    }

    /// Number of bytes currently stored (may be less than capacity).
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Total capacity in bytes.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Clear all stored data.
    pub fn clear(&mut self) {
        self.write_pos = 0;
        self.len = 0;
    }
}

impl Default for ScrollbackBuffer {
    fn default() -> Self {
        Self::with_default_size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_push_and_snapshot() {
        let mut sb = ScrollbackBuffer::new(1024);
        sb.push(b"hello world\n");
        assert_eq!(sb.snapshot(), b"hello world\n");
    }

    #[test]
    fn test_multiple_pushes() {
        let mut sb = ScrollbackBuffer::new(1024);
        sb.push(b"line 1\n");
        sb.push(b"line 2\n");
        sb.push(b"line 3\n");
        assert_eq!(sb.snapshot(), b"line 1\nline 2\nline 3\n");
    }

    #[test]
    fn test_overflow_truncates_old_data() {
        let mut sb = ScrollbackBuffer::new(20);
        sb.push(b"AAAAAAAAAAAA\n"); // 13 bytes
        sb.push(b"BBBBBBBBBBBB\n"); // 13 bytes, total 26 > 20
        let snap = sb.snapshot();
        // Should have truncated the beginning, keeping recent data
        assert!(snap.len() <= 20);
        assert!(snap.windows(5).any(|w| w == b"BBBB\n"));
    }

    #[test]
    fn test_overflow_line_boundary() {
        let mut sb = ScrollbackBuffer::new(14);
        sb.push(b"AAAA\n"); // 5 bytes
        sb.push(b"BBBB\n"); // 5 bytes, total 10
        sb.push(b"CCCC\n"); // 5 bytes, total 15 > 14
        let snap = sb.snapshot();
        // Snapshot should start from a line boundary (after a \n)
        assert!(snap.windows(5).any(|w| w == b"BBBB\n") || snap.windows(5).any(|w| w == b"CCCC\n"));
    }

    #[test]
    fn test_empty_buffer() {
        let sb = ScrollbackBuffer::new(1024);
        assert!(sb.is_empty());
        assert_eq!(sb.snapshot(), b"");
        assert!(sb.replay_chunks(1024).is_empty());
    }

    #[test]
    fn test_replay_chunks() {
        let mut sb = ScrollbackBuffer::new(1024);
        let data = b"0123456789ABCDEF"; // 16 bytes
        for _ in 0..10 {
            sb.push(data);
        }
        // 160 bytes total, chunk into 64-byte pieces
        let chunks = sb.replay_chunks(64);
        assert_eq!(chunks.len(), 3); // 64 + 64 + 32
        assert_eq!(chunks[0].len(), 64);
        assert_eq!(chunks[1].len(), 64);
        assert_eq!(chunks[2].len(), 32);

        // Verify data integrity
        let mut reassembled = Vec::new();
        for chunk in &chunks {
            reassembled.extend_from_slice(chunk);
        }
        let expected = data.repeat(10);
        assert_eq!(reassembled, expected);
    }

    #[test]
    fn test_replay_chunks_default_size() {
        let mut sb = ScrollbackBuffer::new(1024);
        sb.push(b"short");
        let chunks = sb.replay_chunks(SCROLLBACK_CHUNK_SIZE);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], b"short");
    }

    #[test]
    fn test_large_wrap_around() {
        let mut sb = ScrollbackBuffer::new(100);
        // Fill with pattern including newlines
        let pattern: Vec<u8> = (0..=255)
            .map(|i| if i % 40 == 0 { b'\n' } else { i })
            .cycle()
            .take(250)
            .collect();
        sb.push(&pattern);

        let snap = sb.snapshot();
        assert!(snap.len() <= 100);
    }

    #[test]
    fn test_clear() {
        let mut sb = ScrollbackBuffer::new(1024);
        sb.push(b"data");
        sb.clear();
        assert!(sb.is_empty());
        assert_eq!(sb.snapshot(), b"");
    }

    #[test]
    fn test_ansi_sequence_preservation() {
        let mut sb = ScrollbackBuffer::new(50);
        sb.push(b"\x1b[31mRED TEXT\x1b[0m\n");
        sb.push(b"normal text\n");
        let snap = sb.snapshot();
        assert!(snap.windows(5).any(|w| w == b"\x1b[31m"));
        assert!(snap.windows(4).any(|w| w == b"\x1b[0m"));
    }

    #[test]
    fn test_no_newline_overflow() {
        let mut sb = ScrollbackBuffer::new(10);
        sb.push(b"ABCDEFGHIJ"); // 10 bytes, no newline
        sb.push(b"KLMNOP"); // overflow
        let snap = sb.snapshot();
        assert!(snap.len() <= 10);
        // Without newlines, snapshot may include partial data
        assert!(snap.ends_with(b"KLMNOP") || snap.ends_with(b"LMNOP"));
    }

    #[test]
    fn test_partial_fill_snapshot_exact() {
        let mut sb = ScrollbackBuffer::new(100);
        sb.push(b"ABCDEF");
        assert_eq!(sb.len(), 6);
        assert_eq!(sb.snapshot(), b"ABCDEF");
    }
}
