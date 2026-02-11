//! Segment footer for metadata and validation.
//!
//! The footer is fixed size (40 bytes) at the end of every segment, enabling
//! a single range request to fetch segment metadata.

use std::io::{self, Read, Write};

/// Magic bytes identifying a Sybil segment: "SYBL" = 0x5359424C
pub const MAGIC: u32 = 0x5359424C;

/// Footer size in bytes (fixed).
pub const FOOTER_SIZE: usize = 40;

/// Segment footer containing metadata for quick access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentFooter {
    /// Number of log entries in this segment.
    pub log_count: u32,
    /// Minimum timestamp (epoch millis).
    pub min_timestamp: i64,
    /// Maximum timestamp (epoch millis).
    pub max_timestamp: i64,
    /// Size of the compressed entries section.
    pub entries_size: u32,
    /// Offset of the dictionary from start of segment.
    pub dictionary_offset: u32,
    /// Offset of the bloom filters from start of segment.
    pub bloom_offset: u32,
    /// CRC32C checksum of all preceding bytes.
    pub checksum: u32,
}

impl SegmentFooter {
    /// Write the footer (40 bytes) to a writer.
    pub fn write_to<W: Write>(&self, mut w: W) -> io::Result<()> {
        w.write_all(&self.log_count.to_le_bytes())?;
        w.write_all(&self.min_timestamp.to_le_bytes())?;
        w.write_all(&self.max_timestamp.to_le_bytes())?;
        w.write_all(&self.entries_size.to_le_bytes())?;
        w.write_all(&self.dictionary_offset.to_le_bytes())?;
        w.write_all(&self.bloom_offset.to_le_bytes())?;
        w.write_all(&self.checksum.to_le_bytes())?;
        w.write_all(&MAGIC.to_le_bytes())?;
        Ok(())
    }

    /// Read a footer from exactly 40 bytes.
    pub fn read_from<R: Read>(mut r: R) -> io::Result<Self> {
        let mut buf = [0u8; FOOTER_SIZE];
        r.read_exact(&mut buf)?;
        Self::from_bytes(&buf)
    }

    /// Parse footer from a 40-byte slice.
    pub fn from_bytes(data: &[u8]) -> io::Result<Self> {
        if data.len() != FOOTER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("footer must be {} bytes, got {}", FOOTER_SIZE, data.len()),
            ));
        }

        // Check magic
        let magic = u32::from_le_bytes([data[36], data[37], data[38], data[39]]);
        if magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid magic: expected 0x{:08X}, got 0x{:08X}",
                    MAGIC, magic
                ),
            ));
        }

        Ok(Self {
            log_count: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            min_timestamp: i64::from_le_bytes([
                data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11],
            ]),
            max_timestamp: i64::from_le_bytes([
                data[12], data[13], data[14], data[15], data[16], data[17], data[18], data[19],
            ]),
            entries_size: u32::from_le_bytes([data[20], data[21], data[22], data[23]]),
            dictionary_offset: u32::from_le_bytes([data[24], data[25], data[26], data[27]]),
            bloom_offset: u32::from_le_bytes([data[28], data[29], data[30], data[31]]),
            checksum: u32::from_le_bytes([data[32], data[33], data[34], data[35]]),
        })
    }

    /// Validate the checksum against segment data.
    pub fn validate_checksum(&self, segment_data: &[u8]) -> bool {
        if segment_data.len() < FOOTER_SIZE {
            return false;
        }
        // Checksum covers everything except the last 8 bytes (checksum + magic)
        let data_to_check = &segment_data[..segment_data.len() - 8];
        let computed = crc32fast::hash(data_to_check);
        computed == self.checksum
    }
}

/// Compute CRC32C checksum for segment data (excluding checksum and magic).
pub fn compute_checksum(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_footer_roundtrip() {
        let footer = SegmentFooter {
            log_count: 1234,
            min_timestamp: 1705323600000,
            max_timestamp: 1705323660000,
            entries_size: 65536,
            dictionary_offset: 65536,
            bloom_offset: 66000,
            checksum: 0xDEADBEEF,
        };

        let mut buf = Vec::new();
        footer.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), FOOTER_SIZE);

        let parsed = SegmentFooter::read_from(Cursor::new(&buf)).unwrap();
        assert_eq!(footer, parsed);
    }

    #[test]
    fn test_footer_invalid_magic() {
        let mut buf = vec![0u8; FOOTER_SIZE];
        // Set wrong magic
        buf[36..40].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);

        let result = SegmentFooter::read_from(Cursor::new(&buf));
        assert!(result.is_err());
    }

    #[test]
    fn test_footer_checksum_validation() {
        // Build a minimal segment with valid checksum
        // Layout: [60 bytes data][32 bytes partial footer][4 bytes checksum][4 bytes magic]
        let mut data = vec![0u8; 100];

        // Fill data section
        for (i, byte) in data.iter_mut().enumerate().take(60) {
            *byte = (i % 256) as u8;
        }

        // Write partial footer (without checksum/magic)
        let log_count: u32 = 10;
        let min_ts: i64 = 1000;
        let max_ts: i64 = 2000;
        let entries_size: u32 = 50;
        let dict_offset: u32 = 50;
        let bloom_offset: u32 = 60;

        data[60..64].copy_from_slice(&log_count.to_le_bytes());
        data[64..72].copy_from_slice(&min_ts.to_le_bytes());
        data[72..80].copy_from_slice(&max_ts.to_le_bytes());
        data[80..84].copy_from_slice(&entries_size.to_le_bytes());
        data[84..88].copy_from_slice(&dict_offset.to_le_bytes());
        data[88..92].copy_from_slice(&bloom_offset.to_le_bytes());

        // Compute checksum of everything except last 8 bytes
        let checksum = compute_checksum(&data[..92]);
        data[92..96].copy_from_slice(&checksum.to_le_bytes());
        data[96..100].copy_from_slice(&MAGIC.to_le_bytes());

        // Parse and validate
        let parsed = SegmentFooter::from_bytes(&data[60..100]).unwrap();
        assert!(parsed.validate_checksum(&data));
    }
}
