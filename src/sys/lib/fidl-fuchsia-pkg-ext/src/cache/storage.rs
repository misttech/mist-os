// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    super::{OpenBlobError, TruncateBlobError, WriteBlobError},
    anyhow::Context as _,
    fidl_fuchsia_fxfs as ffxfs, fidl_fuchsia_io as fio, fidl_fuchsia_pkg as fpkg,
    fuchsia_zircon_status::Status,
};

pub(super) fn into_blob_writer_and_closer(
    fidl: fpkg::BlobWriter,
) -> Result<(Box<dyn Writer>, Box<dyn Closer>), OpenBlobError> {
    use fpkg::BlobWriter::*;
    match fidl {
        File(file) => {
            let proxy = file.into_proxy()?;
            Ok((Box::new(Clone::clone(&proxy)), Box::new(proxy)))
        }
        Writer(writer) => {
            // fuchsia.fxfs/BlobCreator allows concurrent creation attempts, so we don't need to
            // cancel an ongoing attempt before trying again.
            Ok((Box::new(FxBlob::new(writer.into_proxy()?)), Box::new(())))
        }
    }
}

#[async_trait::async_trait]
pub(super) trait Closer: Send + Sync + std::fmt::Debug {
    /// Close the blob to enable immediate retry of create and write.
    async fn close(&mut self);

    /// Attempt to close the blob. Function may return before blob is closed if closing requires
    /// async.
    fn best_effort_close(&mut self);
}

#[async_trait::async_trait]
impl Closer for fio::FileProxy {
    async fn close(&mut self) {
        let _: Result<Result<(), i32>, fidl::Error> = fio::FileProxy::close(self).await;
    }

    fn best_effort_close(&mut self) {
        let _: fidl::client::QueryResponseFut<Result<(), i32>> = fio::FileProxy::close(self);
    }
}

// fuchsia.fxfs/BlobCreator allows concurrent creation attempts, so we don't need to cancel an
// ongoing attempt before trying again.
#[async_trait::async_trait]
impl Closer for () {
    async fn close(&mut self) {}

    fn best_effort_close(&mut self) {}
}

#[async_trait::async_trait]
pub(super) trait Writer: Send + std::fmt::Debug {
    /// Set the size of the blob.
    /// If the blob is size zero, the returned Future should not complete until the blob
    /// is readable.
    async fn truncate(&mut self, size: u64) -> Result<(), TruncateBlobError>;
    /// Write `bytes` to the blob.
    /// The Future returned by the `write` call that writes the final bytes should
    /// not complete until the blob is readable.
    async fn write(
        &mut self,
        bytes: &[u8],
        after_write: &(dyn Fn(u64) + Send + Sync),
        after_write_ack: &(dyn Fn() + Send + Sync),
    ) -> Result<(), WriteBlobError>;
}

#[async_trait::async_trait]
impl Writer for fio::FileProxy {
    async fn truncate(&mut self, size: u64) -> Result<(), TruncateBlobError> {
        self.resize(size).await?.map_err(|i| match Status::from_raw(i) {
            Status::NO_SPACE => TruncateBlobError::NoSpace,
            other => TruncateBlobError::UnexpectedResponse(other),
        })
    }

    async fn write(
        &mut self,
        mut bytes: &[u8],
        after_write: &(dyn Fn(u64) + Send + Sync),
        after_write_ack: &(dyn Fn() + Send + Sync),
    ) -> Result<(), WriteBlobError> {
        while !bytes.is_empty() {
            let limit = bytes.len().min(fio::MAX_BUF as usize);

            let result_fut = fio::FileProxy::write(self, &bytes[..limit]);
            after_write(bytes.len() as u64);

            let result = result_fut.await;
            after_write_ack();

            let written = result?.map_err(|i| match Status::from_raw(i) {
                Status::IO_DATA_INTEGRITY => WriteBlobError::Corrupt,
                Status::NO_SPACE => WriteBlobError::NoSpace,
                other => WriteBlobError::UnexpectedResponse(other),
            })? as usize;

            if written > bytes.len() {
                return Err(WriteBlobError::Overwrite);
            }
            bytes = &bytes[written..];
        }

        Ok(())
    }
}

#[derive(Debug)]
enum FxBlob {
    NeedsTruncate(ffxfs::BlobWriterProxy),
    NeedsBytes(blob_writer::BlobWriter),
    Invalid,
}

impl FxBlob {
    fn new(proxy: ffxfs::BlobWriterProxy) -> Self {
        Self::NeedsTruncate(proxy)
    }

    fn state_str(&self) -> &'static str {
        match self {
            Self::NeedsTruncate(_) => "needs truncate",
            Self::NeedsBytes(_) => "needs bytes",
            Self::Invalid => "invalid",
        }
    }
}

