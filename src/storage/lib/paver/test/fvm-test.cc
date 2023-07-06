// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/fvm.h"

#include <lib/component/incoming/cpp/clone.h>
#include <lib/driver-integration-test/fixture.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/zx/result.h>

#include <zxtest/zxtest.h>

#include "src/lib/storage/fs_management/cpp/fvm.h"
#include "src/storage/fvm/format.h"
#include "src/storage/fvm/fvm_sparse.h"
#include "src/storage/lib/paver/test/test-utils.h"

namespace {

using device_watcher::RecursiveWaitForFile;
using driver_integration_test::IsolatedDevmgr;

constexpr size_t kSliceSize = kBlockSize * 2;
constexpr uint8_t kFvmType[GPT_GUID_LEN] = GUID_FVM_VALUE;

constexpr fvm::SparseImage SparseHeaderForSliceSize(size_t slice_size) {
  fvm::SparseImage header = {};
  header.slice_size = slice_size;
  return header;
}

constexpr fvm::SparseImage SparseHeaderForSliceSizeAndMaxDiskSize(size_t slice_size,
                                                                  size_t max_disk_size) {
  fvm::SparseImage header = SparseHeaderForSliceSize(slice_size);
  header.maximum_disk_size = max_disk_size;
  return header;
}

class FvmTest : public zxtest::Test {
 public:
  FvmTest() {
    IsolatedDevmgr::Args args;
    args.disable_block_watcher = true;

    ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr_));

    ASSERT_OK(RecursiveWaitForFile(devmgr_.devfs_root().get(), "sys/platform/00:00:2d/ramctl")
                  .status_value());
  }

  void CreateRamdisk() { CreateRamdiskWithBlockCount(); }

  void CreateRamdiskWithBlockCount(size_t block_count = kBlockCount) {
    ASSERT_NO_FATAL_FAILURE(
        BlockDevice::Create(devmgr_.devfs_root(), kFvmType, block_count, &device_));
    ASSERT_TRUE(device_);
  }

  fidl::UnownedClientEnd<fuchsia_hardware_block::Block> block_interface() {
    return device_->block_interface();
  }

  const fbl::unique_fd& devfs_root() { return devmgr_.devfs_root(); }

 protected:
  IsolatedDevmgr devmgr_;
  std::unique_ptr<BlockDevice> device_;
};

TEST_F(FvmTest, FormatFvmEmpty) {
  ASSERT_NO_FAILURES(CreateRamdisk());
  ASSERT_OK(FvmPartitionFormat(devfs_root(), device_->block_interface(),
                               device_->block_controller_interface(),
                               SparseHeaderForSliceSize(kSliceSize), paver::BindOption::Reformat));
}

TEST_F(FvmTest, TryBindEmpty) {
  ASSERT_NO_FAILURES(CreateRamdisk());
  ASSERT_OK(FvmPartitionFormat(devfs_root(), device_->block_interface(),
                               device_->block_controller_interface(),
                               SparseHeaderForSliceSize(kSliceSize), paver::BindOption::TryBind));
}

TEST_F(FvmTest, TryBindAlreadyFormatted) {
  ASSERT_NO_FAILURES(CreateRamdisk());
  ASSERT_OK(fs_management::FvmInit(block_interface(), kSliceSize));
  ASSERT_OK(FvmPartitionFormat(devfs_root(), device_->block_interface(),
                               device_->block_controller_interface(),
                               SparseHeaderForSliceSize(kSliceSize), paver::BindOption::TryBind));
}

TEST_F(FvmTest, TryBindAlreadyBound) {
  ASSERT_NO_FAILURES(CreateRamdisk());
  ASSERT_OK(FvmPartitionFormat(devfs_root(), device_->block_interface(),
                               device_->block_controller_interface(),
                               SparseHeaderForSliceSize(kSliceSize), paver::BindOption::Reformat));

  ASSERT_OK(FvmPartitionFormat(devfs_root(), device_->block_interface(),
                               device_->block_controller_interface(),
                               SparseHeaderForSliceSize(kSliceSize), paver::BindOption::TryBind));
}

TEST_F(FvmTest, TryBindAlreadyFormattedWrongSliceSize) {
  ASSERT_NO_FAILURES(CreateRamdisk());
  ASSERT_OK(fs_management::FvmInit(block_interface(), kSliceSize * 2));
  ASSERT_OK(FvmPartitionFormat(devfs_root(), device_->block_interface(),
                               device_->block_controller_interface(),
                               SparseHeaderForSliceSize(kSliceSize), paver::BindOption::TryBind));
}

