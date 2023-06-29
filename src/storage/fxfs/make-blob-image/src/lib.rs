// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Context, Error},
    fuchsia_async as fasync,
    fuchsia_merkle::{Hash, MerkleTreeBuilder, HASH_SIZE},
    futures::{try_join, SinkExt as _, StreamExt as _, TryStreamExt as _},
    fxfs::{
        errors::FxfsError,
        filesystem::{Filesystem, FxFilesystem, SyncOptions},
        object_handle::{GetProperties, WriteBytes},
        object_store::{
            directory::Directory, transaction::LockKey, volume::root_volume, DirectWriter,
            ObjectStore, BLOB_MERKLE_ATTRIBUTE_ID,
        },
        round::round_up,
        serialized_types::BlobMetadata,
    },
    rayon::{prelude::*, ThreadPoolBuilder},
    serde::{Deserialize, Serialize},
    std::{
        io::{BufRead, BufReader, BufWriter, Read},
        path::{Path, PathBuf},
        str::FromStr,
        sync::Arc,
    },
    storage_device::{file_backed_device::FileBackedDevice, DeviceHolder},
};

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
    manifest_path: &str,
    json_output_path: &str,
    target_size: Option<u64>,
) -> Result<(), Error> {
    let output_image = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output_image_path)?;
    let blobs = parse_manifest(manifest_path).context("Failed to parse manifest")?;

    let size = target_size.unwrap_or_default();

    const BLOCK_SIZE: u32 = 4096;
    if size > 0 && size < BLOCK_SIZE as u64 {
        return Err(anyhow!("Size {} is too small", size));
    }
    if size % BLOCK_SIZE as u64 > 0 {
        return Err(anyhow!("Invalid size {} is not block-aligned", size));
    }
    let block_count = if size != 0 {
        // Truncate the image to the target size now.
        output_image.set_len(size).context("Failed to resize image")?;
        size / BLOCK_SIZE as u64
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
    let filesystem =
        FxFilesystem::new_empty(device).await.context("Failed to format filesystem")?;
    let blobs_json = install_blobs(filesystem.clone(), "blob", blobs).await.map_err(|e| {
        if target_size.is_some() && FxfsError::NoSpace.matches(&e) {
            e.context(format!(
                "Configured image size {} is too small to fit the base system image.",
                size
            ))
        } else {
            e
        }
    })?;
    filesystem
        .sync(SyncOptions { flush_device: true, ..Default::default() })
        .await
        .context("Failed to flush")?;
    let actual_length = filesystem.allocator().maximum_offset();
    filesystem.close().await?;

    if size == 0 {
        let output_image =
            std::fs::OpenOptions::new().read(true).write(true).open(output_image_path)?;
        let actual_size = output_image.metadata()?.len();
        output_image.set_len(actual_size * 2).context("Failed to resize image")?;
    }

    if let Some(sparse_path) = sparse_output_image_path {
        create_sparse_image(sparse_path, output_image_path, actual_length, BLOCK_SIZE)
            .context("Failed to create sparse image")?;
    }

    let mut json_output = BufWriter::new(
        std::fs::File::create(json_output_path).context("Failed to create JSON output file")?,
    );
    serde_json::to_writer_pretty(&mut json_output, &blobs_json)
        .context("Failed to serialize to JSON output")?;

    Ok(())
}

fn parse_manifest(manifest_path: &str) -> Result<Vec<(Hash, PathBuf)>, Error> {
    let manifest_path = Path::new(manifest_path);
    let dot_dot = Path::new("..");
    let manifest_base = manifest_path.parent().unwrap_or(&dot_dot);

    let manifest = BufReader::new(std::fs::File::open(manifest_path)?);
    let mut blobs = vec![];
    for line in manifest.lines() {
        let line = line?;
        let (hash_str, path) = line.split_once('=').ok_or(anyhow!("Expected '='"))?;
        let hash = Hash::from_str(hash_str).context(format!("Invalid hash {}", hash_str))?;
        let path = Path::new(path);
        let path_buf =
            if path.is_relative() { manifest_base.join(path) } else { PathBuf::from(path) };
        blobs.push((hash, path_buf));
    }
    Ok(blobs)
}

