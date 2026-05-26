//! Circular buffer that keeps the most recent N bytes of PTY output.
//! Used to provide context for AI error correction during SSH sessions.

pub struct OutputBuffer {
    data: Vec<u8>,
    capacity: usize,
    write_pos: usize,
    len: usize,
}

impl OutputBuffer {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "OutputBuffer capacity must be > 0");
        Self {
            data: vec![0u8; capacity],
            capacity,
            write_pos: 0,
            len: 0,
        }
    }

    /// Append bytes, overwriting oldest data when full.
    pub fn append(&mut self, input: &[u8]) {
        for &byte in input {
            self.data[self.write_pos] = byte;
            self.write_pos = (self.write_pos + 1) % self.capacity;
            if self.len < self.capacity {
                self.len += 1;
            }
        }
    }

    /// Return the most recent bytes up to `max_len`, in order.
    pub fn recent(&self, max_len: usize) -> Vec<u8> {
        let count = max_len.min(self.len);
        let mut result = Vec::with_capacity(count);
        let actual_start = if self.len < self.capacity {
            self.len.saturating_sub(count)
        } else {
            (self.write_pos + self.capacity - count) % self.capacity
        };
        for i in 0..count {
            result.push(self.data[(actual_start + i) % self.capacity]);
        }
        result
    }

    /// Clear the buffer.
    pub fn clear(&mut self) {
        self.write_pos = 0;
        self.len = 0;
    }

    /// Current number of bytes stored.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_append_and_read() {
        let mut buf = OutputBuffer::new(100);
        buf.append(b"hello world");
        assert_eq!(buf.recent(100), b"hello world");
    }

    #[test]
    fn test_circular_overwrite() {
        let mut buf = OutputBuffer::new(10);
        buf.append(b"0123456789");
        assert_eq!(buf.recent(10), b"0123456789");
        buf.append(b"AB");
        assert_eq!(buf.recent(10), b"23456789AB");
    }

    #[test]
    fn test_recent_with_max_len() {
        let mut buf = OutputBuffer::new(100);
        buf.append(b"hello world");
        assert_eq!(buf.recent(5), b"world");
    }

    #[test]
    fn test_clear() {
        let mut buf = OutputBuffer::new(100);
        buf.append(b"data");
        buf.clear();
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn test_wrap_around_multiple_times() {
        let mut buf = OutputBuffer::new(5);
        buf.append(b"ABCDE");
        buf.append(b"FGHIJ");
        buf.append(b"KLMNO");
        assert_eq!(buf.recent(5), b"KLMNO");
    }

    #[test]
    fn test_empty_buffer() {
        let buf = OutputBuffer::new(100);
        assert!(buf.is_empty());
        assert_eq!(buf.recent(100), b"");
    }
}
