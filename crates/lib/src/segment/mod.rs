//! Segment: Binary log segment format.
//!
//! Segments are immutable, compressed containers for log entries. The format
//! is optimized for S3 access patterns with a footer-first read sequence.
//!
//! L0 segments (64-128KB) are flat with no internal block structure.
//! L1/L2 segments use 64KB blocks for selective reads.

mod varint;
mod dictionary;
mod bloom;
mod entry;
mod footer;
mod builder;
mod reader;

pub use varint::{read_varint, write_varint, zigzag_decode, zigzag_encode};
pub use dictionary::Dictionary;
pub use bloom::BloomFilter;
pub use entry::{field_index, LogEntry, ProcessedEntry, PresenceBitmap, AUTO_INDEX_FIELDS};
pub use footer::{SegmentFooter, FOOTER_SIZE, MAGIC};
pub use builder::Builder;
pub use reader::Reader;
