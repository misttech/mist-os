// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{
        fuchsia::errors::map_to_status,
        fxblob::{blob::FxBlob, directory::BlobDirectory},
    },
    anyhow::Error,
    fidl_fuchsia_io::{self as fio},
    fuchsia_hash::Hash,
    fuchsia_zircon::{self as zx},
    fxfs::errors::FxfsError,
    std::sync::Arc,
    vfs::{file::File, path::Path},
};

/// Implementation for VMO-backed FIDL interface for reading blobs
impl BlobDirectory {
    pub async fn get_blob_vmo(self: &Arc<Self>, hash: Hash) -> Result<zx::Vmo, Error> {
        let path = Path::validate_and_split(format!("{hash}"))?;
        let node =
            self.lookup(fio::OpenFlags::RIGHT_READABLE, path).await.map_err(map_to_status)?;
        let any_blob = node.clone().into_any();
        let blob = any_blob.downcast_ref::<FxBlob>().ok_or(FxfsError::Internal)?;
        let vmo = blob.get_backing_memory(fio::VmoFlags::READ).await?;
        Ok(vmo)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::fuchsia::fxblob::testing::{new_blob_fixture, BlobFixture},
        fidl_fuchsia_io::{self as fio},
        fuchsia_async as fasync,
        fuchsia_component::client::connect_to_protocol_at_dir_svc,
    };

    /// Read a blob using BlobReader API and return its contents as a boxed slice.
    async fn read_blob(
        blob_volume_outgoing_dir: &fio::DirectoryProxy,
        hash: Hash,
    ) -> Result<Box<[u8]>, Error> {
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
        let mut buf = Vec::with_capacity(vmo_size);
        buf.resize(vmo_size, 0);
        vmo.read(&mut buf[..], 0)?;
        Ok(buf.into_boxed_slice())
    }

    #[fasync::run(10, test)]
    async fn test_blob_reader() {
        let fixture = new_blob_fixture().await;
        let empty_blob_hash = fixture.write_blob(&[]).await;
        let short_data = b"This is some data";
        let short_blob_hash = fixture.write_blob(short_data).await;
        let long_data = &[0x65u8; 30000];
        let long_blob_hash = fixture.write_blob(long_data).await;

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
