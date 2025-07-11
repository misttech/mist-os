// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fidl/fuchsia.fs.startup/cpp/wire.h>
#include <fidl/fuchsia.fxfs/cpp/markers.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/component/incoming/cpp/directory.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fzl/owned-vmo-mapper.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <algorithm>
#include <cerrno>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <limits>
#include <memory>
#include <set>
#include <span>
#include <string>
#include <utility>
#include <vector>

#include <fbl/array.h>
#include <gtest/gtest.h>
#include <zstd/zstd.h>

#include "src/lib/files/file.h"
#include "src/storage/blobfs/test/blob_utils.h"
#include "src/storage/fs_test/fs_test.h"
#include "src/storage/lib/fs_management/cpp/mount.h"

namespace fs_test {
namespace {
constexpr size_t kPageSize = 8192;
constexpr size_t kPagesPerBlock = 32;
constexpr size_t kSpareBytes = 16;

constexpr size_t NandSize(uint32_t block_count) {
  return static_cast<size_t>(block_count) * kPagesPerBlock * (kPageSize + kSpareBytes);
}

constexpr size_t WearSize(uint32_t block_count) {
  return static_cast<size_t>(block_count) * sizeof(uint32_t);
}

struct SystemConfig {
  size_t fvm_slice_size;

  uint32_t block_count;

  size_t blobfs_partition_size;
  size_t minfs_partition_size;

  // Used in `InitMinfs()` to create cold data inside Minfs that will not be
  // touched.
  size_t minfs_cold_data_size;

  // Used in `InitMinfs()` to create data files inside Minfs that will be randomly replaced during
  // `SimulateMinfs()`.
  size_t minfs_cycle_data_size;
};

// Holds all the resources that keep all the parts of the system mounted. Dropping this triggers
// unbinding everything shutting down the associated drivers.
struct MountedSystem {
  RamDevice ramnand;

  fidl::ClientEnd<fuchsia_io::Directory> blobfs_export_root;
  fs_management::NamespaceBinding blobfs_binding;
  blobfs::BlobCreatorWrapper blob_creator;

  fidl::ClientEnd<fuchsia_io::Directory> minfs_export_root;
  fs_management::NamespaceBinding minfs_binding;
};

struct Snapshot {
  zx::vmo image;
  zx::vmo wear_info;
};

class WearSimulator {
 public:
  WearSimulator() = delete;
  WearSimulator(const WearSimulator&) = delete;
  WearSimulator& operator=(const WearSimulator&) = delete;

  // The object needs to call `Init()` before it is usable. This anti-RAII pattern is here so that
  // all the heavy lifting can be done in void methods where we can simply ASSERT.
  explicit WearSimulator(SystemConfig config) : config_(config) {}

  // Mounts the partitions and sets everything up. Must be called exactly once before anything else.
  void Init();

  // Bring up the system by reading the image and wear_info into their vmos, and then rebooting.
  void InitFromImage(std::string image_path, std::string wear_info_path);

  // Simulates a number of operations or "cycles" in minfs.
  void SimulateMinfs(int cycles);

  // Fills Blobfs with randomly sized blobs to target the required space.
  void FillBlobfs(size_t space);

  // Attempts to reduce blobfs by the requested size. Due to varying blob sizes it may not be able
  // to do so exactly. The number of bytes it was unable to reduce by will be left in the space
  // argument.
  void ReduceBlobfsBy(size_t* space);

  // Tears down the current system in the given simulator and remounts the ftl. Useful to log ftl
  // wear info.
  zx::result<RamDevice> RemountFtl();

  // Tears down the current system and remounts everything.
  void Reboot();

  // Return a snapshot of the nand image and the nand wear info. The two snapshots are not perfectly
  // atomic, and may be slightly our of sync from each other.
  zx::result<Snapshot> Snapshot();

  // Write out the images to the custom_artifacts directory.
  void ExportImage();

