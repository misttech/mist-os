// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Context, Error},
    fuchsia_merkle::{Hash, MerkleTreeBuilder, HASH_SIZE},
    fxfs::{
        filesystem::{Filesystem, FxFilesystem},
        object_handle::{GetProperties, WriteBytes},
        object_store::{
            directory::Directory, transaction::LockKey, volume::root_volume, DirectWriter,
            BLOB_MERKLE_ATTRIBUTE_ID,
        },
        round::{round_down, round_up},
        serialized_types::BlobMetadata,
    },
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
pub async fn make_blob_image(
    output_image_path: &str,
    manifest_path: &str,
    json_output_path: &str,
) -> Result<(), Error> {
    let output_image = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output_image_path)?;
    let blobs = parse_manifest(manifest_path).context("Failed to parse manifest")?;

    // Arbitrarily set the maximum image size to 64GiB.  The actual image size will be determined
    // by the greatest offset Fxfs writes to (which, when using a sequential allocator, results in
    // an optimally sized image).
    const BLOCK_SIZE: u32 = 4096;
    const BLOCK_COUNT: u64 = 1024 * 1024 * 16;
    let device = DeviceHolder::new(FileBackedDevice::new_with_block_count(
        output_image,
        BLOCK_SIZE,
        BLOCK_COUNT,
    ));
    let filesystem = FxFilesystem::new_empty(device).await.context("Faile to format filesystem")?;
    let blobs_json = install_blobs(filesystem.clone(), "blob", &blobs[..]).await?;
    filesystem.close().await?;

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

async fn install_blobs(
    filesystem: Arc<dyn Filesystem>,
    volume_name: &str,
    blobs: &[(Hash, PathBuf)],
) -> Result<BlobsJsonOutput, Error> {
    let root_volume = root_volume(filesystem.clone()).await?;
    let vol = root_volume.new_volume(volume_name, None).await.context("Failed to create volume")?;
    let directory = Directory::open(&vol, vol.root_directory_object_id())
        .await
        .context("Unable to open root directory")?;
    let mut blobs_json = BlobsJsonOutput::new();
    for (hash, path) in blobs {
        let mut contents = Vec::new();
        std::fs::File::open(path)
            .context(format!("Unable to open `{:?}'", path))?
            .read_to_end(&mut contents)?;
        let uncompressed_size = contents.len() as u64;

        let mut builder = MerkleTreeBuilder::new();
        builder.write(&contents);
        let tree = builder.finish();
        assert_eq!(&tree.root(), hash);

        let offsets_and_chunk_size = if let Some((compressed, chunk_size, offsets)) =
            maybe_compress(&contents, fuchsia_merkle::BLOCK_SIZE, filesystem.block_size() as usize)
        {
            contents = compressed;
            Some((offsets, chunk_size))
        } else {
            None
        };

        let handle;
        let keys = [LockKey::object(vol.store_object_id(), directory.object_id())];
        let mut transaction = filesystem.clone().new_transaction(&keys, Default::default()).await?;
        handle = directory.create_child_file(&mut transaction, &format!("{}", hash)).await?;
        transaction.commit().await?;

        // TODO(fxbug.dev/122125): Should we inline the data payload too?
        {
            let mut writer = DirectWriter::new(&handle, Default::default());
            writer.write_bytes(&contents).await?;
            writer.complete().await?;
        }

        let levels = tree.as_ref();
        // TODO(fxbug.dev/122056: Refactor to share implementation with blob.rs.
        let metadata_len = if levels.len() > 1 || offsets_and_chunk_size.is_some() {
            let mut hashes: Vec<[u8; HASH_SIZE]> = Vec::new();
            if levels.len() > 1 {
                // We only need to store the leaf hashes.
                for hash in &levels[0] {
                    hashes.push(hash.clone().into());
                }
            }
            let (compressed_offsets, chunk_size) = offsets_and_chunk_size.unwrap_or((vec![], 0));
            let mut serialized = Vec::new();
            bincode::serialize_into(
                &mut serialized,
                &BlobMetadata { hashes, chunk_size, compressed_offsets, uncompressed_size },
            )
            .unwrap();
            let len = serialized.len();
            handle.write_attr(BLOB_MERKLE_ATTRIBUTE_ID, &serialized).await?;
            len
        } else {
            0
        };

        let properties = handle.get_properties().await?;
        blobs_json.push(BlobsJsonOutputEntry {
            source_path: path.to_str().unwrap().to_string(),
            merkle: hash.to_string(),
            bytes: uncompressed_size as usize,
            size: properties.allocated_size,
            file_size: uncompressed_size as usize,
            compressed_file_size: properties.data_attribute_size,
            merkle_tree_size: metadata_len,
            used_space_in_blobfs: properties.allocated_size,
        })
    }

    Ok(blobs_json)
}

// TODO(fxbug.dev/122125): Support the blob delivery format.
fn maybe_compress(
    buf: &[u8],
    block_size: usize,
    filesystem_block_size: usize,
) -> Option<(Vec<u8>, u64, Vec<u64>)> {
    if buf.len() <= filesystem_block_size {
        // No savings.
        return None;
    }

    const MAX_FRAMES: usize = 1023;
    const TARGET_CHUNK_SIZE: usize = 32 * 1024;
    let chunk_size = if buf.len() > MAX_FRAMES * TARGET_CHUNK_SIZE {
        round_up(buf.len() / MAX_FRAMES, block_size).unwrap()
    } else {
        TARGET_CHUNK_SIZE
    };

    // Limit the size of the output buffer so that we fail if the data won't
    // compress.
    let mut output = Vec::with_capacity(round_down(buf.len() - 1, block_size));
    let mut compressed_offsets = Vec::new();

    struct Appender<'a>(&'a mut Vec<u8>);
    unsafe impl zstd::stream::raw::WriteBuf for Appender<'_> {
        fn as_slice(&self) -> &[u8] {
            &self.0[self.0.len()..]
        }
        fn capacity(&self) -> usize {
            self.0.capacity() - self.0.len()
        }
        fn as_mut_ptr(&mut self) -> *mut u8 {
            let len = self.0.len();
            unsafe { self.0.get_unchecked_mut(len) }
        }
        unsafe fn filled_until(&mut self, n: usize) {
            self.0.set_len(self.0.len() + n);
        }
    }

    let mut compressor = zstd::bulk::Compressor::new(14).ok()?;
    compressor.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true)).ok()?;
    for chunk in buf.chunks(chunk_size) {
        compressed_offsets.push(output.len() as u64);
        // If this fails it will be because the buffer is too small.
        if let Err(_) = compressor.compress_to_buffer(chunk, &mut Appender(&mut output)) {
            return None;
        }
    }
    if round_up(output.len(), filesystem_block_size).unwrap() >= buf.len() {
        // Compression expanded the file.
        return None;
    }
    Some((output, chunk_size as u64, compressed_offsets))
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

        let blobs_out = install_blobs(filesystem.clone(), "blob", &blobs_in[..])
            .await
            .expect("Failed to install blobs");

        assert_eq!(blobs_out.len(), 3);
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
