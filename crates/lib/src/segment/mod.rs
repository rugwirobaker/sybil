//! Segment: Binary log segment format.
//!
//! Segments are immutable, compressed containers for log entries. The format
//! is optimized for S3 access patterns with a footer-first read sequence.
//!
//! L0 segments (64-128KB) are flat with no internal block structure.
//! L1/L2 segments use 64KB blocks for selective reads.
//!
//! All multi-byte integers use little-endian encoding with length prefixes
//! for variable-length fields.

mod bloom;
mod builder;
mod dictionary;
mod entry;
mod footer;
mod reader;

pub use bloom::BloomFilter;
pub use builder::Builder;
pub use dictionary::Dictionary;
pub use entry::{field_index, Block, LogEntry, PresenceBitmap, ProcessedEntry, AUTO_INDEX_FIELDS};
pub use footer::{SegmentFooter, FOOTER_SIZE, MAGIC};
pub use reader::Reader;

/// External metadata for a segment.
///
/// This is the index/catalog view of a segment, containing the key information
/// needed to locate and filter segments without reading the full file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentMeta {
    /// S3 key or relative path to the segment file.
    pub key: String,
    /// Total file size in bytes.
    pub size_bytes: u64,
    /// Minimum timestamp (epoch millis) of entries in this segment.
    pub min_timestamp: i64,
    /// Maximum timestamp (epoch millis) of entries in this segment.
    pub max_timestamp: i64,
    /// Number of log entries in this segment.
    pub entry_count: u32,
}

impl SegmentMeta {
    /// Create metadata from a key, size, and parsed footer.
    pub fn from_footer(key: impl Into<String>, size_bytes: u64, footer: &SegmentFooter) -> Self {
        Self {
            key: key.into(),
            size_bytes,
            min_timestamp: footer.min_timestamp,
            max_timestamp: footer.max_timestamp,
            entry_count: footer.log_count,
        }
    }

    /// Check if this segment's time range overlaps with a query range.
    pub fn overlaps(&self, start: i64, end: i64) -> bool {
        self.min_timestamp <= end && self.max_timestamp >= start
    }
}
