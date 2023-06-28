// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(test)]
mod tests {
    use {
        blob_writer::BlobWriter,
        fidl::endpoints::{create_proxy, ServerEnd},
        fidl_fuchsia_fxfs::MountOptions,
        fidl_fuchsia_io::{self as fio, MAX_TRANSFER_SIZE},
        fs_management::{filesystem::Filesystem, Fxfs},
        fuchsia_component::client::connect_to_protocol_at_dir_svc,
        fuchsia_merkle::MerkleTreeBuilder,
        ramdevice_client::RamdiskClient,
        rand::{thread_rng, Rng},
    };
    const DEVICE_SIZE: u64 = 128 * 1024 * 1024;
    const BLOCK_SIZE: u64 = 4 * 1024;
    const BLOCK_COUNT: u64 = DEVICE_SIZE / BLOCK_SIZE;

    #[fuchsia::test]
    async fn small_write_no_wrap_test() {
        let mut ramdisk = RamdiskClient::create(BLOCK_SIZE, BLOCK_COUNT).await.unwrap();
        let mut fs = Filesystem::new(ramdisk.take_controller().unwrap(), Fxfs::default());
        fs.format().await.expect("Failed to format the filesystem");
        let mut serving_filesystem =
            fs.serve_multi_volume().await.expect("Failed to start the filesystem");
        let vol = serving_filesystem
            .create_volume("blob", MountOptions { crypt: None, as_blob: true })
            .await
            .expect("Failed to create volume");
        let blob_proxy = connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::BlobCreatorMarker>(
            vol.exposed_dir(),
        )
        .expect("failed to connect to the Blob service");

        let mut data = vec![1; 196608];
        thread_rng().fill(&mut data[..]);

        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let hash = builder.finish().root();

        let writer_client_end = blob_proxy
            .create(&hash.into(), false)
            .await
            .expect("transport error on BlobCreator.Create")
            .expect("failed to create blob");
        let writer = writer_client_end.into_proxy().unwrap();
        let mut blob_writer = BlobWriter::create(writer, data.len() as u64)
            .await
            .expect("failed to create BlobWriter");

        let list_of_writes =
            vec![(0..8192), (8192..24576), (24576..32768), (32768..98304), (98304..196608)];

        for write in list_of_writes {
            blob_writer.write(&data[write]).await.unwrap();
        }

        let (blob, server_end) = create_proxy::<fio::FileMarker>().expect("create_proxy failed");
        vol.root()
            .open(
                fio::OpenFlags::RIGHT_READABLE,
                fio::ModeType::empty(),
                &hash.to_string(),
                ServerEnd::new(server_end.into_channel()),
            )
            .expect("failed to open blob");
        let _: Vec<_> = blob.query().await.expect("failed to query blob");

        let mut read_data = Vec::new();
        loop {
            let chunk =
                blob.read(MAX_TRANSFER_SIZE).await.expect("FIDL call failed").expect("read failed");
            let done = chunk.len() < MAX_TRANSFER_SIZE as usize;
            read_data.extend(chunk);
            if done {
                break;
            }
        }
        assert_eq!(data, read_data)
    }

    #[fuchsia::test]
    async fn large_write_wraps_multiple_times_test() {
        let mut ramdisk = RamdiskClient::create(BLOCK_SIZE, BLOCK_COUNT).await.unwrap();
        let mut fs = Filesystem::new(ramdisk.take_controller().unwrap(), Fxfs::default());
        fs.format().await.expect("Failed to format the filesystem");
        let mut serving_filesystem =
            fs.serve_multi_volume().await.expect("Failed to start the filesystem");
        let vol = serving_filesystem
            .create_volume("blob", MountOptions { crypt: None, as_blob: true })
            .await
            .expect("Failed to create volume");
        let blob_proxy = connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::BlobCreatorMarker>(
            vol.exposed_dir(),
        )
        .expect("failed to connect to the Blob service");

        let mut data = vec![1; 499712];
        thread_rng().fill(&mut data[..]);

        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let hash = builder.finish().root();

        let writer_client_end = blob_proxy
            .create(&hash.into(), false)
            .await
            .expect("transport error on BlobCreator.Create")
            .expect("failed to create blob");
        let writer = writer_client_end.into_proxy().unwrap();

        let mut blob_writer = BlobWriter::create(writer, data.len() as u64)
            .await
            .expect("failed to create BlobWriter");

        let list_of_writes = vec![
            (0..8192),
            (8192..24576),
            (24576..32768),
            (32768..98304),
            (98304..196608),
            (196608..204800),
            (204800..237568),
            (237568..303104),
            (303104..401408),
            (401408..499712),
        ];

        for write in list_of_writes {
            blob_writer.write(&data[write]).await.unwrap();
        }

        let (blob, server_end) = create_proxy::<fio::FileMarker>().expect("create_proxy failed");
        vol.root()
            .open(
                fio::OpenFlags::RIGHT_READABLE,
                fio::ModeType::empty(),
                &hash.to_string(),
                ServerEnd::new(server_end.into_channel()),
            )
            .expect("failed to open blob");
        let _: Vec<_> = blob.query().await.expect("failed to query blob");

        let mut read_data = Vec::new();
        loop {
            let chunk =
                blob.read(MAX_TRANSFER_SIZE).await.expect("FIDL call failed").expect("read failed");
            let done = chunk.len() < MAX_TRANSFER_SIZE as usize;
            read_data.extend(chunk);
            if done {
                break;
            }
        }
        assert_eq!(data, read_data)
    }
}
