//! Variable-length integer encoding.
//!
//! Uses MSB as continuation flag. Also provides zigzag encoding for signed
//! integers (maps small negative values to small unsigned values).

use std::io::{self, Read, Write};

/// Write a varint to a writer. Returns bytes written.
pub fn write_varint<W: Write>(mut w: W, mut value: u64) -> io::Result<usize> {
    let mut bytes_written = 0;
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80; // set continuation bit
        }
        w.write_all(&[byte])?;
        bytes_written += 1;
        if value == 0 {
            break;
        }
    }
    Ok(bytes_written)
}

/// Read a varint from a reader.
pub fn read_varint<R: Read>(mut r: R) -> io::Result<u64> {
    let mut result: u64 = 0;
    let mut shift = 0;
    loop {
        let mut byte = [0u8];
        r.read_exact(&mut byte)?;
        result |= ((byte[0] & 0x7F) as u64) << shift;
        if byte[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "varint too long"));
        }
    }
    Ok(result)
}

/// Encode a signed integer using zigzag encoding.
/// Maps: 0 -> 0, -1 -> 1, 1 -> 2, -2 -> 3, 2 -> 4, ...
pub fn zigzag_encode(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

/// Decode a zigzag-encoded integer.
pub fn zigzag_decode(n: u64) -> i64 {
    ((n >> 1) as i64) ^ (-((n & 1) as i64))
}

/// Calculate the encoded size of a varint without writing it.
pub fn varint_size(mut value: u64) -> usize {
    let mut size = 1;
    while value >= 0x80 {
        value >>= 7;
        size += 1;
    }
    size
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_varint_roundtrip() {
        let values = [0, 1, 127, 128, 16383, 16384, u64::MAX];
        for &v in &values {
            let mut buf = Vec::new();
            write_varint(&mut buf, v).unwrap();
            let decoded = read_varint(Cursor::new(&buf)).unwrap();
            assert_eq!(v, decoded, "failed for value {}", v);
        }
    }

    #[test]
    fn test_varint_sizes() {
        assert_eq!(varint_size(0), 1);
        assert_eq!(varint_size(127), 1);
        assert_eq!(varint_size(128), 2);
        assert_eq!(varint_size(16383), 2);
        assert_eq!(varint_size(16384), 3);
    }

    #[test]
    fn test_zigzag_roundtrip() {
        let values = [0, -1, 1, -2, 2, i64::MIN, i64::MAX];
        for &v in &values {
            let encoded = zigzag_encode(v);
            let decoded = zigzag_decode(encoded);
            assert_eq!(v, decoded, "failed for value {}", v);
        }
    }

    #[test]
    fn test_zigzag_small_values() {
        // Small values should encode to small unsigned integers
        assert_eq!(zigzag_encode(0), 0);
        assert_eq!(zigzag_encode(-1), 1);
        assert_eq!(zigzag_encode(1), 2);
        assert_eq!(zigzag_encode(-2), 3);
        assert_eq!(zigzag_encode(2), 4);
    }
}
