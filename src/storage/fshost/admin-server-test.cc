// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fidl/fuchsia.fs/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/namespace.h>
#include <lib/fdio/vfs.h>
#include <sys/stat.h>
#include <sys/statvfs.h>

#include <cstddef>

#include <gtest/gtest.h>

#include "sdk/lib/fdio/include/lib/fdio/spawn.h"
#include "src/lib/storage/fs_management/cpp/admin.h"
#include "src/lib/storage/fs_management/cpp/mount.h"
#include "src/storage/fshost/constants.h"
#include "src/storage/fshost/testing/fshost_integration_test.h"
#include "src/storage/testing/fvm.h"
#include "src/storage/testing/ram_disk.h"

namespace fshost {
namespace {

using AdminServerTest = testing::FshostIntegrationTest;

TEST_F(AdminServerTest, GetDevicePathForBuiltInFilesystem) {
  constexpr uint32_t kBlockCount = 9 * 1024 * 256;
  constexpr uint32_t kBlockSize = 512;
  constexpr uint32_t kSliceSize = 32'768;
  constexpr size_t kDeviceSize = static_cast<const size_t>(kBlockCount) * kBlockSize;

  constexpr const char* kFshostBindPath = "/fshost";
  constexpr const char* kFshostSvcPath = "/fshost/fuchsia.fshost.Admin";
  {
    // Bind Fshost's exposed directory into the namespace so the mount/umount binaries can reference
    // it.
    fdio_ns_t* ns = nullptr;
    ASSERT_EQ(fdio_ns_get_installed(&ns), ZX_OK);
    ASSERT_EQ(fdio_ns_bind(ns, kFshostBindPath, exposed_dir().client_end().channel().get()), ZX_OK);
  }

  PauseWatcher();  // Pause whilst we create a ramdisk.

  // Create a ramdisk with an unformatted minfs partitition.
  zx::vmo vmo;
  ASSERT_EQ(zx::vmo::create(kDeviceSize, 0, &vmo), ZX_OK);

  // Create a child VMO so that we can keep hold of the original.
  zx::vmo child_vmo;
  ASSERT_EQ(vmo.create_child(ZX_VMO_CHILD_SLICE, 0, kDeviceSize, &child_vmo), ZX_OK);

  // Now create the ram-disk with a single FVM partition.
  {
    auto ramdisk_or = storage::RamDisk::CreateWithVmo(std::move(child_vmo), kBlockSize);
    ASSERT_EQ(ramdisk_or.status_value(), ZX_OK);
    storage::FvmOptions options{
        .name = kDataPartitionLabel,
        .type = std::array<uint8_t, BLOCK_GUID_LEN>{GUID_DATA_VALUE},
    };
    auto fvm_partition_or = storage::CreateFvmPartition(ramdisk_or->path(), kSliceSize, options);
    ASSERT_EQ(fvm_partition_or.status_value(), ZX_OK);
  }

  ResumeWatcher();

  // Now reattach the ram-disk and fshost should format it.
  auto ramdisk_or = storage::RamDisk::CreateWithVmo(std::move(vmo), kBlockSize);
  ASSERT_EQ(ramdisk_or.status_value(), ZX_OK);
  auto [fd, fs_type] = WaitForMount("data");
  ASSERT_TRUE(fd);
  auto expected_fs_type = fuchsia_fs::VfsType::kMinfs;
  if (DataFilesystemFormat() == "f2fs") {
    expected_fs_type = fuchsia_fs::VfsType::kF2Fs;
  } else if (DataFilesystemFormat() == "fxfs") {
    expected_fs_type = fuchsia_fs::VfsType::kFxfs;
  }
  EXPECT_EQ(fs_type, fidl::ToUnderlying(expected_fs_type));

  struct statvfs buf;
  ASSERT_EQ(fstatvfs(fd.get(), &buf), 0);

  auto fshost_or = component::Connect<fuchsia_fshost::Admin>(kFshostSvcPath);
  ASSERT_EQ(fshost_or.status_value(), ZX_OK);

  // The device path is registered in fshost *after* the mount point shows up so this is racy.  It's
  // not worth fixing fshost since the device path is used for debugging/diagnostics, so we just
  // loop here.
  int attempts = 0;
  for (;;) {
    auto result = fidl::WireCall(*fshost_or)->GetDevicePath(buf.f_fsid);
    ASSERT_TRUE(result.ok());
    if (result.value().is_error()) {
      if (++attempts == 100)
        GTEST_FAIL() << "Timed out trying to get device path";
      usleep(100'000);
    } else {
      if (DataFilesystemFormat() == "fxfs") {
        // Fxfs doesn't use zxcrypt.
        EXPECT_EQ(result.value().value()->path.get(), ramdisk_or->path() + "/fvm/data-p-1/block");
      } else {
        EXPECT_EQ(result.value().value()->path.get(),
                  ramdisk_or->path() + "/fvm/data-p-1/block/zxcrypt/unsealed/block");
      }
      break;
    }
  }
}

}  // namespace
}  // namespace fshost
