// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Error};
use delivery_blob::compression::ChunkedArchive;
use fuchsia_async as fasync;
use fuchsia_merkle::{Hash, HASH_SIZE};
use futures::{try_join, SinkExt as _, StreamExt as _, TryStreamExt as _};
use fxfs::errors::FxfsError;
use fxfs::filesystem::{FxFilesystem, FxFilesystemBuilder};
use fxfs::object_handle::WriteBytes;
use fxfs::object_store::directory::Directory;
use fxfs::object_store::journal::RESERVED_SPACE;
use fxfs::object_store::transaction::{lock_keys, LockKey};
use fxfs::object_store::volume::root_volume;
use fxfs::object_store::{DirectWriter, HandleOptions, ObjectStore, BLOB_MERKLE_ATTRIBUTE_ID};
use fxfs::round::round_up;
use fxfs::serialized_types::BlobMetadata;
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use serde::{Deserialize, Serialize};
use std::io::{BufWriter, Read};
use std::path::PathBuf;
use std::sync::Arc;
use storage_device::file_backed_device::FileBackedDevice;
use storage_device::DeviceHolder;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct BlobsJsonOutputEntry {
    source_path: String,
    merkle: String,
    bytes: usize,
    size: u64,
    file_size: usize,
    compressed_file_size: u64,
    merkle_tree_size: usize,
    // For consistency with the legacy blobfs tooling, we still use the name `blobfs`.
    used_space_in_blobfs: u64,
}

type BlobsJsonOutput = Vec<BlobsJsonOutputEntry>;

/// Generates an Fxfs image containing a blob volume with the blobs specified in `manifest_path`.
/// Creates the block image at `output_image_path` and writes a blobs.json file to
/// `json_output_path`.
/// If `target_size` bytes is set, the raw image will be set to exactly this size (and an error is
/// returned if the contents exceed that size).  If unset (or 0), the image will be truncated to
/// twice the size of its contents, which is a heuristic that gives us roughly enough space for
/// normal usage of the image.
/// If `sparse_output_image_path` is set, an image will also be emitted in the Android sparse
/// format, which is suitable for flashing via fastboot.  The sparse image's logical size and
/// contents are identical to the raw image, but its actual size will likely be smaller.
pub async fn make_blob_image(
    output_image_path: &str,
    sparse_output_image_path: Option<&str>,
    blobs: Vec<(Hash, PathBuf)>,
    json_output_path: &str,
    target_size: Option<u64>,
) -> Result<(), Error> {
    let output_image = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output_image_path)?;

    let mut target_size = target_size.unwrap_or_default();

    const BLOCK_SIZE: u32 = 4096;
    if target_size > 0 && target_size < BLOCK_SIZE as u64 {
        return Err(anyhow!("Size {} is too small", target_size));
    }
    if target_size % BLOCK_SIZE as u64 > 0 {
        return Err(anyhow!("Invalid size {} is not block-aligned", target_size));
    }
    let block_count = if target_size != 0 {
        // Truncate the image to the target size now.
        output_image.set_len(target_size).context("Failed to resize image")?;
        target_size / BLOCK_SIZE as u64
    } else {
        // Arbitrarily use 4GiB for the initial block device size, but don't truncate the file yet,
        // so it becomes exactly as large as needed to contain the contents.  We'll truncate it down
        // to 2x contents later.
        // 4G just needs to be large enough to fit pretty much any image.
        const FOUR_GIGS: u64 = 4 * 1024 * 1024 * 1024;
        FOUR_GIGS / BLOCK_SIZE as u64
    };

    let device = DeviceHolder::new(FileBackedDevice::new_with_block_count(
        output_image,
        BLOCK_SIZE,
        block_count,
    ));
    let filesystem = FxFilesystemBuilder::new()
        .format(true)
        .trim_config(None)
        .image_builder_mode(true)
        .open(device)
        .await
        .context("Failed to format filesystem")?;
    let blobs_json = install_blobs(filesystem.clone(), "blob", blobs).await.map_err(|e| {
        if target_size != 0 && FxfsError::NoSpace.matches(&e) {
            e.context(format!(
                "Configured image size {} is too small to fit the base system image.",
                target_size
            ))
        } else {
            e
        }
    })?;
    let (_device, actual_size) = filesystem.finalize().await?;

    if target_size == 0 {
        // Apply a default heuristic of 2x the actual image size.  This is necessary to use the
        // Fxfs image, since if it's completely full it can't be modified.
        target_size = (actual_size + RESERVED_SPACE) * 2;
    }

    if let Some(sparse_path) = sparse_output_image_path {
        create_sparse_image(sparse_path, output_image_path, target_size, BLOCK_SIZE)
            .context("Failed to create sparse image")?;
    }

    if target_size != actual_size {
        debug_assert!(target_size > actual_size);
        let output_image =
            std::fs::OpenOptions::new().read(true).write(true).open(output_image_path)?;
        output_image.set_len(target_size).context("Failed to resize image")?;
    }

    let mut json_output = BufWriter::new(
        std::fs::File::create(json_output_path).context("Failed to create JSON output file")?,
    );
    serde_json::to_writer_pretty(&mut json_output, &blobs_json)
        .context("Failed to serialize to JSON output")?;

    Ok(())
}