 private:
  zx::vmo vmo_;
  zx::vmo wear_vmo_;
  SystemConfig config_;
  std::unique_ptr<MountedSystem> mount_;
  std::vector<size_t> cycle_files_;
};
void InitMinfs(const char* root_path, const SystemConfig& config, std::vector<size_t>* file_sizes) {
  constexpr size_t kMaxWritePages = 64ul;
  constexpr size_t kMaxWriteSize = kMaxWritePages * kPageSize;
  char path_buf[255];
  auto write_buf = std::make_unique<char[]>(kMaxWriteSize);
  memset(write_buf.get(), 0xAA, kMaxWriteSize);

  // Create"cold" data"
  sprintf(path_buf, "%s/cold/", root_path);
  ASSERT_EQ(mkdir(path_buf, 0755), 0) << std::string(path_buf) << ": " << errno;
  for (size_t space = 0; space < config.minfs_cold_data_size;) {
    sprintf(path_buf, "%s/cold/%ld", root_path, space);
    int f = open(path_buf, O_RDWR | O_CREAT | O_APPEND, 0644);
    ASSERT_GE(f, 0);
    size_t write_size;
    if (kMaxWritePages <= 1) {
      write_size = kPageSize;
    } else {
      write_size = ((rand() % (kMaxWritePages - 1)) + 1) * kPageSize;
    }
    space += write_size;
    while (write_size > 0) {
      ssize_t written = write(f, write_buf.get(), write_size);
      ASSERT_GT(written, 0l) << "fd " << f << " with size " << write_size << ": " << errno;
      write_size -= written;
    }
    ASSERT_EQ(close(f), 0) << errno;
  }

  // "Cycling" data. Files that are periodically overwritten, usually doing some kind of
  // read-modify-write to a new file, then mv'ing the new file over the old one.
  sprintf(path_buf, "%s/cycle/", root_path);
  ASSERT_EQ(mkdir(path_buf, 0755), 0);
  int i = 0;
  for (size_t space = 0; space < config.minfs_cycle_data_size;) {
    sprintf(path_buf, "%s/cycle/%d", root_path, i++);
    int f = open(path_buf, O_WRONLY | O_CREAT);
    ASSERT_GE(f, 0);
    size_t write_size;
    if (kMaxWritePages == 1) {
      write_size = kPageSize;
    } else {
      write_size = (rand() % (kMaxWritePages - 1) + 1) * kPageSize;
    }
    space += write_size;
    file_sizes->push_back(write_size);
    while (write_size > 0) {
      ssize_t written = write(f, write_buf.get(), write_size);
      ASSERT_GT(written, 0l) << "fd " << f << ": " << errno;
      write_size -= written;
    }
    ASSERT_EQ(close(f), 0) << errno;
  }

  // A folder of growing data, through some mixture of appending data and adding new files. This is
  // like cache, and when the fs gets over 95% full, we'll clear it.
  sprintf(path_buf, "%s/cache/", root_path);
  ASSERT_EQ(mkdir(path_buf, 0755), 0);
}

void WearSimulator::Init() {
  ASSERT_FALSE(mount_) << "Wear simulator already initialized";

  fzl::OwnedVmoMapper mapper;
  ASSERT_EQ(mapper.CreateAndMap(NandSize(config_.block_count), "wear-test-vmo"), ZX_OK);
  memset(mapper.start(), 0xff, NandSize(config_.block_count));
  vmo_ = mapper.Release();
  ASSERT_TRUE(vmo_.is_valid());

  ASSERT_EQ(zx::vmo::create(WearSize(config_.block_count), 0, &wear_vmo_), ZX_OK);

  auto res = CreateRamDevice({
      .use_ram_nand = true,
      .vmo = vmo_.borrow(),
      .use_fvm = true,
      .nand_wear_vmo = wear_vmo_.borrow(),
      .device_block_size = kPageSize,
      .device_block_count = 0,
      .fvm_slice_size = config_.fvm_slice_size,
  });
  ASSERT_TRUE(res.is_ok()) << "Failed to setup ram device: " << res.error_value();
  RamDevice ramnand = std::move(res.value());

  fidl::Arena arena;
  fs_management::MountedVolume* blobfs;
  fs_management::NamespaceBinding blobfs_bind;
  fidl::WireSyncClient<fuchsia_fxfs::BlobCreator> blob_creator;
  {
    auto res = ramnand.fvm_partition()->fvm().fs().CreateVolume(
        "blobfs",
        fuchsia_fs_startup::wire::CreateOptions::Builder(arena)
            .type_guid({1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1})
            .guid({1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1})
            .initial_size(config_.blobfs_partition_size)
            .Build(),
        fuchsia_fs_startup::wire::MountOptions::Builder(arena)
            .as_blob(true)
            .uri("#meta/blobfs.cm")
            .Build());
    ASSERT_TRUE(res.is_ok()) << "Failed to create blobfs: " << res.error_value();
    blobfs = res.value();

    auto svc = component::OpenDirectoryAt(blobfs->ExportRoot(), "svc");
    ASSERT_TRUE(svc.is_ok());
    auto creator = component::ConnectAt<fuchsia_fxfs::BlobCreator>(*svc);
    ASSERT_TRUE(creator.is_ok());
    blob_creator = fidl::WireSyncClient<fuchsia_fxfs::BlobCreator>(std::move(*creator));
    auto binding = fs_management::NamespaceBinding::Create("/blob/", blobfs->DataRoot().value());
    ASSERT_TRUE(binding.is_ok()) << binding.status_string();
    blobfs_bind = std::move(binding.value());
  }

  fs_management::MountedVolume* minfs;
  fs_management::NamespaceBinding minfs_bind;
  {
    auto res = ramnand.fvm_partition()->fvm().fs().CreateVolume(
        "minfs",
        fuchsia_fs_startup::wire::CreateOptions::Builder(arena)
            .type_guid({2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2})
            .guid({2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2})
            .initial_size(config_.minfs_partition_size)
            .Build(),
        fuchsia_fs_startup::wire::MountOptions::Builder(arena).uri("#meta/minfs.cm").Build());
    ASSERT_TRUE(res.is_ok()) << "Failed to create minfs: " << res.error_value();
    minfs = res.value();

    auto binding = fs_management::NamespaceBinding::Create("/minfs/", minfs->DataRoot().value());
    ASSERT_TRUE(binding.is_ok()) << binding.status_string();
    minfs_bind = std::move(binding.value());
  }

  InitMinfs(minfs_bind.path().c_str(), config_, &cycle_files_);

  mount_ = std::make_unique<MountedSystem>(MountedSystem{
      .ramnand = std::move(ramnand),
      .blobfs_export_root = blobfs->Release(),
      .blobfs_binding = std::move(blobfs_bind),
      .blob_creator = blobfs::BlobCreatorWrapper(std::move(blob_creator)),
      .minfs_export_root = minfs->Release(),
      .minfs_binding = std::move(minfs_bind),
  });
}

void WearSimulator::InitFromImage(std::string image_path, std::string wear_info_path) {
  {
    std::vector<uint8_t> compressed;
    ASSERT_TRUE(files::ReadFileToVector(image_path, &compressed));
    ASSERT_EQ(zx::vmo::create(NandSize(config_.block_count), 0, &vmo_), ZX_OK);
    fzl::VmoMapper image_mapper;
    ASSERT_EQ(image_mapper.Map(vmo_, 0, NandSize(config_.block_count)), ZX_OK);

    size_t decompressed = ZSTD_decompress(image_mapper.start(), NandSize(config_.block_count),
                                          compressed.data(), compressed.size());
    ASSERT_FALSE(ZSTD_isError(decompressed));
    ASSERT_EQ(decompressed, NandSize(config_.block_count)) << "Image ws not expected size";
  }

  {
    std::vector<uint8_t> wear_data;
    ASSERT_TRUE(files::ReadFileToVector(wear_info_path, &wear_data));
    ASSERT_EQ(wear_data.size(), WearSize(config_.block_count));
    ASSERT_EQ(zx::vmo::create(WearSize(config_.block_count), 0, &wear_vmo_), ZX_OK);
    ASSERT_EQ(wear_vmo_.write(wear_data.data(), 0, wear_data.size()), ZX_OK);
  }
  Reboot();
}

void WearSimulator::SimulateMinfs(int cycles) {
  constexpr size_t kMaxWritePages = 64ul;
  constexpr size_t kMaxWriteSize = kMaxWritePages * kPageSize;
  constexpr size_t kMaxCacheGrowth = 8 * kPageSize;
  constexpr size_t kNumCacheFiles = 128;

  ASSERT_TRUE(mount_) << "Wear simulator not initialized";
  const char* root_path = mount_->minfs_binding.path().c_str();

  char path_buf[255];
  char temp_path_buf[255];
  auto write_buf = std::make_unique<char[]>(kMaxWriteSize);

  sprintf(temp_path_buf, "%s/cycle/tmp", root_path);

  memset(write_buf.get(), 0xAB, kMaxWriteSize);
  for (int i = 0; i < cycles; i++) {
    switch (rand() % 2) {
      case 0: {
        // Cycle a file.
        int index = rand() % static_cast<int>(cycle_files_.size());
        sprintf(path_buf, "%s/cycle/%d", root_path, index);
        size_t size = cycle_files_[index];

        int f = open(temp_path_buf, O_WRONLY | O_CREAT);
        ASSERT_GE(f, 0) << "Failed to open tmp file: " << errno;
        ASSERT_EQ(write(f, write_buf.get(), size), static_cast<ssize_t>(size));
        ASSERT_EQ(close(f), 0);

        std::filesystem::rename(temp_path_buf, path_buf);
      } break;
      case 1: {
        // Add to a cache file.
        sprintf(path_buf, "%s/cache/%lu", root_path, rand() % kNumCacheFiles);
        int f = open(path_buf, O_WRONLY | O_CREAT | O_APPEND);
        ASSERT_GE(f, 0);
        size_t size = (rand() % (kMaxCacheGrowth - 1) + 1);
        ASSERT_EQ(write(f, write_buf.get(), size), static_cast<ssize_t>(size));
        ASSERT_EQ(close(f), 0);
      }

      break;
      default:
        ASSERT_TRUE(false) << "Didn't cover all cases!";
    }

    // Check if space is full and we need to purge cache.
    std::filesystem::space_info space = std::filesystem::space(std::filesystem::path(root_path));
    if (space.available * 20 < space.capacity) {
      // Less than 5% left. Wipe the cache.
      sprintf(path_buf, "%s/cache", root_path);
      for (const auto& entry : std::filesystem::directory_iterator(path_buf)) {
        ASSERT_EQ(unlink(entry.path().c_str()), 0) << path_buf << ": " << errno;
      }
    }
  }
}

void WearSimulator::FillBlobfs(size_t space) {
  constexpr size_t kMaxBlobSize = 96ul * 1024 * 1024;
  ASSERT_TRUE(mount_) << "Wear simulator not initialized";

  for (; space > 0;) {
    size_t max_pages = std::min(space, kMaxBlobSize) / kPageSize;
    size_t size;
    if (max_pages <= 1) {
      size = kPageSize;
    } else {
      size = ((rand() % (max_pages - 1)) + 1) * kPageSize;
    }
    auto blob = fbl::MakeArray<uint8_t>(size);
    memset(blob.get(), 0x55, blob.size());
    // Random 64 bit value at the start, avoid duplicate blobs.
    *reinterpret_cast<uint32_t*>(blob.get()) = rand();
    *reinterpret_cast<uint32_t*>(&blob[4]) = rand();

    // Don't compress the delivery blob, this way the blob will fill asmich space as needed for the
    // test, but can be well compressed for image import/export.
    auto delivery_blob =
        blobfs::TestDeliveryBlob::CreateUncompressed(std::span<uint8_t>(blob.begin(), blob.end()));
    ASSERT_TRUE(mount_->blob_creator.CreateAndWriteBlob(delivery_blob).is_ok());
    space -= size;
  }
}

void WearSimulator::ReduceBlobfsBy(size_t* space) {
  std::set<std::pair<size_t, std::string>> entries;
  for (const auto& entry :
       std::filesystem::directory_iterator(mount_->blobfs_binding.path().c_str())) {
    size_t size = std::filesystem::file_size(entry.path().c_str());
    entries.insert(std::make_pair(size, entry.path().string()));
  }

  // Remove files starting from the biggest, skipping over files that would remove
  // too much.
  for (auto it = entries.crbegin(); it != entries.crend() && *space > 0; it++) {
    if (it->first < *space) {
      ASSERT_EQ(unlink(it->second.c_str()), 0) << it->second << ": " << errno;
      *space -= it->first;
    }
  }
}

zx::result<RamDevice> WearSimulator::RemountFtl() {
  mount_.reset();

  auto snapshot = Snapshot();
  if (snapshot.is_error()) {
    return snapshot.take_error();
  }

  RamDevice ramnand = CreateRamDevice({
                                          .use_ram_nand = true,
                                          .vmo = snapshot->image.borrow(),
                                          .use_existing_fvm = true,
                                          .nand_wear_vmo = snapshot->wear_info.borrow(),
                                          .device_block_size = kPageSize,
                                          .device_block_count = 0,
                                          .fvm_slice_size = config_.fvm_slice_size,
                                      })
                          .value();

  {
    fzl::VmoMapper mapper;
    if (zx_status_t s = mapper.Map(snapshot->wear_info); s != ZX_OK) {
      return zx::error(s);
    }

    uint32_t* ptr = reinterpret_cast<uint32_t*>(mapper.start());
    uint32_t max = 0;
    uint32_t min = std::numeric_limits<uint32_t>::max();
    for (uint32_t i = 0; i < config_.block_count; ++i) {
      max = std::max(max, ptr[i]);
      min = std::min(min, ptr[i]);
    }
    printf("Max wear: %u, Min wear: %u\n", max, min);
  }
  vmo_ = std::move(snapshot->image);
  wear_vmo_ = std::move(snapshot->wear_info);
  return zx::ok(std::move(ramnand));
}

void WearSimulator::Reboot() {
  ASSERT_TRUE(vmo_.is_valid()) << "No image vmo to snapshot";
  ASSERT_TRUE(wear_vmo_.is_valid()) << "No wear info to snapshot";
  RamDevice ramnand = RemountFtl().value();

  fidl::Arena arena;
  fs_management::MountedVolume* blobfs;
  fs_management::NamespaceBinding blobfs_bind;
  fidl::WireSyncClient<fuchsia_fxfs::BlobCreator> blob_creator;
  {
    auto res = ramnand.fvm_partition()->fvm().fs().OpenVolume(
        "blobfs", fuchsia_fs_startup::wire::MountOptions::Builder(arena)
                      .as_blob(true)
                      .uri("#meta/blobfs.cm")
                      .Build());
    ASSERT_TRUE(res.is_ok()) << "Failed to create blobfs: " << res.error_value();
    blobfs = res.value();

    auto svc = component::OpenDirectoryAt(blobfs->ExportRoot(), "svc");
    ASSERT_TRUE(svc.is_ok());
    auto creator = component::ConnectAt<fuchsia_fxfs::BlobCreator>(*svc);
    ASSERT_TRUE(creator.is_ok());
    blob_creator = fidl::WireSyncClient<fuchsia_fxfs::BlobCreator>(std::move(*creator));
    auto binding = fs_management::NamespaceBinding::Create("/blob/", blobfs->DataRoot().value());
    ASSERT_TRUE(binding.is_ok()) << binding.status_string();
    blobfs_bind = std::move(binding.value());
  }

  fs_management::MountedVolume* minfs;
  fs_management::NamespaceBinding minfs_bind;
  {
    auto res = ramnand.fvm_partition()->fvm().fs().OpenVolume(
        "minfs",
        fuchsia_fs_startup::wire::MountOptions::Builder(arena).uri("#meta/minfs.cm").Build());
    ASSERT_TRUE(res.is_ok()) << "Failed to create minfs: " << res.error_value();
    minfs = res.value();

    auto binding = fs_management::NamespaceBinding::Create("/minfs/", minfs->DataRoot().value());
    ASSERT_TRUE(binding.is_ok()) << binding.status_string();
    minfs_bind = std::move(binding.value());
  }

  mount_ = std::make_unique<MountedSystem>(MountedSystem{
      .ramnand = std::move(ramnand),
      .blobfs_export_root = blobfs->Release(),
      .blobfs_binding = std::move(blobfs_bind),
      .blob_creator = blobfs::BlobCreatorWrapper(std::move(blob_creator)),
      .minfs_export_root = minfs->Release(),
      .minfs_binding = std::move(minfs_bind),
  });
}

zx::result<Snapshot> WearSimulator::Snapshot() {
  struct Snapshot snapshot;

  // Taking a snapshot when remounting to ensure that the new component doesn't come up before the
  // old one dies and end up with two components modifying the device at once.
  zx::vmo vmo_snapshot;
  if (zx_status_t s = vmo_.create_child(ZX_VMO_CHILD_SNAPSHOT, 0, NandSize(config_.block_count),
                                        &snapshot.image);
      s != ZX_OK) {
    return zx::error(s);
  }
  // The two snapshots won't be atomic, but it won't matter much in the aggregate. Due to racing
  // with the ramnand component the erase and wear count increment will never be perfectly in sync
  // anyways, so it will always be racy.
  zx::vmo wear_snapshot;
  if (zx_status_t s = wear_vmo_.create_child(ZX_VMO_CHILD_SNAPSHOT, 0,
                                             WearSize(config_.block_count), &snapshot.wear_info);
      s != ZX_OK) {
    return zx::error(s);
  }

  return zx::ok(std::move(snapshot));
}

void WearSimulator::ExportImage() {
  auto snapshot = Snapshot();
  ASSERT_TRUE(snapshot.is_ok());

  // Write out the data vmo. Compressed with zstd.
  {
    fzl::OwnedVmoMapper compressed_mapper;
    size_t compressed_max_size = ZSTD_compressBound(NandSize(config_.block_count));
    ASSERT_EQ(compressed_mapper.CreateAndMap(compressed_max_size, "compressed_image"), ZX_OK);
    fzl::OwnedVmoMapper image_mapper;
    ASSERT_EQ(image_mapper.Map(std::move(snapshot->image), 0, NandSize(config_.block_count),
                               ZX_VM_PERM_READ),
              ZX_OK);

    size_t compressed_size;
    compressed_size = ZSTD_compress(compressed_mapper.start(), compressed_max_size,
                                    image_mapper.start(), NandSize(config_.block_count), 17);
    ASSERT_FALSE(ZSTD_isError(compressed_size)) << ZSTD_getErrorName(compressed_size);

    int f = open("custom_artifacts/nand.zstd", O_WRONLY | O_CREAT, 0644);
    ASSERT_GE(f, 0) << errno;
    uint8_t* offset = reinterpret_cast<uint8_t*>(compressed_mapper.start());
    while (compressed_size > 0) {
      ssize_t written = write(f, offset, compressed_size);
      ASSERT_GT(written, 0) << errno;
      compressed_size -= written;
      offset += written;
    }
    ASSERT_EQ(close(f), 0) << errno;
  }

  // Write out the wear info. No need to compress, it should be relatively small.
  {
    fzl::OwnedVmoMapper mapper;
    ASSERT_EQ(mapper.Map(std::move(snapshot->wear_info), 0, WearSize(config_.block_count),
                         ZX_VM_PERM_READ),
              ZX_OK);

    size_t size = WearSize(config_.block_count);

    int f = open("custom_artifacts/wear_info.bin", O_WRONLY | O_CREAT, 0644);
    ASSERT_GE(f, 0) << errno;
    uint8_t* offset = reinterpret_cast<uint8_t*>(mapper.start());
    while (size > 0) {
      ssize_t written = write(f, offset, size);
      ASSERT_GT(written, 0) << errno;
      size -= written;
      offset += written;
    }
    ASSERT_EQ(close(f), 0) << errno;
  }
}

// Test disabled because it isn't meant to run as part of CI. Meant for local experimentation.
TEST(Wear, DISABLED_LargeScale) {
  constexpr size_t kBlobUpdateSize = 178ul * 1024 * 1024;

  std::srand(testing::UnitTest::GetInstance()->random_seed());
  WearSimulator sim = WearSimulator({
      .fvm_slice_size = 32ul * 1024,
      .block_count = 1716,
      // Set up A/B partitions each with 2MB of breathing room so we don't fill up.
      .blobfs_partition_size = kBlobUpdateSize + (4ul * 1024 * 1024),
      .minfs_partition_size = 13ul * 1024 * 1024,
      .minfs_cold_data_size = 2ul * 1024 * 1024,
      .minfs_cycle_data_size = 2ul * 1024 * 1024,
  });
  sim.Init();
  sim.FillBlobfs(kBlobUpdateSize * 2);

  // Perform a number of cycles between updates.
  for (int i = 0; i < 2; i++) {
    sim.SimulateMinfs(400000);
    size_t reduce_by = kBlobUpdateSize;
    sim.ReduceBlobfsBy(&reduce_by);
    sim.FillBlobfs(kBlobUpdateSize - reduce_by);
  }

  ASSERT_TRUE(sim.RemountFtl().is_ok());
}

// A minimal test meant to be fast while exploring the full range of operations.
TEST(Wear, MinimalSimulator) {
  std::srand(testing::UnitTest::GetInstance()->random_seed());
  WearSimulator sim = WearSimulator({
      .fvm_slice_size = 32ul * 1024,
      .block_count = 100,
      .blobfs_partition_size = 10ul * 1024 * 1024,
      .minfs_partition_size = 10ul * 1024 * 1024,
      .minfs_cold_data_size = 2ul * 1024 * 1024,
      .minfs_cycle_data_size = 2ul * 1024 * 1024,
  });
  sim.Init();
  sim.FillBlobfs(2ul * 1024 * 1024);
  sim.SimulateMinfs(100);
  size_t reduce_by = 1ul * 1024 * 1024;
  sim.ReduceBlobfsBy(&reduce_by);
  sim.FillBlobfs(1ul * 1024 * 1024 - reduce_by);
  sim.SimulateMinfs(100);

  sim.Reboot();

  sim.SimulateMinfs(100);
  reduce_by = 1ul * 1024 * 1024;
  sim.ReduceBlobfsBy(&reduce_by);
  sim.FillBlobfs(1ul * 1024 * 1024 - reduce_by);
  sim.SimulateMinfs(100);

  // The image compresses to ~40K in the test. So leave it in.
  sim.ExportImage();
  ASSERT_TRUE(sim.RemountFtl().is_ok());
}

TEST(Wear, ImageImport) {
  // Must match the size settings from MinimalSimulator which generated this image.
  WearSimulator sim = WearSimulator({
      .fvm_slice_size = 32ul * 1024,
      .block_count = 100,
      .blobfs_partition_size = 10ul * 1024 * 1024,
      .minfs_partition_size = 10ul * 1024 * 1024,
  });
  sim.InitFromImage("pkg/testdata/nand.zstd", "pkg/testdata/wear_info.bin");
}

}  // namespace
}  // namespace fs_test
