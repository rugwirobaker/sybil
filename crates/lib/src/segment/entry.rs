//! Log entry encoding and decoding.
//!
//! Each entry stores: timestamp delta, presence bitmap, dictionary IDs for
//! indexed fields, and the original JSON payload.
//!
//! All multi-byte integers use little-endian encoding.

use std::io::{self, Read, Write};

/// Fields that are automatically indexed.
pub const AUTO_INDEX_FIELDS: &[&str] = &[
    "service",     // 0
    "app",         // 1 (alias for service)
    "source",      // 2 (alias for service)
    "component",   // 3
    "namespace",   // 4
    "environment", // 5
    "env",         // 6 (alias for environment)
    "host",        // 7
    "level",       // 8
    "severity",    // 9 (alias for level)
    "trace_id",    // 10
];

/// Bitmap indicating which indexed fields are present in an entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PresenceBitmap(pub u16);

impl PresenceBitmap {
    pub fn new() -> Self {
        Self(0)
    }

    pub fn set(&mut self, field_index: usize) {
        if field_index < 16 {
            self.0 |= 1 << field_index;
        }
    }

    pub fn is_set(&self, field_index: usize) -> bool {
        if field_index < 16 {
            (self.0 >> field_index) & 1 == 1
        } else {
            false
        }
    }

    /// Count how many bits are set.
    pub fn count(&self) -> u32 {
        self.0.count_ones()
    }

    /// Iterate over set field indices.
    pub fn iter_set(&self) -> impl Iterator<Item = usize> + '_ {
        (0..16).filter(|&i| self.is_set(i))
    }
}

/// A raw log entry before processing.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Timestamp in epoch milliseconds.
    pub timestamp: i64,
    /// The original JSON payload as bytes.
    pub payload: Vec<u8>,
}

/// A processed log entry ready for segment encoding.
#[derive(Debug, Clone)]
pub struct ProcessedEntry {
    /// Timestamp in epoch milliseconds.
    pub timestamp: i64,
    /// Which indexed fields are present.
    pub presence: PresenceBitmap,
    /// Dictionary IDs for present indexed fields (in order of presence bitmap).
    pub dict_ids: Vec<u32>,
    /// The original JSON payload.
    pub payload: Vec<u8>,
}

impl ProcessedEntry {
    /// Encode this entry to a writer.
    ///
    /// Format (all integers little-endian):
    /// - timestamp_delta: i32 LE (delta from base_timestamp in milliseconds)
    /// - presence_bitmap: u16 LE (which indexed fields are present)
    /// - dict_ids: [u32 LE] × popcount(presence) (dictionary IDs for present fields)
    /// - payload_len: u32 LE
    /// - payload: [u8] × payload_len
    pub fn write_to<W: Write>(&self, mut w: W, base_timestamp: i64) -> io::Result<usize> {
        let mut written = 0;

        // Timestamp delta as i32 LE (handles ±24 days in milliseconds)
        let delta = (self.timestamp - base_timestamp) as i32;
        w.write_all(&delta.to_le_bytes())?;
        written += 4;

        // Presence bitmap
        w.write_all(&self.presence.0.to_le_bytes())?;
        written += 2;

        // Dictionary IDs for present fields (u32 LE each)
        for &id in &self.dict_ids {
            w.write_all(&id.to_le_bytes())?;
            written += 4;
        }

        // Payload length and payload
        let payload_len = self.payload.len() as u32;
        w.write_all(&payload_len.to_le_bytes())?;
        written += 4;
        w.write_all(&self.payload)?;
        written += self.payload.len();

        Ok(written)
    }

    /// Decode an entry from a reader.
    pub fn read_from<R: Read>(mut r: R, base_timestamp: i64) -> io::Result<Self> {
        // Timestamp delta (i32 LE)
        let mut delta_bytes = [0u8; 4];
        r.read_exact(&mut delta_bytes)?;
        let delta = i32::from_le_bytes(delta_bytes) as i64;
        let timestamp = base_timestamp + delta;

        // Presence bitmap (u16 LE)
        let mut presence_bytes = [0u8; 2];
        r.read_exact(&mut presence_bytes)?;
        let presence = PresenceBitmap(u16::from_le_bytes(presence_bytes));

        // Dictionary IDs (u32 LE each)
        let num_ids = presence.count() as usize;
        let mut dict_ids = Vec::with_capacity(num_ids);
        for _ in 0..num_ids {
            let mut id_bytes = [0u8; 4];
            r.read_exact(&mut id_bytes)?;
            dict_ids.push(u32::from_le_bytes(id_bytes));
        }

        // Payload length (u32 LE) and payload
        let mut len_bytes = [0u8; 4];
        r.read_exact(&mut len_bytes)?;
        let payload_len = u32::from_le_bytes(len_bytes) as usize;
        let mut payload = vec![0u8; payload_len];
        r.read_exact(&mut payload)?;

        Ok(Self {
            timestamp,
            presence,
            dict_ids,
            payload,
        })
    }
}

/// Get the field index for a field name.
pub fn field_index(name: &str) -> Option<usize> {
    AUTO_INDEX_FIELDS.iter().position(|&f| f == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_presence_bitmap() {
        let mut bitmap = PresenceBitmap::new();
        assert_eq!(bitmap.count(), 0);

        bitmap.set(0);
        bitmap.set(3);
        bitmap.set(8);

        assert!(bitmap.is_set(0));
        assert!(!bitmap.is_set(1));
        assert!(bitmap.is_set(3));
        assert!(bitmap.is_set(8));
        assert_eq!(bitmap.count(), 3);

        let set: Vec<_> = bitmap.iter_set().collect();
        assert_eq!(set, vec![0, 3, 8]);
    }

    #[test]
    fn test_processed_entry_roundtrip() {
        let mut presence = PresenceBitmap::new();
        presence.set(0); // service
        presence.set(8); // level

        let entry = ProcessedEntry {
            timestamp: 1705323600123,
            presence,
            dict_ids: vec![5, 2], // service=5, level=2
            payload: b"{\"message\":\"test\"}".to_vec(),
        };

        let base_ts = 1705323600000;
        let mut buf = Vec::new();
        entry.write_to(&mut buf, base_ts).unwrap();

        let parsed = ProcessedEntry::read_from(Cursor::new(&buf), base_ts).unwrap();
        assert_eq!(parsed.timestamp, entry.timestamp);
        assert_eq!(parsed.presence, entry.presence);
        assert_eq!(parsed.dict_ids, entry.dict_ids);
        assert_eq!(parsed.payload, entry.payload);
    }

    #[test]
    fn test_processed_entry_negative_delta() {
        // Entry with timestamp before base (out-of-order log)
        let mut presence = PresenceBitmap::new();
        presence.set(0);

        let entry = ProcessedEntry {
            timestamp: 1705323599000, // 1 second before base
            presence,
            dict_ids: vec![1],
            payload: b"{}".to_vec(),
        };

        let base_ts = 1705323600000;
        let mut buf = Vec::new();
        entry.write_to(&mut buf, base_ts).unwrap();

        let parsed = ProcessedEntry::read_from(Cursor::new(&buf), base_ts).unwrap();
        assert_eq!(parsed.timestamp, entry.timestamp);
    }

    #[test]
    fn test_field_index() {
        assert_eq!(field_index("service"), Some(0));
        assert_eq!(field_index("level"), Some(8));
        assert_eq!(field_index("trace_id"), Some(10));
        assert_eq!(field_index("unknown"), None);
    }
}
