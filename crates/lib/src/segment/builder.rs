//! Segment builder for constructing L0 segments.
//!
//! # L0 Segment Binary Format
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │                     COMPRESSED ENTRIES                       │
//! │  zstd-compressed block containing all log entries            │
//! │                                                              │
//! │  Per-entry format (before compression):                      │
//! │  ┌────────────────┬──────────────┬───────────┬─────────────┐ │
//! │  │ ts_delta:varint│ presence:u16 │ dict_ids* │ payload     │ │
//! │  │ (zigzag)       │ (LE)         │ (varints) │ (len+bytes) │ │
//! │  └────────────────┴──────────────┴───────────┴─────────────┘ │
//! ├──────────────────────────────────────────────────────────────┤
//! │                        DICTIONARY                            │
//! │  ┌─────────────┬─────────────────────────────────────────┐   │
//! │  │ count:varint│ entries: [len:varint, bytes]*           │   │
//! │  └─────────────┴─────────────────────────────────────────┘   │
//! ├──────────────────────────────────────────────────────────────┤
//! │                      BLOOM FILTERS                           │
//! │  ┌────────────┬──────────────────────────────────────────┐   │
//! │  │ count:u8   │ filters: [field_id:u8, bloom_bytes]*     │   │
//! │  └────────────┴──────────────────────────────────────────┘   │
//! ├──────────────────────────────────────────────────────────────┤
//! │                         FOOTER                               │
//! │  ┌───────────┬─────────┬─────────┬────────────┬───────────┐  │
//! │  │ log_count │ min_ts  │ max_ts  │entries_size│ dict_off  │  │
//! │  │ u32 LE    │ i64 LE  │ i64 LE  │ u32 LE     │ u32 LE    │  │
//! │  ├───────────┴─────────┴─────────┴────────────┴───────────┤  │
//! │  │ bloom_off │ checksum │  magic                          │  │
//! │  │ u32 LE    │ u32 LE   │  "SYBL" (0x5359424C)            │  │
//! │  └───────────┴──────────┴─────────────────────────────────┘  │
//! │                        (40 bytes fixed)                      │
//! └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! Read sequence: fetch footer (last 40 bytes) → validate magic/checksum →
//! use offsets to read dictionary and blooms → decompress entries.

use std::collections::HashMap;
use std::io::{self, Cursor, Write};

use super::bloom::BloomFilter;
use super::dictionary::Dictionary;
use super::entry::{Block, PresenceBitmap, ProcessedEntry, AUTO_INDEX_FIELDS};
use super::footer::{compute_checksum, FOOTER_SIZE};

pub struct Builder {
    entries: Block,
    dictionary: Dictionary,
    blooms: Vec<BloomFilter>,
    bloom_values: Vec<std::collections::HashSet<String>>,
}

impl Builder {
    pub fn new() -> Self {
        let num_fields = AUTO_INDEX_FIELDS.len();
        Self {
            entries: Block::new(),
            dictionary: Dictionary::new(),
            blooms: (0..num_fields).map(|_| BloomFilter::new(1000)).collect(),
            bloom_values: (0..num_fields)
                .map(|_| std::collections::HashSet::new())
                .collect(),
        }
    }