fn create_sparse_image(
    sparse_output_image_path: &str,
    image_path: &str,
    target_length: u64,
    block_size: u32,
) -> Result<(), Error> {
    let image = std::fs::OpenOptions::new()
        .read(true)
        .open(image_path)
        .with_context(|| format!("Failed to open {:?}", image_path))?;
    let actual_length = image.metadata()?.len();
    let mut output = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(sparse_output_image_path)
        .with_context(|| format!("Failed to create {:?}", sparse_output_image_path))?;
    sparse::builder::SparseImageBuilder::new()
        .set_block_size(block_size)
        .add_chunk(sparse::builder::DataSource::Reader(Box::new(image)))
        .add_chunk(sparse::builder::DataSource::Skip(target_length - actual_length))
        .build(&mut output)
}

enum BlobData {
    Uncompressed(Vec<u8>),
    Compressed(ChunkedArchive),
}

impl BlobData {
    fn compressed_offsets(&self) -> Option<Vec<u64>> {
        if let BlobData::Compressed(archive) = self {
            let mut offsets = vec![0 as u64];
            let chunks = archive.chunks();
            if chunks.len() > 1 {
                offsets.reserve(chunks.len() - 1);
                for chunk in &chunks[..chunks.len() - 1] {
                    offsets.push(*offsets.last().unwrap() + chunk.compressed_data.len() as u64);
                }
            }
            Some(offsets)
        } else {
            None
        }
    }

    fn chunk_size(&self) -> Option<u64> {
        if let BlobData::Compressed(archive) = self {
            Some(archive.chunk_size() as u64)
        } else {
            None
        }
    }
}

struct BlobToInstall {
    hash: Hash,
    path: PathBuf,
    data: BlobData,
    uncompressed_size: usize,
    serialized_metadata: Vec<u8>,
}

