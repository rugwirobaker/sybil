//! Dictionary encoding for string deduplication.
//!
//! Field values are deduplicated within a segment. Entries store small integer
//! IDs instead of repeated strings.

use std::collections::HashMap;
use std::io::{self, Read, Write};

use super::varint::{read_varint, write_varint};

/// A dictionary mapping strings to integer IDs and back.
#[derive(Debug, Clone, Default)]
pub struct Dictionary {
    /// String to ID mapping (for encoding).
    string_to_id: HashMap<String, u32>,
    /// ID to string mapping (for decoding).
    id_to_string: Vec<String>,
}

impl Dictionary {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or create an ID for a string value.
    pub fn get_or_insert(&mut self, value: &str) -> u32 {
        if let Some(&id) = self.string_to_id.get(value) {
            return id;
        }
        let id = self.id_to_string.len() as u32;
        self.id_to_string.push(value.to_string());
        self.string_to_id.insert(value.to_string(), id);
        id
    }

    /// Look up a string by ID (for decoding).
    pub fn get(&self, id: u32) -> Option<&str> {
        self.id_to_string.get(id as usize).map(|s| s.as_str())
    }

    /// Number of entries in the dictionary.
    pub fn len(&self) -> usize {
        self.id_to_string.len()
    }

    pub fn is_empty(&self) -> bool {
        self.id_to_string.is_empty()
    }

    /// Serialize the dictionary.
    /// Format: entry_count: varint, then for each entry: len: varint, bytes
    pub fn write_to<W: Write>(&self, mut w: W) -> io::Result<usize> {
        let mut written = 0;
        written += write_varint(&mut w, self.id_to_string.len() as u64)?;
        for s in &self.id_to_string {
            let bytes = s.as_bytes();
            written += write_varint(&mut w, bytes.len() as u64)?;
            w.write_all(bytes)?;
            written += bytes.len();
        }
        Ok(written)
    }

    /// Deserialize a dictionary.
    pub fn read_from<R: Read>(mut r: R) -> io::Result<Self> {
        let count = read_varint(&mut r)? as usize;
        let mut dict = Dictionary::new();

        for _ in 0..count {
            let len = read_varint(&mut r)? as usize;
            let mut buf = vec![0u8; len];
            r.read_exact(&mut buf)?;
            let s = String::from_utf8(buf)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            dict.get_or_insert(&s);
        }

        Ok(dict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_dictionary_get_or_insert() {
        let mut dict = Dictionary::new();

        let id1 = dict.get_or_insert("error");
        let id2 = dict.get_or_insert("warn");
        let id3 = dict.get_or_insert("error"); // duplicate

        assert_eq!(id1, 0);
        assert_eq!(id2, 1);
        assert_eq!(id3, 0); // same as first
        assert_eq!(dict.len(), 2);
    }

    #[test]
    fn test_dictionary_get() {
        let mut dict = Dictionary::new();
        dict.get_or_insert("error");
        dict.get_or_insert("warn");

        assert_eq!(dict.get(0), Some("error"));
        assert_eq!(dict.get(1), Some("warn"));
        assert_eq!(dict.get(2), None);
    }

    #[test]
    fn test_dictionary_roundtrip() {
        let mut dict = Dictionary::new();
        dict.get_or_insert("error");
        dict.get_or_insert("warn");
        dict.get_or_insert("info");
        dict.get_or_insert("api-gateway");

        let mut buf = Vec::new();
        dict.write_to(&mut buf).unwrap();

        let parsed = Dictionary::read_from(Cursor::new(&buf)).unwrap();
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed.get(0), Some("error"));
        assert_eq!(parsed.get(1), Some("warn"));
        assert_eq!(parsed.get(2), Some("info"));
        assert_eq!(parsed.get(3), Some("api-gateway"));
    }

    #[test]
    fn test_dictionary_empty() {
        let dict = Dictionary::new();
        let mut buf = Vec::new();
        dict.write_to(&mut buf).unwrap();

        let parsed = Dictionary::read_from(Cursor::new(&buf)).unwrap();
        assert!(parsed.is_empty());
    }
}