TEST_F(FvmTest, TryBindAlreadyFormattedWithSmallerSize) {
  constexpr size_t kBlockDeviceInitialSize = 1000 * kSliceSize;
  constexpr size_t kBlockDeviceMaxSize = 100000 * kSliceSize;
  ASSERT_NO_FAILURES(CreateRamdiskWithBlockCount(kBlockDeviceMaxSize / kBlockSize));
  ASSERT_OK(fs_management::FvmInitPreallocated(block_interface(), kBlockDeviceInitialSize,
                                               kBlockDeviceMaxSize, kSliceSize));
  // Same slice size but can reference up to 200 Slices, which is far less than what the
  // preallocated can have.
  fvm::SparseImage header =
      SparseHeaderForSliceSizeAndMaxDiskSize(kSliceSize, 2 * kBlockDeviceInitialSize);
  paver::FormatResult result;
  ASSERT_OK(FvmPartitionFormat(devfs_root(), device_->block_interface(),
                               device_->block_controller_interface(), header,
                               paver::BindOption::TryBind, &result));
  ASSERT_EQ(paver::FormatResult::kPreserved, result);
}

TEST_F(FvmTest, TryBindAlreadyFormattedWithBiggerSize) {
  constexpr size_t kBlockDeviceInitialSize = 1000 * kSliceSize;
  constexpr size_t kBlockDeviceMaxSize = 100000 * kSliceSize;
  ASSERT_NO_FAILURES(CreateRamdiskWithBlockCount(kBlockDeviceMaxSize / kBlockSize));
  ASSERT_OK(fs_management::FvmInitPreallocated(block_interface(), kBlockDeviceInitialSize,
                                               kBlockDeviceMaxSize / 100, kSliceSize));
  // Same slice size but can reference up to 200 Slices, which is far less than what the
  // preallocated can have.
  fvm::SparseImage header = SparseHeaderForSliceSizeAndMaxDiskSize(kSliceSize, kBlockDeviceMaxSize);
  paver::FormatResult result;
  ASSERT_OK(FvmPartitionFormat(devfs_root(), device_->block_interface(),
                               device_->block_controller_interface(), header,
                               paver::BindOption::TryBind, &result));
  ASSERT_EQ(paver::FormatResult::kReformatted, result);
}

constexpr char kRamdisk0BlobPath[] =
    "sys/platform/00:00:2d/ramctl/ramdisk-0/block/fvm/blobfs-p-1/block";
constexpr char kRamdisk0DataPath[] =
    "sys/platform/00:00:2d/ramctl/ramdisk-0/block/fvm/data-p-2/block";
constexpr char kRamdisk1BlobPath[] =
    "sys/platform/00:00:2d/ramctl/ramdisk-1/block/fvm/blobfs-p-1/block";
constexpr char kRamdisk1DataPath[] =
    "sys/platform/00:00:2d/ramctl/ramdisk-1/block/fvm/data-p-2/block";

TEST_F(FvmTest, AllocateEmptyPartitions) {
  ASSERT_NO_FAILURES(CreateRamdisk());
  zx::result fvm = FvmPartitionFormat(
      devfs_root(), device_->block_interface(), device_->block_controller_interface(),
      SparseHeaderForSliceSize(kSliceSize), paver::BindOption::Reformat);
  ASSERT_OK(fvm);

  zx::result volume_endpoints =
      fidl::CreateEndpoints<fuchsia_hardware_block_volume::VolumeManager>();
  ASSERT_OK(volume_endpoints);
  auto& [volume, volume_server] = volume_endpoints.value();
  ASSERT_OK(fidl::WireCall(fvm.value())->ConnectToDeviceFidl(volume_server.TakeChannel()).status());
  ASSERT_OK(paver::AllocateEmptyPartitions(devfs_root(), volume));

  fbl::unique_fd blob;
  ASSERT_OK(
      fdio_open_fd_at(devfs_root().get(), kRamdisk0BlobPath, 0, blob.reset_and_get_address()));

  fbl::unique_fd data;
  ASSERT_OK(
      fdio_open_fd_at(devfs_root().get(), kRamdisk0DataPath, 0, data.reset_and_get_address()));
}