async fn install_blobs(
    filesystem: Arc<FxFilesystem>,
    volume_name: &str,
    blobs: Vec<(Hash, PathBuf)>,
) -> Result<BlobsJsonOutput, Error> {
    let root_volume = root_volume(filesystem.clone()).await?;
    let vol = root_volume.new_volume(volume_name, None).await.context("Failed to create volume")?;
    let directory = Directory::open(&vol, vol.root_directory_object_id())
        .await
        .context("Unable to open root directory")?;
    let num_blobs = blobs.len();
    let fs_block_size = filesystem.block_size() as usize;
    // We don't need any backpressure as the channel guarantees at least one slot per sender.
    let (tx, rx) = futures::channel::mpsc::channel::<BlobToInstall>(0);
    // Generate each blob in parallel using a thread pool.
    let num_threads: usize = std::thread::available_parallelism().unwrap().into();
    let thread_pool = ThreadPoolBuilder::new().num_threads(num_threads).build().unwrap();
    let generate = fasync::unblock(move || {
        thread_pool.install(|| {
            blobs.par_iter().try_for_each(|(hash, path)| {
                let blob = generate_blob(hash.clone(), path.clone(), fs_block_size)?;
                futures::executor::block_on(tx.clone().send(blob))
                    .context("send blob to install task")
            })
        })?;
        Ok(())
    });
    // We can buffer up to this many blobs after processing.
    const MAX_INSTALL_CONCURRENCY: usize = 10;
    let install = rx
        .map(|blob| install_blob(blob, &filesystem, &directory))
        .buffer_unordered(MAX_INSTALL_CONCURRENCY)
        .try_collect::<BlobsJsonOutput>();
    let (installed_blobs, _) = try_join!(install, generate)?;
    assert_eq!(installed_blobs.len(), num_blobs);
    Ok(installed_blobs)
}

async fn install_blob(
    blob: BlobToInstall,
    filesystem: &Arc<FxFilesystem>,
    directory: &Directory<ObjectStore>,
) -> Result<BlobsJsonOutputEntry, Error> {
    let merkle = blob.hash.to_string();

    let handle;
    let keys =
        lock_keys![LockKey::object(directory.store().store_object_id(), directory.object_id())];
    let mut transaction = filesystem
        .clone()
        .new_transaction(keys, Default::default())
        .await
        .context("new transaction")?;
    handle = directory
        .create_child_file_with_options(
            &mut transaction,
            merkle.as_str(),
            // Checksums are redundant for blobs, which are already content-verified.
            HandleOptions { skip_checksums: true, ..Default::default() },
        )
        .await
        .context("create child file")?;
    transaction.commit().await.context("transaction commit")?;

    {
        let mut writer = DirectWriter::new(&handle, Default::default()).await;
        match blob.data {
            BlobData::Uncompressed(data) => {
                writer.write_bytes(&data).await.context("write blob contents")?;
            }
            BlobData::Compressed(archive) => {
                for chunk in archive.chunks() {
                    writer
                        .write_bytes(&chunk.compressed_data)
                        .await
                        .context("write blob contents")?;
                }
            }
        }
        writer.complete().await.context("flush blob contents")?;
    }

    if !blob.serialized_metadata.is_empty() {
        handle
            .write_attr(BLOB_MERKLE_ATTRIBUTE_ID, &blob.serialized_metadata)
            .await
            .context("write blob metadata")?;
    }
    let properties = handle.get_properties().await.context("get properties")?;
    Ok(BlobsJsonOutputEntry {
        source_path: blob.path.to_str().context("blob path to utf8")?.to_string(),
        merkle,
        bytes: blob.uncompressed_size,
        size: properties.allocated_size,
        file_size: blob.uncompressed_size,
        compressed_file_size: properties.data_attribute_size,
        merkle_tree_size: blob.serialized_metadata.len(),
        used_space_in_blobfs: properties.allocated_size,
    })
}

