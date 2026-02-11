//! Bloom filter for probabilistic set membership.
//!
//! Used to quickly check if a segment might contain a value for an indexed
//! field. False positives are possible, but false negatives are not.

use std::io::{self, Read, Write};

use super::varint::{read_varint, write_varint};

/// A simple bloom filter implementation.
#[derive(Debug, Clone)]
pub struct BloomFilter {
    bits: Vec<u64>,
    num_bits: usize,
    num_hashes: u32,
}

impl BloomFilter {
    /// Create a new bloom filter sized for the expected number of items.
    /// Uses ~10 bits per item for ~1% false positive rate.
    pub fn new(expected_items: usize) -> Self {
        // ~10 bits per item gives ~1% FPR
        let num_bits = (expected_items * 10).max(64);
        // Round up to multiple of 64
        let num_bits = (num_bits + 63) & !63;
        let num_words = num_bits / 64;

        Self {
            bits: vec![0u64; num_words],
            num_bits,
            num_hashes: 7, // optimal for 10 bits/item
        }
    }

    /// Create a bloom filter with specific size (in bits).
    pub fn with_size(num_bits: usize, num_hashes: u32) -> Self {
        let num_bits = (num_bits + 63) & !63;
        let num_words = num_bits / 64;

        Self {
            bits: vec![0u64; num_words],
            num_bits,
            num_hashes,
        }
    }

    /// Add a value to the filter.
    pub fn insert(&mut self, value: &[u8]) {
        let (h1, h2) = self.hash(value);
        for i in 0..self.num_hashes {
            let bit = self.get_bit_index(h1, h2, i);
            self.set_bit(bit);
        }
    }

    /// Check if a value might be in the set.
    /// Returns false if definitely not present, true if maybe present.
    pub fn might_contain(&self, value: &[u8]) -> bool {
        let (h1, h2) = self.hash(value);
        for i in 0..self.num_hashes {
            let bit = self.get_bit_index(h1, h2, i);
            if !self.get_bit(bit) {
                return false;
            }
        }
        true
    }

    /// Compute two independent hashes for double hashing.
    fn hash(&self, value: &[u8]) -> (u64, u64) {
        // Simple FNV-1a based hashing
        let h1 = fnv1a(value);
        // Second hash: rotate and mix
        let h2 = fnv1a(&[&h1.to_le_bytes()[..], value].concat());
        (h1, h2)
    }

    /// Get bit index using double hashing: h1 + i * h2
    fn get_bit_index(&self, h1: u64, h2: u64, i: u32) -> usize {
        (h1.wrapping_add((i as u64).wrapping_mul(h2)) % self.num_bits as u64) as usize
    }

    fn set_bit(&mut self, bit: usize) {
        let word = bit / 64;
        let offset = bit % 64;
        self.bits[word] |= 1 << offset;
    }

    fn get_bit(&self, bit: usize) -> bool {
        let word = bit / 64;
        let offset = bit % 64;
        (self.bits[word] >> offset) & 1 == 1
    }

    /// Serialize the bloom filter.
    /// Format: num_bits: varint, num_hashes: varint, bits: [u64 LE]
    pub fn write_to<W: Write>(&self, mut w: W) -> io::Result<usize> {
        let mut written = 0;
        written += write_varint(&mut w, self.num_bits as u64)?;
        written += write_varint(&mut w, self.num_hashes as u64)?;
        for &word in &self.bits {
            w.write_all(&word.to_le_bytes())?;
            written += 8;
        }
        Ok(written)
    }

    /// Deserialize a bloom filter.
    pub fn read_from<R: Read>(mut r: R) -> io::Result<Self> {
        let num_bits = read_varint(&mut r)? as usize;
        let num_hashes = read_varint(&mut r)? as u32;
        let num_words = num_bits / 64;

        let mut bits = vec![0u64; num_words];
        for word in &mut bits {
            let mut buf = [0u8; 8];
            r.read_exact(&mut buf)?;
            *word = u64::from_le_bytes(buf);
        }

        Ok(Self {
            bits,
            num_bits,
            num_hashes,
        })
    }

    /// Get the size in bytes when serialized.
    pub fn size_bytes(&self) -> usize {
        // varints + bits
        super::varint::varint_size(self.num_bits as u64)
            + super::varint::varint_size(self.num_hashes as u64)
            + self.bits.len() * 8
    }
}

/// FNV-1a hash function.
fn fnv1a(data: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_bloom_filter_basic() {
        let mut bloom = BloomFilter::new(100);

        bloom.insert(b"error");
        bloom.insert(b"warn");

        assert!(bloom.might_contain(b"error"));
        assert!(bloom.might_contain(b"warn"));
        // This might be a false positive, but unlikely for such a small set
        // We can't assert it's false, but we can test the logic works
    }

    #[test]
    fn test_bloom_filter_false_positive_rate() {
        let mut bloom = BloomFilter::new(1000);

        // Insert 1000 items
        for i in 0..1000u32 {
            bloom.insert(&i.to_le_bytes());
        }

        // Check all inserted items are found
        for i in 0..1000u32 {
            assert!(bloom.might_contain(&i.to_le_bytes()));
        }

        // Check false positive rate on items not inserted
        let mut false_positives = 0;
        for i in 1000..2000u32 {
            if bloom.might_contain(&i.to_le_bytes()) {
                false_positives += 1;
            }
        }

        // Should be around 1% (10 +/- some variance)
        assert!(
            false_positives < 50,
            "Too many false positives: {}",
            false_positives
        );
    }

    #[test]
    fn test_bloom_filter_roundtrip() {
        let mut bloom = BloomFilter::new(100);
        bloom.insert(b"error");
        bloom.insert(b"warn");
        bloom.insert(b"info");

        let mut buf = Vec::new();
        bloom.write_to(&mut buf).unwrap();

        let parsed = BloomFilter::read_from(Cursor::new(&buf)).unwrap();
        assert!(parsed.might_contain(b"error"));
        assert!(parsed.might_contain(b"warn"));
        assert!(parsed.might_contain(b"info"));
    }

    #[test]
    fn test_bloom_filter_empty() {
        let bloom = BloomFilter::new(10);
        // Empty filter should not contain anything
        assert!(!bloom.might_contain(b"anything"));
    }
}