TEST_F(FvmTest, WipeWithMultipleFvm) {
  ASSERT_NO_FAILURES(CreateRamdisk());
  zx::result fvm1 = FvmPartitionFormat(
      devfs_root(), device_->block_interface(), device_->block_controller_interface(),
      SparseHeaderForSliceSize(kSliceSize), paver::BindOption::Reformat);
  ASSERT_OK(fvm1);

  zx::result volume_endpoints1 =
      fidl::CreateEndpoints<fuchsia_hardware_block_volume::VolumeManager>();
  ASSERT_OK(volume_endpoints1);
  auto& [volume1, volume_server1] = volume_endpoints1.value();
  ASSERT_OK(
      fidl::WireCall(fvm1.value())->ConnectToDeviceFidl(volume_server1.TakeChannel()).status());
  ASSERT_OK(paver::AllocateEmptyPartitions(devfs_root(), volume1));

  {
    fbl::unique_fd blob;
    ASSERT_OK(
        fdio_open_fd_at(devfs_root().get(), kRamdisk0BlobPath, 0, blob.reset_and_get_address()));

    fbl::unique_fd data;
    ASSERT_OK(
        fdio_open_fd_at(devfs_root().get(), kRamdisk0DataPath, 0, data.reset_and_get_address()));
  }

  // Save the old device.
  std::unique_ptr<BlockDevice> first_device = std::move(device_);

  ASSERT_NO_FAILURES(CreateRamdisk());
  zx::result fvm2 = FvmPartitionFormat(
      devfs_root(), device_->block_interface(), device_->block_controller_interface(),
      SparseHeaderForSliceSize(kSliceSize), paver::BindOption::Reformat);
  ASSERT_OK(fvm2);

  zx::result volume_endpoints2 =
      fidl::CreateEndpoints<fuchsia_hardware_block_volume::VolumeManager>();
  ASSERT_OK(volume_endpoints2);
  auto& [volume2, volume_server2] = volume_endpoints2.value();
  ASSERT_OK(
      fidl::WireCall(fvm2.value())->ConnectToDeviceFidl(volume_server2.TakeChannel()).status());
  ASSERT_OK(paver::AllocateEmptyPartitions(devfs_root(), volume2));

  {
    fbl::unique_fd blob;
    ASSERT_OK(
        fdio_open_fd_at(devfs_root().get(), kRamdisk1BlobPath, 0, blob.reset_and_get_address()));

    fbl::unique_fd data;
    ASSERT_OK(
        fdio_open_fd_at(devfs_root().get(), kRamdisk1DataPath, 0, data.reset_and_get_address()));
  }

  std::array<uint8_t, fvm::kGuidSize> blobfs_guid = GUID_BLOB_VALUE;
  ASSERT_OK(paver::WipeAllFvmPartitionsWithGuid(fvm2.value(), blobfs_guid.data()));

  // Check we can still open the first ramdisk's blobfs:
  {
    fbl::unique_fd blob;
    ASSERT_OK(
        fdio_open_fd_at(devfs_root().get(), kRamdisk0BlobPath, 0, blob.reset_and_get_address()));
  }

  // But not the second's.
  {
    fbl::unique_fd blob;
    ASSERT_OK(
        fdio_open_fd_at(devfs_root().get(), kRamdisk1BlobPath, 0, blob.reset_and_get_address()));
  }
}

TEST_F(FvmTest, Unbind) {
  ASSERT_NO_FAILURES(CreateRamdisk());
  zx::result fvm = FvmPartitionFormat(
      devfs_root(), device_->block_interface(), device_->block_controller_interface(),
      SparseHeaderForSliceSize(kSliceSize), paver::BindOption::Reformat);
  ASSERT_OK(fvm);

  zx::result volume_endpoints =
      fidl::CreateEndpoints<fuchsia_hardware_block_volume::VolumeManager>();
  ASSERT_OK(volume_endpoints);
  auto& [volume, volume_server] = volume_endpoints.value();
  ASSERT_OK(fidl::WireCall(fvm.value())->ConnectToDeviceFidl(volume_server.TakeChannel()).status());
  ASSERT_OK(paver::AllocateEmptyPartitions(devfs_root(), volume));

  fbl::unique_fd blob;
  ASSERT_OK(
      fdio_open_fd_at(devfs_root().get(), kRamdisk0BlobPath, 0, blob.reset_and_get_address()));

  fbl::unique_fd data;
  ASSERT_OK(
      fdio_open_fd_at(devfs_root().get(), kRamdisk0DataPath, 0, data.reset_and_get_address()));

  ASSERT_OK(paver::FvmUnbind(devfs_root(), "/dev/sys/platform/00:00:2d/ramctl/ramdisk-0/block"));
}

TEST_F(FvmTest, UnbindInvalidPath) {
  ASSERT_NO_FAILURES(CreateRamdisk());
  zx::result fvm = FvmPartitionFormat(
      devfs_root(), device_->block_interface(), device_->block_controller_interface(),
      SparseHeaderForSliceSize(kSliceSize), paver::BindOption::Reformat);
  ASSERT_OK(fvm);

  zx::result volume_endpoints =
      fidl::CreateEndpoints<fuchsia_hardware_block_volume::VolumeManager>();
  ASSERT_OK(volume_endpoints);
  auto& [volume, volume_server] = volume_endpoints.value();
  ASSERT_OK(fidl::WireCall(fvm.value())->ConnectToDeviceFidl(volume_server.TakeChannel()).status());

  ASSERT_OK(paver::AllocateEmptyPartitions(devfs_root(), volume));

  fbl::unique_fd blob;
  ASSERT_OK(
      fdio_open_fd_at(devfs_root().get(), kRamdisk0BlobPath, 0, blob.reset_and_get_address()));

  fbl::unique_fd data;
  ASSERT_OK(
      fdio_open_fd_at(devfs_root().get(), kRamdisk0DataPath, 0, data.reset_and_get_address()));

  // Path too short
  ASSERT_EQ(paver::FvmUnbind(devfs_root(), "/dev"), ZX_ERR_INVALID_ARGS);

  // Path too long
  char path[PATH_MAX + 2];
  memset(path, 'a', sizeof(path));
  path[sizeof(path) - 1] = '\0';
  ASSERT_EQ(paver::FvmUnbind(devfs_root(), path), ZX_ERR_INVALID_ARGS);

  ASSERT_OK(paver::FvmUnbind(devfs_root(), "/dev/sys/platform/00:00:2d/ramctl/ramdisk-0/block"));
}

}  // namespace