fn generate_blob(hash: Hash, path: PathBuf, fs_block_size: usize) -> Result<BlobToInstall, Error> {
    let mut contents = Vec::new();
    std::fs::File::open(&path)
        .with_context(|| format!("Unable to open `{:?}'", &path))?
        .read_to_end(&mut contents)
        .with_context(|| format!("Unable to read contents of `{:?}'", &path))?;
    let hashes = {
        // TODO(https://fxbug.dev/42073036): Refactor to share implementation with blob.rs.
        let tree = fuchsia_merkle::from_slice(&contents);
        assert_eq!(tree.root(), hash);
        let mut hashes: Vec<[u8; HASH_SIZE]> = Vec::new();
        let levels = tree.as_ref();
        if levels.len() > 1 {
            // We only need to store the leaf hashes.
            for hash in &levels[0] {
                hashes.push(hash.clone().into());
            }
        }
        hashes
    };

    let uncompressed_size = contents.len();

    let data = maybe_compress(contents, fuchsia_merkle::BLOCK_SIZE, fs_block_size);

    // We only need to store metadata with the blob if it's large enough or was compressed.
    let serialized_metadata: Vec<u8> =
        if !hashes.is_empty() || matches!(data, BlobData::Compressed(_)) {
            let metadata = BlobMetadata {
                hashes,
                chunk_size: data.chunk_size().unwrap_or_default(),
                compressed_offsets: data.compressed_offsets().unwrap_or_default(),
                uncompressed_size: uncompressed_size as u64,
            };
            bincode::serialize(&metadata).context("serialize blob metadata")?
        } else {
            vec![]
        };

    Ok(BlobToInstall { hash, path, data, uncompressed_size, serialized_metadata })
}

fn maybe_compress(buf: Vec<u8>, block_size: usize, filesystem_block_size: usize) -> BlobData {
    if buf.len() <= filesystem_block_size {
        return BlobData::Uncompressed(buf); // No savings, return original data.
    }
    let archive: ChunkedArchive =
        ChunkedArchive::new(&buf, block_size).expect("failed to compress data");
    if round_up(archive.compressed_data_size(), filesystem_block_size).unwrap() >= buf.len() {
        BlobData::Uncompressed(buf) // Compression expanded the file, return original data.
    } else {
        BlobData::Compressed(archive)
    }
}

#[cfg(test)]
mod tests {
    use super::{make_blob_image, BlobsJsonOutput, BlobsJsonOutputEntry};
    use assert_matches::assert_matches;
    use fuchsia_async as fasync;
    use fxfs::filesystem::FxFilesystem;
    use fxfs::object_store::directory::Directory;
    use fxfs::object_store::volume::root_volume;
    use sparse::reader::SparseReader;
    use std::fs::File;
    use std::io::{Seek as _, SeekFrom, Write as _};
    use std::path::Path;
    use std::str::from_utf8;
    use storage_device::file_backed_device::FileBackedDevice;
    use storage_device::DeviceHolder;
    use tempfile::TempDir;

    #[fasync::run(10, test)]
    async fn test_make_blob_image() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let blobs_in = {
            let write_data = |path, data: &str| {
                let mut file = File::create(&path).unwrap();
                write!(file, "{}", data).unwrap();
                let tree = fuchsia_merkle::from_slice(data.as_bytes());
                (tree.root(), path)
            };
            vec![
                write_data(dir.join("stuff1.txt"), "Goodbye, stranger!"),
                write_data(dir.join("stuff2.txt"), "It's been nice!"),
                write_data(dir.join("stuff3.txt"), from_utf8(&['a' as u8; 65_537]).unwrap()),
            ]
        };

        let dir = tmp.path();
        let output_path = dir.join("fxfs.blk");
        let sparse_path = dir.join("fxfs.sparse.blk");
        let blobs_json_path = dir.join("blobs.json");
        make_blob_image(
            output_path.as_os_str().to_str().unwrap(),
            Some(sparse_path.as_os_str().to_str().unwrap()),
            blobs_in,
            blobs_json_path.as_os_str().to_str().unwrap(),
            None,
        )
        .await
        .expect("make_blob_image failed");

        // Check that the blob manifest contains the entries we expect.
        let mut blobs_json = std::fs::OpenOptions::new()
            .read(true)
            .open(blobs_json_path)
            .expect("Failed to open blob manifest");
        let mut blobs: BlobsJsonOutput =
            serde_json::from_reader(&mut blobs_json).expect("Failed to serialize to JSON output");

        assert_eq!(blobs.len(), 3);
        blobs.sort_by_key(|entry| entry.source_path.clone());

