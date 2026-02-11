//! Segment: Binary log segment format.
//!
//! Segments are immutable, compressed containers for log entries. The format
//! is optimized for S3 access patterns with a footer-first read sequence.
//!
//! L0 segments (64-128KB) are flat with no internal block structure.
//! L1/L2 segments use 64KB blocks for selective reads.

mod bloom;
mod builder;
mod dictionary;
mod entry;
mod footer;
mod reader;
mod varint;

pub use bloom::BloomFilter;
pub use builder::Builder;
pub use dictionary::Dictionary;
pub use entry::{field_index, LogEntry, PresenceBitmap, ProcessedEntry, AUTO_INDEX_FIELDS};
pub use footer::{SegmentFooter, FOOTER_SIZE, MAGIC};
pub use reader::Reader;
pub use varint::{read_varint, write_varint, zigzag_decode, zigzag_encode};
