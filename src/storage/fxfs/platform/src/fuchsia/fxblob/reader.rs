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
        crate::fuchsia::fxblob::testing::new_blob_fixture,
        delivery_blob::{CompressionMode, Type1Blob},
        fidl_fuchsia_io::{self as fio},
        fuchsia_async as fasync,
        fuchsia_component::client::connect_to_protocol_at_dir_svc,
        fuchsia_merkle::MerkleTreeBuilder,
    };

    /// Write a blob using BlobCreator API and return its hash.
    async fn write_blob(
        blob_volume_outgoing_dir: &fio::DirectoryProxy,
        data: &[u8],
    ) -> Result<Hash, Error> {
        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let hash = builder.finish().root();
        let compressed_data = Type1Blob::generate(&data, CompressionMode::Always);
        let blob_proxy = connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::BlobCreatorMarker>(
            &blob_volume_outgoing_dir,
        )
        .expect("failed to connect to the BlobCreator service");
        let blob_writer_client_end = blob_proxy
            .create(&hash.into(), false)
            .await
            .expect("transport error on create")
            .expect("failed to create blob");

        let writer = blob_writer_client_end.into_proxy().unwrap();
        let vmo = writer
            .get_vmo(compressed_data.len() as u64)
            .await
            .expect("transport error on get_vmo")
            .expect("failed to get vmo");

        let vmo_size = vmo.get_size().expect("failed to get vmo size") as usize;
        let mut offset = 0;
        for chunk in compressed_data.chunks(vmo_size / 2) {
            vmo.write(chunk, (offset % vmo_size) as u64).expect("failed to write to vmo");
            let _ = writer
                .bytes_ready(chunk.len() as u64)
                .await
                .expect("transport error on bytes_ready")
                .expect("failed to write data to vmo");
            offset += chunk.len();
        }
        Ok(hash)
    }

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
        let (blob_volume_outgoing_dir, server_end) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                .expect("Create dir proxy to succeed");
        fixture
            .volumes_directory()
            .serve_volume(fixture.volume(), server_end, true, true)
            .await
            .expect("failed to create_and_serve the blob volume");

        let empty_blob_hash =
            write_blob(&blob_volume_outgoing_dir, &[]).await.expect("write empty blob");
        let short_data = b"This is some data";
        let short_blob_hash =
            write_blob(&blob_volume_outgoing_dir, short_data).await.expect("write short blob");
        let long_data = &[0x65u8; 30000];
        let long_blob_hash =
            write_blob(&blob_volume_outgoing_dir, long_data).await.expect("write long blob");

        assert_eq!(
            &*read_blob(&blob_volume_outgoing_dir, empty_blob_hash).await.expect("read empty"),
            &[0u8; 0]
        );
        assert_eq!(
            &*read_blob(&blob_volume_outgoing_dir, short_blob_hash).await.expect("read short"),
            short_data
        );
        assert_eq!(
            &*read_blob(&blob_volume_outgoing_dir, long_blob_hash).await.expect("read long"),
            long_data
        );
        let missing_hash = Hash::from([0x77u8; 32]);
        assert!(read_blob(&blob_volume_outgoing_dir, missing_hash).await.is_err());

        fixture.close().await;
    }
}
