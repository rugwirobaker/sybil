//! Segment reader for decoding L0 segments.

use std::io::{self, Cursor};

use super::bloom::BloomFilter;
use super::dictionary::Dictionary;
use super::entry::ProcessedEntry;
use super::footer::{SegmentFooter, FOOTER_SIZE};

/// Parsed segment ready for querying.
pub struct Reader {
    pub footer: SegmentFooter,
    pub dictionary: Dictionary,
    pub blooms: Vec<(u8, BloomFilter)>,
    entries: Vec<ProcessedEntry>,
}

impl Reader {
    /// Parse a complete segment from bytes.
    pub fn from_bytes(data: &[u8]) -> io::Result<Self> {
        if data.len() < FOOTER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "segment too small for footer",
            ));
        }

        // Parse footer
        let footer_start = data.len() - FOOTER_SIZE;
        let footer = SegmentFooter::from_bytes(&data[footer_start..])?;

        // Validate checksum
        if !footer.validate_checksum(data) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "checksum mismatch",
            ));
        }

        // Parse dictionary
        let dict_start = footer.dictionary_offset as usize;
        let dict_end = footer.bloom_offset as usize;
        let dictionary = Dictionary::read_from(Cursor::new(&data[dict_start..dict_end]))?;

        // Parse bloom filters
        let bloom_start = footer.bloom_offset as usize;
        let bloom_end = footer_start;
        let mut blooms = Vec::new();
        if bloom_start < bloom_end {
            let mut cursor = Cursor::new(&data[bloom_start..bloom_end]);
            let mut count_buf = [0u8; 1];
            io::Read::read_exact(&mut cursor, &mut count_buf)?;
            let count = count_buf[0] as usize;

            for _ in 0..count {
                let mut field_buf = [0u8; 1];
                io::Read::read_exact(&mut cursor, &mut field_buf)?;
                let field_id = field_buf[0];
                let bloom = BloomFilter::read_from(&mut cursor)?;
                blooms.push((field_id, bloom));
            }
        }

        // Decompress and parse entries
        let compressed = &data[..footer.entries_size as usize];
        let decompressed = zstd::decode_all(Cursor::new(compressed))?;

        let mut entries = Vec::with_capacity(footer.log_count as usize);
        let mut cursor = Cursor::new(&decompressed);
        for _ in 0..footer.log_count {
            let entry = ProcessedEntry::read_from(&mut cursor, footer.min_timestamp)?;
            entries.push(entry);
        }

        Ok(Self {
            footer,
            dictionary,
            blooms,
            entries,
        })
    }

    /// Check if a field might contain a value using bloom filters.
    pub fn bloom_check(&self, field_id: u8, value: &[u8]) -> bool {
        for (fid, bloom) in &self.blooms {
            if *fid == field_id {
                return bloom.might_contain(value);
            }
        }
        // No bloom for this field, assume it might be present
        true
    }

    /// Iterate over all entries.
    pub fn entries(&self) -> impl Iterator<Item = &ProcessedEntry> {
        self.entries.iter()
    }

    /// Get entry count.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Resolve a dictionary ID to its string value.
    pub fn resolve(&self, dict_id: u32) -> Option<&str> {
        self.dictionary.get(dict_id)
    }
}

#[cfg(test)]
mod tests {
    use super::super::builder::Builder;
    use super::*;

    #[test]
    fn test_reader_roundtrip() {
        let mut builder = Builder::new();
        builder
            .add(
                1705323600000,
                br#"{"level":"error","service":"api","message":"hello"}"#,
            )
            .unwrap();
        builder
            .add(
                1705323600100,
                br#"{"level":"warn","service":"web","message":"world"}"#,
            )
            .unwrap();

        let segment = builder.build().unwrap();
        let reader = Reader::from_bytes(&segment).unwrap();

        assert_eq!(reader.len(), 2);
        assert_eq!(reader.footer.log_count, 2);

        let entries: Vec<_> = reader.entries().collect();
        assert_eq!(entries[0].timestamp, 1705323600000);
        assert_eq!(entries[1].timestamp, 1705323600100);
    }

    #[test]
    fn test_reader_bloom_check() {
        let mut builder = Builder::new();
        builder
            .add(1705323600000, br#"{"level":"error","service":"api"}"#)
            .unwrap();

        let segment = builder.build().unwrap();
        let reader = Reader::from_bytes(&segment).unwrap();

        // level is field 8
        assert!(reader.bloom_check(8, b"error"));
        // service is field 0
        assert!(reader.bloom_check(0, b"api"));
    }

    #[test]
    fn test_reader_dictionary_resolve() {
        let mut builder = Builder::new();
        builder
            .add(1705323600000, br#"{"level":"info","service":"gateway"}"#)
            .unwrap();

        let segment = builder.build().unwrap();
        let reader = Reader::from_bytes(&segment).unwrap();

        // The dictionary should contain "info" and "gateway"
        let entries: Vec<_> = reader.entries().collect();
        let entry = entries[0];

        // Resolve dict IDs from the entry
        for &dict_id in &entry.dict_ids {
            let value = reader.resolve(dict_id);
            assert!(value.is_some());
        }
    }
}