    /// Add a log entry. Extracts indexed fields from the JSON payload.
    pub fn add(&mut self, timestamp: i64, payload: &[u8]) -> Result<(), serde_json::Error> {
        let json: HashMap<String, serde_json::Value> = serde_json::from_slice(payload)?;

        let mut presence = PresenceBitmap::new();
        let mut dict_ids = Vec::new();

        for (i, &field_name) in AUTO_INDEX_FIELDS.iter().enumerate() {
            if let Some(value) = json.get(field_name) {
                if let Some(s) = value.as_str() {
                    presence.set(i);
                    let id = self.dictionary.get_or_insert(s);
                    dict_ids.push(id);

                    if !self.bloom_values[i].contains(s) {
                        self.blooms[i].insert(s.as_bytes());
                        self.bloom_values[i].insert(s.to_string());
                    }
                }
            }
        }

        self.entries.push(ProcessedEntry {
            timestamp,
            presence,
            dict_ids,
            payload: payload.to_vec(),
        });

        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Build the segment, returning the complete bytes.
    pub fn build(self) -> io::Result<Vec<u8>> {
        if self.entries.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot build empty segment",
            ));
        }

        let min_ts = self.entries.iter().map(|e| e.timestamp).min().unwrap();
        let max_ts = self.entries.iter().map(|e| e.timestamp).max().unwrap();

        // Encode entries
        let mut entries_buf = Vec::new();
        for entry in &self.entries {
            entry.write_to(&mut entries_buf, min_ts)?;
        }

        // Compress entries
        let compressed_entries = zstd::encode_all(Cursor::new(&entries_buf), 3)?;

        // Serialize dictionary
        let mut dict_buf = Vec::new();
        self.dictionary.write_to(&mut dict_buf)?;

        // Serialize bloom filters
        let mut bloom_buf = Vec::new();
        let active_blooms: Vec<_> = self
            .blooms
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.bloom_values[*i].is_empty())
            .collect();
        bloom_buf.write_all(&[active_blooms.len() as u8])?;
        for (field_id, bloom) in active_blooms {
            bloom_buf.write_all(&[field_id as u8])?;
            bloom.write_to(&mut bloom_buf)?;
        }

        // Calculate offsets
        let entries_size = compressed_entries.len() as u32;
        let dictionary_offset = entries_size;
        let bloom_offset = dictionary_offset + dict_buf.len() as u32;

        // Assemble segment data
        let mut segment = Vec::new();
        segment.extend_from_slice(&compressed_entries);
        segment.extend_from_slice(&dict_buf);
        segment.extend_from_slice(&bloom_buf);

        // Write partial footer (everything before checksum)
        segment.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        segment.extend_from_slice(&min_ts.to_le_bytes());
        segment.extend_from_slice(&max_ts.to_le_bytes());
        segment.extend_from_slice(&entries_size.to_le_bytes());
        segment.extend_from_slice(&dictionary_offset.to_le_bytes());
        segment.extend_from_slice(&bloom_offset.to_le_bytes());

        // Checksum covers everything so far
        let checksum = compute_checksum(&segment);
        segment.extend_from_slice(&checksum.to_le_bytes());
        segment.extend_from_slice(&super::footer::MAGIC.to_le_bytes());

        Ok(segment)
    }

    /// Estimate compressed size for flush decisions.
    pub fn estimated_size(&self) -> usize {
        let payload_size: usize = self.entries.iter().map(|e| e.payload.len()).sum();
        let dict_size = self.dictionary.len() * 20;
        let bloom_size = self.blooms.len() * 200;
        (payload_size / 5) + dict_size + bloom_size + FOOTER_SIZE
    }
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::footer::SegmentFooter;
    use super::*;

    #[test]
    fn test_builder_basic() {
        let mut builder = Builder::new();

        builder
            .add(
                1705323600000,
                br#"{"level":"error","service":"api","message":"test"}"#,
            )
            .unwrap();
        builder
            .add(
                1705323600100,
                br#"{"level":"warn","service":"api","message":"another"}"#,
            )
            .unwrap();

        assert_eq!(builder.len(), 2);

        let segment = builder.build().unwrap();
        assert!(!segment.is_empty());

        let footer_bytes = &segment[segment.len() - FOOTER_SIZE..];
        let footer = SegmentFooter::from_bytes(footer_bytes).unwrap();
        assert_eq!(footer.log_count, 2);
        assert_eq!(footer.min_timestamp, 1705323600000);
        assert_eq!(footer.max_timestamp, 1705323600100);
    }

    #[test]
    fn test_builder_checksum() {
        let mut builder = Builder::new();
        builder
            .add(1705323600000, br#"{"level":"info","message":"test"}"#)
            .unwrap();

        let segment = builder.build().unwrap();
        let footer_bytes = &segment[segment.len() - FOOTER_SIZE..];
        let footer = SegmentFooter::from_bytes(footer_bytes).unwrap();

        assert!(footer.validate_checksum(&segment));
    }

    #[test]
    fn test_builder_empty_fails() {
        let builder = Builder::new();
        assert!(builder.build().is_err());
    }
}
