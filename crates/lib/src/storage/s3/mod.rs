//! S3-compatible object store implementation.
//!
//! Uses the `object_store` crate for S3 operations. Supports any S3-compatible
//! service (AWS S3, Tigris, MinIO, R2, etc.) via endpoint configuration.

use bytes::Bytes;
use object_store::aws::{AmazonS3Builder, S3ConditionalPut};
use object_store::path::Path;
use object_store::{ObjectStore as ObjectStoreExt, PutMode, PutOptions, PutPayload, UpdateVersion};

use super::engine::{ByteRange, ObjectStore, Result};
use super::error::StorageError;

/// S3-compatible object store.
///
/// Wraps `object_store::aws::AmazonS3` with our `ObjectStore` trait,
/// providing a consistent interface for S3-compatible storage backends.
pub struct S3Store {
    inner: object_store::aws::AmazonS3,
    bucket: String,
}

impl std::fmt::Debug for S3Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Store")
            .field("bucket", &self.bucket)
            .finish_non_exhaustive()
    }
}

impl S3Store {
    /// Create a new S3 store.
    ///
    /// For S3-compatible services (Tigris, MinIO, R2), provide the endpoint URL.
    /// For AWS S3, pass `None` for endpoint.
    pub fn new(
        bucket: &str,
        region: &str,
        endpoint: Option<&str>,
        access_key_id: &str,
        secret_access_key: &str,
    ) -> Result<Self> {
        let mut builder = AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_region(region)
            .with_access_key_id(access_key_id)
            .with_secret_access_key(secret_access_key);

        if let Some(endpoint) = endpoint {
            builder = builder.with_endpoint(endpoint);

            // Allow HTTP for local development/testing endpoints
            if endpoint.starts_with("http://") {
                builder = builder.with_allow_http(true);
            }
        }

        // Enable conditional put for immutability checks (if-none-match header)
        builder = builder.with_conditional_put(S3ConditionalPut::ETagMatch);

        let inner = builder.build().map_err(|e| StorageError::Io(e.into()))?;

        Ok(Self {
            inner,
            bucket: bucket.to_string(),
        })
    }

    /// Get the bucket name.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    fn path(key: &str) -> Path {
        Path::from(key)
    }

    fn convert_error(key: &str, err: object_store::Error) -> StorageError {
        match &err {
            object_store::Error::NotFound { .. } => StorageError::not_found(key),
            object_store::Error::AlreadyExists { .. } => StorageError::already_exists(key),
            object_store::Error::Precondition { .. } => StorageError::conflict(key),
            object_store::Error::Generic { source, .. } => {
                // Check for S3 InvalidRange error (HTTP 416)
                let source_str = source.to_string();
                if source_str.contains("416") || source_str.contains("InvalidRange") {
                    StorageError::invalid_range(key, "range not satisfiable")
                } else {
                    StorageError::Io(err.into())
                }
            }
            _ => StorageError::Io(err.into()),
        }
    }

    async fn object_size(&self, key: &str) -> Result<usize> {
        let meta = self
            .inner
            .head(&Self::path(key))
            .await
            .map_err(|e| Self::convert_error(key, e))?;
        Ok(meta.size)
    }
}

#[async_trait::async_trait]
impl ObjectStore for S3Store {
    async fn get(&self, key: &str) -> Result<Bytes> {
        let result = self
            .inner
            .get(&Self::path(key))
            .await
            .map_err(|e| Self::convert_error(key, e))?;

        result
            .bytes()
            .await
            .map_err(|e| Self::convert_error(key, e))
    }

    async fn get_with_etag(&self, key: &str) -> Result<(Bytes, String)> {
        let result = self
            .inner
            .get(&Self::path(key))
            .await
            .map_err(|e| Self::convert_error(key, e))?;

        let etag = result.meta.e_tag.clone().unwrap_or_default();

        let bytes = result
            .bytes()
            .await
            .map_err(|e| Self::convert_error(key, e))?;

        Ok((bytes, etag))
    }

    async fn get_range(&self, key: &str, range: ByteRange) -> Result<Bytes> {
        let path = Self::path(key);

        let result = match range {
            ByteRange::Range { start, end } => {
                if start > end {
                    return Err(StorageError::invalid_range(
                        key,
                        format!("start ({start}) > end ({end})"),
                    ));
                }
                // object_store uses exclusive end, our API uses inclusive end
                self.inner
                    .get_range(&path, start as usize..(end + 1) as usize)
                    .await
            }
            ByteRange::From(offset) => {
                let size = self.object_size(key).await?;

                if offset as usize >= size {
                    return Err(StorageError::invalid_range(
                        key,
                        format!("offset {offset} >= object size {size}"),
                    ));
                }

                self.inner.get_range(&path, offset as usize..size).await
            }
            ByteRange::Suffix(len) => {
                if len == 0 {
                    return Err(StorageError::invalid_range(key, "suffix length is 0"));
                }

                let size = self.object_size(key).await?;

                // S3 behavior: return entire object if suffix > size
                let start = size.saturating_sub(len as usize);

                self.inner.get_range(&path, start..size).await
            }
        };

        result.map_err(|e| Self::convert_error(key, e))
    }

    async fn put(&self, key: &str, data: Bytes) -> Result<()> {
        // Use PutMode::Create - objects are immutable, never overwrite
        let opts = PutOptions {
            mode: PutMode::Create,
            ..Default::default()
        };

        self.inner
            .put_opts(&Self::path(key), PutPayload::from(data), opts)
            .await
            .map_err(|e| Self::convert_error(key, e))?;

        Ok(())
    }

    async fn put_if_match(&self, key: &str, data: Bytes, etag: &str) -> Result<()> {
        let opts = PutOptions {
            mode: PutMode::Update(UpdateVersion {
                e_tag: Some(etag.to_string()),
                version: None,
            }),
            ..Default::default()
        };

        self.inner
            .put_opts(&Self::path(key), PutPayload::from(data), opts)
            .await
            .map_err(|e| Self::convert_error(key, e))?;

        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.inner
            .delete(&Self::path(key))
            .await
            .map_err(|e| Self::convert_error(key, e))?;

        Ok(())
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        use futures::TryStreamExt;

        let prefix_path = Self::path(prefix);
        let stream = self.inner.list(Some(&prefix_path));

        let keys: Vec<String> = stream
            .map_ok(|meta| meta.location.to_string())
            .try_collect()
            .await
            .map_err(|e| Self::convert_error(prefix, e))?;

        Ok(keys)
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        match self.inner.head(&Self::path(key)).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(Self::convert_error(key, e)),
        }
    }
}

#[cfg(test)]
mod tests;
