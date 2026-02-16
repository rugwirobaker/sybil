mod engine;
mod error;
mod filesystem;
mod memory;
mod s3;

#[cfg(any(test, feature = "test-harness"))]
pub mod test_harness;

pub use engine::{ByteRange, ObjectKey, ObjectStore, Result};
pub use error::StorageError;
pub use filesystem::FilesystemStore;
pub use memory::MemoryStore;
pub use s3::S3Store;

use std::sync::Arc;

use crate::config::{StorageBackend, StorageConfig};

/// Create an `ObjectStore` from configuration.
pub async fn from_config(
    config: &StorageConfig,
) -> std::result::Result<Arc<dyn ObjectStore>, StorageError> {
    match config.backend {
        StorageBackend::Memory => Ok(Arc::new(MemoryStore::new())),
        StorageBackend::S3 => {
            let s3 = &config.s3;
            let store = S3Store::new(
                &s3.bucket,
                &s3.region,
                Some(&s3.endpoint),
                &s3.access_key_id,
                &s3.secret_access_key,
            )?;
            Ok(Arc::new(store))
        }
        StorageBackend::Filesystem => {
            let store = FilesystemStore::new(&config.filesystem.root).await?;
            Ok(Arc::new(store))
        }
    }
}
