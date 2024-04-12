// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements fuchsia.fxfs/BlobReader for reading blobs as VMOs from a [`BlobDirectory`].

use {
    crate::fxblob::directory::BlobDirectory,
    anyhow::Error,
    fuchsia_hash::Hash,
    fuchsia_zircon::{self as zx},
    fxfs::errors::FxfsError,
    std::sync::Arc,
};

impl BlobDirectory {
    /// Get a pager-backed VMO for the blob identified by `hash` in this [`BlobDirectory`]. The blob
    /// cannot be purged until all VMOs returned by this function are destroyed.
    pub async fn get_blob_vmo(self: &Arc<Self>, hash: Hash) -> Result<zx::Vmo, Error> {
        let blob = self.open_blob(&hash.into()).await?.ok_or(FxfsError::NotFound)?;
        let vmo = blob.create_child_vmo()?;
        Ok(vmo)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::fuchsia::fxblob::testing::{new_blob_fixture, BlobFixture},
        delivery_blob::CompressionMode,
        fidl_fuchsia_io::{self as fio},
        fuchsia_async as fasync,
        fuchsia_component::client::connect_to_protocol_at_dir_svc,
    };

    /// Read a blob using BlobReader API and return its contents as a boxed slice.
    async fn read_blob(
        blob_volume_outgoing_dir: &fio::DirectoryProxy,
        hash: Hash,
    ) -> Result<Vec<u8>, Error> {
        let blob_proxy = connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::BlobReaderMarker>(
            &blob_volume_outgoing_dir,
        )
        .expect("failed to connect to the BlobReader service");
        let vmo = blob_proxy
            .get_vmo(&hash.into())
            .await
            .expect("transport error on blobreader")
            .map_err(zx::Status::from_raw)?;
        let vmo_size = vmo.get_content_size().expect("failed to get vmo size") as usize;
        let mut buf = vec![0; vmo_size];
        vmo.read(&mut buf[..], 0)?;
        Ok(buf)
    }

    #[fasync::run(10, test)]
    async fn test_blob_reader_uncompressed() {
        const NEVER_COMPRESS: CompressionMode = CompressionMode::Never;
        let fixture = new_blob_fixture().await;
        let empty_blob_hash = fixture.write_blob(&[], NEVER_COMPRESS).await;
        let short_data = b"This is some data";
        let short_blob_hash = fixture.write_blob(short_data, NEVER_COMPRESS).await;
        let long_data = &[0x65u8; 30000];
        let long_blob_hash = fixture.write_blob(long_data, NEVER_COMPRESS).await;

        assert_eq!(
            &*read_blob(fixture.volume_out_dir(), empty_blob_hash).await.expect("read empty"),
            &[0u8; 0]
        );
        assert_eq!(
            &*read_blob(fixture.volume_out_dir(), short_blob_hash).await.expect("read short"),
            short_data
        );
        assert_eq!(
            &*read_blob(fixture.volume_out_dir(), long_blob_hash).await.expect("read long"),
            long_data
        );
        let missing_hash = Hash::from([0x77u8; 32]);
        assert!(read_blob(fixture.volume_out_dir(), missing_hash).await.is_err());

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_blob_reader_compressed() {
        const ALWAYS_COMPRESS: CompressionMode = CompressionMode::Always;
        let fixture = new_blob_fixture().await;
        let empty_blob_hash = fixture.write_blob(&[], ALWAYS_COMPRESS).await;
        let short_data = b"This is some data";
        let short_blob_hash = fixture.write_blob(short_data, ALWAYS_COMPRESS).await;
        let long_data = &[0x65u8; 30000];
        let long_blob_hash = fixture.write_blob(long_data, ALWAYS_COMPRESS).await;

        assert_eq!(
            &*read_blob(fixture.volume_out_dir(), empty_blob_hash).await.expect("read empty"),
            &[0u8; 0]
        );
        assert_eq!(
            &*read_blob(fixture.volume_out_dir(), short_blob_hash).await.expect("read short"),
            short_data
        );
        assert_eq!(
            &*read_blob(fixture.volume_out_dir(), long_blob_hash).await.expect("read long"),
            long_data
        );
        let missing_hash = Hash::from([0x77u8; 32]);
        assert!(read_blob(fixture.volume_out_dir(), missing_hash).await.is_err());

        fixture.close().await;
    }
}
