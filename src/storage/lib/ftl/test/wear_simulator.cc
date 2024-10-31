// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/directory.h>
#include <lib/fzl/owned-vmo-mapper.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/vmo.h>
#include <unistd.h>
#include <zircon/types.h>

#include <filesystem>
#include <memory>
#include <set>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/storage/blobfs/test/blob_utils.h"
#include "src/storage/fs_test/fs_test.h"
#include "src/storage/lib/fs_management/cpp/mount.h"
#include "src/storage/testing/fvm.h"

namespace fs_test {
namespace {
constexpr size_t kPageSize = 8192;

struct SystemConfig {
  size_t fvm_slice_size;

  // The size in bytes of the nand storage. Needs to account for the spare area bytes.
  size_t nand_size;

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

  fidl::ClientEnd<fuchsia_io::Directory> minfs_export_root;
  fs_management::NamespaceBinding minfs_binding;
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

  // Simulates a number of operations or "cycles" in minfs.
  void SimulateMinfs(int cycles);

  // Fills Blobfs with randomly sized blobs to target the required space.
  void FillBlobfs(size_t space);

  // Attempts to reduce blobfs by the requested size. Due to varying blob sizes it may not be able
  // to do so exactly. The number of bytes it was unable to reduce by will be left in the space
  // argument.
  void ReduceBlobfsBy(size_t* space);

  // Tears down the current system in the given simulator and remounts the ftl one final time in
  // order to dump the wear information to logs.
  static void RemountFtl(WearSimulator&& simulator);

 private:
  zx::vmo vmo_;
  SystemConfig config_;
  std::unique_ptr<MountedSystem> mount_;
};

// Creates the directories and initial state files inside minfs.
void InitMinfs(const char* root_path, const SystemConfig& config) {
  constexpr size_t kMaxWritePages = 64ul;
  constexpr size_t kMaxWriteSize = kMaxWritePages * kPageSize;
  char path_buf[255];
  auto write_buf = std::make_unique<char[]>(kMaxWriteSize);
  memset(write_buf.get(), 0xAB, kMaxWriteSize);

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
  ASSERT_EQ(mapper.CreateAndMap(config_.nand_size, "wear-test-vmo"), ZX_OK);
  memset(mapper.start(), 0xff, config_.nand_size);
  vmo_ = mapper.Release();
  ASSERT_TRUE(vmo_.is_valid());

  auto res = CreateRamDevice({
      .use_ram_nand = true,
      .vmo = vmo_.borrow(),
      .use_fvm = true,
      .device_block_size = kPageSize,
      .device_block_count = 0,
      .fvm_slice_size = config_.fvm_slice_size,
  });
  ASSERT_TRUE(res.is_ok()) << "Failed to setup ram device: " << res.error_value();
  RamDevice ramnand = std::move(res.value());

  fidl::Arena arena;
  fs_management::MountedVolume* blobfs;
  fs_management::NamespaceBinding blobfs_bind;
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

  InitMinfs(minfs_bind.path().c_str(), config_);

  mount_ = std::make_unique<MountedSystem>(MountedSystem{
      .ramnand = std::move(ramnand),
      .blobfs_export_root = blobfs->Release(),
      .blobfs_binding = std::move(blobfs_bind),
      .minfs_export_root = minfs->Release(),
      .minfs_binding = std::move(minfs_bind),
  });
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

  int num_cycle_files = 0;
  sprintf(path_buf, "%s/cycle", root_path);
  for (const auto& entry : std::filesystem::directory_iterator(path_buf)) {
    if (entry.path().c_str()[0] != '.') {
      num_cycle_files++;
    }
  }

  memset(write_buf.get(), 0xAB, kMaxWriteSize);
  for (int i = 0; i < cycles; i++) {
    switch (rand() % 2) {
      case 0: {
        // Cycle a file.
        int index = rand() % num_cycle_files;
        sprintf(path_buf, "%s/cycle/%d", root_path, index);
        size_t size = std::filesystem::file_size(path_buf);

        int f = open(temp_path_buf, O_WRONLY | O_CREAT);
        ASSERT_GE(f, 0);
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

  // Random data in blob to avoid compression. Get actual sizing.
  for (; space > 0;) {
    size_t max_pages = std::min(space, kMaxBlobSize) / kPageSize;
    size_t size;
    if (max_pages <= 1) {
      size = kPageSize;
    } else {
      size = ((rand() % (max_pages - 1)) + 1) * kPageSize;
    }
    std::unique_ptr<blobfs::BlobInfo> info =
        blobfs::GenerateRandomBlob(mount_->blobfs_binding.path(), size);
    fbl::unique_fd fd;
    ASSERT_NO_FATAL_FAILURE(blobfs::MakeBlob(*info, &fd));
    ASSERT_EQ(close(fd.release()), 0);
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

void WearSimulator::RemountFtl(WearSimulator&& simulator) {
  ASSERT_TRUE(simulator.mount_) << "Wear simulator not initialized";
  simulator.mount_.reset();
  zx::vmo vmo_snapshot;
  ASSERT_EQ(simulator.vmo_.create_child(ZX_VMO_CHILD_SNAPSHOT, 0, simulator.config_.nand_size,
                                        &vmo_snapshot),
            ZX_OK);
  RamDevice ramnand = CreateRamDevice({
                                          .use_ram_nand = true,
                                          .vmo = vmo_snapshot.borrow(),
                                          .use_fvm = true,
                                          .device_block_size = kPageSize,
                                          .device_block_count = 0,
                                          .fvm_slice_size = simulator.config_.fvm_slice_size,
                                      })
                          .value();
}

// Test disabled because it isn't meant to run as part of CI. Meant for local experimentation.
TEST(Wear, DISABLED_LargeScale) {
  // 1716 blocks containing 64 pages of 4 KiB with 8 bytes OOB
  constexpr int kSize = 1716 * 64 * (4096 + 8);
  constexpr size_t kBlobUpdateSize = 178ul * 1024 * 1024;

  std::srand(testing::UnitTest::GetInstance()->random_seed());
  WearSimulator sim = WearSimulator({
      .fvm_slice_size = 32ul * 1024,
      .nand_size = kSize,
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

  WearSimulator::RemountFtl(std::move(sim));
}

// A minimal test meant to be fast while exploring the full range of operations.
TEST(Wear, MinimalSimulator) {
  // 100 blocks containing 64 pages of 4 KiB with 8 bytes OOB
  constexpr int kSize = 100 * 64 * (4096 + 8);

  std::srand(testing::UnitTest::GetInstance()->random_seed());
  WearSimulator sim = WearSimulator({
      .fvm_slice_size = 32ul * 1024,
      .nand_size = kSize,
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

  WearSimulator::RemountFtl(std::move(sim));
}

}  // namespace
}  // namespace fs_test