fn create_sparse_image(
    sparse_output_image_path: &str,
    image_path: &str,
    actual_length: u64,
    block_size: u32,
) -> Result<(), Error> {
    let image = std::fs::OpenOptions::new()
        .read(true)
        .open(image_path)
        .context(format!("Failed to open {:?}", image_path))?;
    let full_length = image.metadata()?.len();
    let mut output = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(sparse_output_image_path)
        .context(format!("Failed to create {:?}", sparse_output_image_path))?;
    sparse::SparseImageBuilder::new()
        .set_block_size(block_size)
        .add_chunk(sparse::DataSource::Reader(Box::new(image), actual_length))
        .add_chunk(sparse::DataSource::Skip(full_length - actual_length))
        .build(&mut output)
}

struct BlobToInstall {
    hash: Hash,
    path: PathBuf,
    // Compressed blobs may be made up of several chunks. To reduce data copying and speed up image
    // generation, we write each chunk in order, rather than copying them into a contiguous buffer.
    contents: Vec<Vec<u8>>,
    uncompressed_size: usize,
    serialized_metadata: Vec<u8>,
}

async fn install_blobs(
    filesystem: Arc<dyn Filesystem>,
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
    filesystem: &Arc<dyn Filesystem>,
    directory: &Directory<ObjectStore>,
) -> Result<BlobsJsonOutputEntry, Error> {
    let merkle = blob.hash.to_string();

    let handle;
    let keys = [LockKey::object(directory.store().store_object_id(), directory.object_id())];
    let mut transaction = filesystem
        .clone()
        .new_transaction(&keys, Default::default())
        .await
        .context("new transaction")?;
    handle = directory
        .create_child_file(&mut transaction, merkle.as_str(), None)
        .await
        .context("create child file")?;
    transaction.commit().await.context("transaction commit")?;

    // TODO(fxbug.dev/122125): Should we inline the data payload too?
    {
        let mut writer = DirectWriter::new(&handle, Default::default());
        for bytes in blob.contents {
            writer.write_bytes(&bytes).await.context("write blob contents")?;
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
        .context(format!("Unable to open `{:?}'", &path))?
        .read_to_end(&mut contents)
        .context(format!("Unable to read contents of `{:?}'", &path))?;
    let hashes = {
        // TODO(fxbug.dev/122056): Refactor to share implementation with blob.rs.
        let mut builder = MerkleTreeBuilder::new();
        builder.write(&contents);
        let tree = builder.finish();
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

    let (contents, chunk_size, compressed_offsets) =
        maybe_compress(contents, fuchsia_merkle::BLOCK_SIZE, fs_block_size);

    // We only need to store metadata with the blob if it's large enough or was compressed.
    let serialized_metadata: Vec<u8> = if !hashes.is_empty() || !compressed_offsets.is_empty() {
        let metadata = BlobMetadata {
            hashes,
            chunk_size,
            compressed_offsets,
            uncompressed_size: uncompressed_size as u64,
        };
        bincode::serialize(&metadata).context("serialize blob metadata")?
    } else {
        vec![]
    };

    Ok(BlobToInstall { hash, path, contents, uncompressed_size, serialized_metadata })
}

// TODO(fxbug.dev/124377): Support the blob delivery format.
fn maybe_compress(
    buf: Vec<u8>,
    block_size: usize,
    filesystem_block_size: usize,
) -> (/*data*/ Vec<Vec<u8>>, /*chunk_size*/ u64, /*compressed_offsets*/ Vec<u64>) {
    if buf.len() <= filesystem_block_size {
        // No savings, return original data.
        return (vec![buf], 0, vec![]);
    }

    const MAX_FRAMES: usize = 1023;
    const TARGET_CHUNK_SIZE: usize = 32 * 1024;
    let chunk_size = if buf.len() > MAX_FRAMES * TARGET_CHUNK_SIZE {
        round_up(buf.len() / MAX_FRAMES, block_size).unwrap()
    } else {
        TARGET_CHUNK_SIZE
    };

    let mut chunks: Vec<Vec<u8>> = vec![];
    buf.par_chunks(chunk_size)
        .map(|chunk| {
            let mut compressor = zstd::bulk::Compressor::new(14).unwrap();
            compressor.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true)).unwrap();
            compressor.compress(chunk).unwrap()
        })
        .collect_into_vec(&mut chunks);

    let mut compressed_offsets = vec![0 as u64];
    if chunks.len() > 1 {
        compressed_offsets.reserve(chunks.len() - 1);
        for chunk in &chunks[..chunks.len() - 1] {
            compressed_offsets.push(*compressed_offsets.last().unwrap() + chunk.len() as u64);
        }
    }

    let total_size = *compressed_offsets.last().unwrap() as usize + chunks.last().unwrap().len();
    if round_up(total_size, filesystem_block_size).unwrap() >= buf.len() {
        // Compression expanded the file, return original data.
        return (vec![buf], 0, vec![]);
    }

    (chunks, chunk_size as u64, compressed_offsets)
}

#[cfg(test)]
mod tests {
    use {
        super::{install_blobs, BlobsJsonOutputEntry},
        assert_matches::assert_matches,
        fuchsia_async as fasync,
        fuchsia_merkle::MerkleTreeBuilder,
        fxfs::{
            filesystem::FxFilesystem,
            object_store::{directory::Directory, volume::root_volume},
        },
        std::{fs::File, io::Write as _, path::Path, str::from_utf8},
        storage_device::{fake_device::FakeDevice, DeviceHolder},
        tempfile::TempDir,
    };

    #[fasync::run(10, test)]
    async fn make_blob_image() {
        let device = DeviceHolder::new(FakeDevice::new(16384, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let blobs_in = {
            let write_data = |path, data: &str| {
                let mut file = File::create(&path).unwrap();
                write!(file, "{}", data).unwrap();
                let mut builder = MerkleTreeBuilder::new();
                builder.write(data.as_bytes());
                let tree = builder.finish();
                (tree.root(), path)
            };
            vec![
                write_data(dir.join("stuff1.txt"), "Goodbye, stranger!"),
                write_data(dir.join("stuff2.txt"), "It's been nice!"),
                write_data(dir.join("stuff3.txt"), from_utf8(&['a' as u8; 65_537]).unwrap()),
            ]
        };

        let mut blobs_out = install_blobs(filesystem.clone(), "blob", blobs_in)
            .await
            .expect("Failed to install blobs");
        assert_eq!(blobs_out.len(), 3);
        blobs_out.sort_by_key(|entry| entry.source_path.clone());

        assert_eq!(Path::new(blobs_out[0].source_path.as_str()), dir.join("stuff1.txt"));
        assert_matches!(
            &blobs_out[0],
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
        assert_eq!(Path::new(blobs_out[1].source_path.as_str()), dir.join("stuff2.txt"));
        assert_matches!(
            &blobs_out[1],
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
        assert_eq!(Path::new(blobs_out[2].source_path.as_str()), dir.join("stuff3.txt"));
        assert_matches!(
            &blobs_out[2],
            BlobsJsonOutputEntry {
                merkle,
                bytes: 65537,
                // XXX: This is technically sensitive to compression, but a string of 'a' should
                // always compress down to a single block.
                size: 8192,
                file_size: 65537,
                merkle_tree_size: 344,
                used_space_in_blobfs: 8192,
                ..
            } if merkle == "1194c76d2d3b61f29df97a85ede7b2fd2b293b452f53072356e3c5c939c8131d"
        );

        // Also verify the files exist in Fxfs.
        let root_volume = root_volume(filesystem.clone()).await.expect("Opening root volume");
        let vol = root_volume.volume("blob", None).await.expect("Opening volume");
        let directory =
            Directory::open(&vol, vol.root_directory_object_id()).await.expect("Opening root dir");
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