#[async_trait::async_trait]
impl Writer for FxBlob {
    async fn truncate(&mut self, size: u64) -> Result<(), TruncateBlobError> {
        *self = match std::mem::replace(self, Self::Invalid) {
            Self::NeedsTruncate(proxy) => Self::NeedsBytes(
                blob_writer::BlobWriter::create(proxy, size)
                    .await
                    .context("creating a BlobWriter")
                    .map_err(TruncateBlobError::Other)?,
            ),
            Self::NeedsBytes(_) => {
                return Err(TruncateBlobError::AlreadyTruncated(self.state_str()))
            }
            Self::Invalid => return Err(TruncateBlobError::BadState),
        };
        Ok(())
    }

    async fn write(
        &mut self,
        bytes: &[u8],
        after_write: &(dyn Fn(u64) + Send + Sync),
        after_write_ack: &(dyn Fn() + Send + Sync),
    ) -> Result<(), WriteBlobError> {
        let Self::NeedsBytes(writer) = self else {
            return Err(WriteBlobError::BytesNotNeeded(self.state_str()));
        };
        let fut = writer.write(bytes);
        let () = after_write(bytes.len() as u64);
        let res = fut.await;
        let () = after_write_ack();
        res.context("calling write on BlobWriter").map_err(WriteBlobError::Other)
    }
}

#[cfg(test)]
mod tests {
    use {super::*, futures::stream::TryStreamExt as _};

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_proxy_chunks_writes() {
        let (mut proxy, mut server) =
            fidl::endpoints::create_proxy_and_stream::<fio::FileMarker>().unwrap();
        let bytes = vec![0; fio::MAX_BUF as usize + 1];

        let write_fut = async move {
            <fio::FileProxy as Writer>::write(&mut proxy, &bytes, &|_| (), &|| ()).await.unwrap()
        };
        let server_fut = async move {
            match server.try_next().await.unwrap().unwrap() {
                fio::FileRequest::Write { data, responder } => {
                    // Proxy limited writes to MAX_BUF bytes.
                    assert_eq!(data, vec![0; fio::MAX_BUF as usize]);
                    let () = responder.send(Ok(fio::MAX_BUF)).unwrap();
                }
                req => panic!("unexpected request {req:?}"),
            }
            match server.try_next().await.unwrap().unwrap() {
                fio::FileRequest::Write { data, responder } => {
                    assert_eq!(data, vec![0; 1]);
                    let () = responder.send(Ok(1)).unwrap();
                }
                req => panic!("unexpected request {req:?}"),
            }
            assert!(server.try_next().await.unwrap().is_none());
        };

        let ((), ()) = futures::future::join(write_fut, server_fut).await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_proxy_handles_short_writes() {
        let (mut proxy, mut server) =
            fidl::endpoints::create_proxy_and_stream::<fio::FileMarker>().unwrap();
        let bytes = [0; 10];

        let write_fut = async move {
            <fio::FileProxy as Writer>::write(&mut proxy, &bytes, &|_| (), &|| ()).await.unwrap()
        };
        let server_fut = async move {
            match server.try_next().await.unwrap().unwrap() {
                fio::FileRequest::Write { data, responder } => {
                    assert_eq!(data, [0; 10]);
                    // Ack only 8 of the 10 bytes.
                    let () = responder.send(Ok(8)).unwrap();
                }
                req => panic!("unexpected request {req:?}"),
            }
            match server.try_next().await.unwrap().unwrap() {
                fio::FileRequest::Write { data, responder } => {
                    assert_eq!(data, [0; 2]);
                    let () = responder.send(Ok(2)).unwrap();
                }
                req => panic!("unexpected request {req:?}"),
            }
            assert!(server.try_next().await.unwrap().is_none());
        };

        let ((), ()) = futures::future::join(write_fut, server_fut).await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn fxblob_writer() {
        let blobfs = blobfs_ramdisk::BlobfsRamdisk::builder().fxblob().start().await.unwrap();
        assert_eq!(blobfs.list_blobs().unwrap(), std::collections::BTreeSet::new());
        let contents = [0u8; 7];
        let hash = fuchsia_merkle::MerkleTree::from_reader(&contents[..]).unwrap().root();
        let compressed = delivery_blob::Type1Blob::generate(
            &contents[..],
            delivery_blob::CompressionMode::Attempt,
        );
        let writer = blobfs
            .blob_creator_proxy()
            .unwrap()
            .unwrap()
            .create(&hash.into(), false)
            .await
            .unwrap()
            .unwrap();

        let (mut writer, _closer) =
            into_blob_writer_and_closer(fpkg::BlobWriter::Writer(writer)).unwrap();
        let () = writer.truncate(compressed.len().try_into().unwrap()).await.unwrap();
        let () = writer.write(&compressed, &|_| (), &|| ()).await.unwrap();

        assert_eq!(blobfs.list_blobs().unwrap(), std::collections::BTreeSet::from([hash]));

        let () = blobfs.stop().await.unwrap();
    }
}