        assert_eq!(Path::new(blobs[0].source_path.as_str()), dir.join("stuff1.txt"));
        assert_matches!(
            &blobs[0],
            BlobsJsonOutputEntry {
                merkle,
                bytes: 18,
                size: 4096,
                file_size: 18,
                merkle_tree_size: 0,
                used_space_in_blobfs: 4096,
                ..
            } if merkle == "9a24fe2fb8da617f39d303750bbe23f4e03a8b5f4d52bc90b2e5e9e44daddb3a"
        );
        assert_eq!(Path::new(blobs[1].source_path.as_str()), dir.join("stuff2.txt"));
        assert_matches!(
            &blobs[1],
            BlobsJsonOutputEntry {
                merkle,
                bytes: 15,
                size: 4096,
                file_size: 15,
                merkle_tree_size: 0,
                used_space_in_blobfs: 4096,
                ..
            } if merkle == "deebe5d5a0a42a51a293b511d0368e6f2b4da522ee0f05c6ae728c77d904f916"
        );
        assert_eq!(Path::new(blobs[2].source_path.as_str()), dir.join("stuff3.txt"));
        assert_matches!(
            &blobs[2],
            BlobsJsonOutputEntry {
                merkle,
                bytes: 65537,
                // This is technically sensitive to compression, but a string of 'a' should
                // always compress down to a single block.
                size: 8192,
                file_size: 65537,
                merkle_tree_size: 344,
                used_space_in_blobfs: 8192,
                ..
            } if merkle == "1194c76d2d3b61f29df97a85ede7b2fd2b293b452f53072356e3c5c939c8131d"
        );

        let unsparsed_image = {
            let sparse_image = std::fs::OpenOptions::new().read(true).open(sparse_path).unwrap();
            let mut reader =
                SparseReader::new(Box::new(sparse_image)).expect("Failed to parse sparse image");

            let unsparsed_image_path = dir.join("fxfs.unsparsed.blk");
            let mut unsparsed_image = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open(unsparsed_image_path)
                .unwrap();

            std::io::copy(&mut reader, &mut unsparsed_image).expect("Failed to unsparse");
            unsparsed_image.seek(SeekFrom::Start(0)).unwrap();
            unsparsed_image
        };

        let orig_image = std::fs::OpenOptions::new()
            .read(true)
            .open(output_path.clone())
            .expect("Failed to open image");

        assert_eq!(unsparsed_image.metadata().unwrap().len(), orig_image.metadata().unwrap().len());

        // Verify the images created are valid Fxfs images and contains the blobs we expect.
        for image in [orig_image, unsparsed_image] {
            let device = DeviceHolder::new(FileBackedDevice::new(image, 4096));
            let filesystem = FxFilesystem::open(device).await.unwrap();
            let root_volume = root_volume(filesystem.clone()).await.expect("Opening root volume");
            let vol = root_volume.volume("blob", None).await.expect("Opening volume");
            let directory = Directory::open(&vol, vol.root_directory_object_id())
                .await
                .expect("Opening root dir");
            let entries = {
                let layer_set = directory.store().tree().layer_set();
                let mut merger = layer_set.merger();
                let mut iter = directory.iter(&mut merger).await.expect("iter failed");
                let mut entries = vec![];
                while let Some((name, _, _)) = iter.get() {
                    entries.push(name.to_string());
                    iter.advance().await.expect("advance failed");
                }
                entries
            };
            assert_eq!(
                &entries[..],
                &[
                    "1194c76d2d3b61f29df97a85ede7b2fd2b293b452f53072356e3c5c939c8131d",
                    "9a24fe2fb8da617f39d303750bbe23f4e03a8b5f4d52bc90b2e5e9e44daddb3a",
                    "deebe5d5a0a42a51a293b511d0368e6f2b4da522ee0f05c6ae728c77d904f916",
                ]
            );
        }
    }
}
