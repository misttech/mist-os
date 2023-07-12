// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "factory_reset.h"

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.fshost/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fidl/fuchsia.hardware.power.statecontrol/cpp/wire_test_base.h>
#include <lib/component/incoming/cpp/clone.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/driver-integration-test/fixture.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/fd.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/fit/defer.h>
#include <lib/zx/vmo.h>
#include <zircon/hw/gpt.h>

#include <string_view>

#include <fbl/algorithm.h>
#include <gtest/gtest.h>
#include <ramdevice-client/ramdisk.h>
#include <src/lib/uuid/uuid.h>

#include "src/lib/storage/block_client/cpp/remote_block_device.h"
#include "src/lib/storage/fs_management/cpp/format.h"
#include "src/lib/storage/fs_management/cpp/fvm.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "src/security/lib/fcrypto/secret.h"
#include "src/security/lib/zxcrypt/client.h"

namespace {

using driver_integration_test::IsolatedDevmgr;

constexpr uint32_t kBlockCount = 1024 * 256;
constexpr uint32_t kBlockSize = 512;
constexpr uint32_t kSliceSize = (1 << 20);
constexpr uint32_t kDeviceSize = kBlockCount * kBlockSize;
constexpr std::string_view kDataName = "fdr-data";
constexpr char kRamCtlPath[] = "sys/platform/00:00:2d/ramctl";
constexpr size_t kKeyBytes = 32;  // Generate a 256-bit key for the zxcrypt volume

class MockAdmin : public fidl::testing::WireTestBase<fuchsia_hardware_power_statecontrol::Admin> {
 public:
  bool suspend_called() const { return suspend_called_; }

 private:
  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    ADD_FAILURE() << "'" << name << "' was called unexpectedly";
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  void Reboot(RebootRequestView request, RebootCompleter::Sync& completer) override {
    ASSERT_FALSE(suspend_called_);
    suspend_called_ = true;
    ASSERT_EQ(request->reason,
              fuchsia_hardware_power_statecontrol::RebootReason::kFactoryDataReset);
    completer.ReplySuccess();
  }

  bool suspend_called_ = false;
};

class MockFshostAdmin : public fidl::testing::WireTestBase<fuchsia_fshost::Admin> {
 public:
  bool shred_data_volume_called() const { return shred_data_volume_called_; }

 private:
  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    ADD_FAILURE() << "'" << name << "' was called unexpectedly";
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  void ShredDataVolume(ShredDataVolumeCompleter::Sync& completer) override {
    shred_data_volume_called_ = true;
    completer.ReplySuccess();
  }

  bool shred_data_volume_called_ = false;
};

class FactoryResetTest : public gtest::RealLoopFixture {
 protected:
  // Create an IsolatedDevmgr that can load device drivers such as fvm,
  // zxcrypt, etc.
  void SetUp() override {
    IsolatedDevmgr::Args args;
    args.disable_block_watcher = true;

    ASSERT_EQ(IsolatedDevmgr::Create(&args, &devmgr_), ZX_OK);

    CreateRamdisk();
    CreateFvmPartition();
  }

  void TearDown() override { ASSERT_EQ(ramdisk_destroy(ramdisk_client_), ZX_OK); }

  void WithPartitionHasFormat(void (*fn)(fs_management::DiskFormat)) {
    fdio_cpp::UnownedFdioCaller caller(devfs_root_fd());
    zx::result channel =
        component::ConnectAt<fuchsia_hardware_block::Block>(caller.directory(), fvm_block_path_);
    ASSERT_TRUE(channel.is_ok()) << channel.status_string();
    fn(fs_management::DetectDiskFormat(channel.value()));
  }

  void CreateZxcrypt() {
    std::string controller_path = fvm_block_path_ + "/device_controller";
    zx::result channel = WaitForDevice(controller_path);
    ASSERT_EQ(channel.status_value(), ZX_OK);

    zxcrypt::VolumeManager zxcrypt_volume(
        fidl::ClientEnd<fuchsia_device::Controller>(std::move(channel.value())),
        devfs_root_fd().duplicate());
    zx::channel zxc_manager_chan;
    ASSERT_EQ(zxcrypt_volume.OpenClient(zx::duration::infinite(), zxc_manager_chan), ZX_OK);

    // Use an explicit key for this test volume.  Other key sources may not be
    // available in the isolated test environment.
    crypto::Secret key;
    ASSERT_EQ(key.Generate(kKeyBytes), ZX_OK);
    zxcrypt::EncryptedVolumeClient volume_client(std::move(zxc_manager_chan));

    ASSERT_EQ(volume_client.Format(key.get(), key.len(), 0), ZX_OK);
    ASSERT_EQ(volume_client.Unseal(key.get(), key.len(), 0), ZX_OK);
    WaitForZxcrypt();
  }

  void CreateCorruptedZxcrypt() {
    zx::result channel = WaitForDevice(fvm_block_path_);
    ASSERT_EQ(channel.status_value(), ZX_OK);
    fidl::ClientEnd<fuchsia_hardware_block::Block> client_end(std::move(channel.value()));

    // Write just the zxcrypt magic at the start of the volume.
    // It will not be possible to unseal this device, but we want to ensure that
    // factory reset completes anyway and shreds what key material would reside
    // in that block.

    // Prepare a buffer of the native block size that starts with zxcrypt_magic.
    // Block reads and writes via fds must match the block size.
    ssize_t block_size;
    GetBlockSize(client_end, &block_size);
    std::unique_ptr block = std::make_unique<uint8_t[]>(block_size);
    memset(block.get(), 0, block_size);
    memcpy(block.get(), fs_management::kZxcryptMagic, sizeof(fs_management::kZxcryptMagic));
    ASSERT_NO_FATAL_FAILURE(WriteBlocks(client_end, block.get(), block_size, 0));
  }

  void CreateFakeBlobfs() {
    // Writes just the blobfs magic at the start of the volume, just as something
    // else we expect to detect so we can see if the block gets randomized later
    // or not.

    zx::result channel = WaitForDevice(fvm_block_path_);
    ASSERT_EQ(channel.status_value(), ZX_OK);
    fidl::ClientEnd<fuchsia_hardware_block::Block> client_end(std::move(channel.value()));

    // Prepare a buffer of the native block size that starts with blobfs_magic.
    // Block reads and writes via fds must match the block size.
    ssize_t block_size;
    GetBlockSize(client_end, &block_size);
    std::unique_ptr block = std::make_unique<uint8_t[]>(block_size);
    memset(block.get(), 0, block_size);
    memcpy(block.get(), fs_management::kBlobfsMagic, sizeof(fs_management::kBlobfsMagic));
    ASSERT_NO_FATAL_FAILURE(WriteBlocks(client_end, block.get(), block_size, 0));
  }

  void CreateFakeFxfs() {
    // Writes just the Fxfs magic byte sequence (FxfsSupr) so that we detect that the filesystem
    // is Fxfs and shred it accordingly.
    zx::result channel = WaitForDevice(fvm_block_path_);
    ASSERT_EQ(channel.status_value(), ZX_OK);
    fidl::ClientEnd<fuchsia_hardware_block::Block> client_end(std::move(channel.value()));

    ssize_t block_size;
    GetBlockSize(client_end, &block_size);
    std::unique_ptr block = std::make_unique<uint8_t[]>(block_size);
    memset(block.get(), 0, block_size);

    // Initialize one megabyte of NULL for the A/B super block extents.
    const size_t num_blocks = (1L << 20) / block_size;
    for (size_t i = 0; i < num_blocks; i++) {
      ASSERT_NO_FATAL_FAILURE(WriteBlocks(client_end, block.get(), block_size, i * block_size));
    }

    // Add magic bytes at the correct offsets.
    memcpy(block.get(), fs_management::kFxfsMagic, sizeof(fs_management::kFxfsMagic));
    for (off_t ofs : {0L, 512L << 10}) {
      ASSERT_NO_FATAL_FAILURE(WriteBlocks(client_end, block.get(), block_size, ofs));
    }
  }

  const fbl::unique_fd& devfs_root_fd() const { return devmgr_.devfs_root(); }

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> devfs_root() const {
    fdio_cpp::UnownedFdioCaller caller(devfs_root_fd());
    return component::Clone(caller.directory());
  }

  void RunReset(fit::function<void(const MockAdmin&)> cb,
                std::optional<std::reference_wrapper<MockFshostAdmin>> mock_fshost = std::nullopt) {
    zx::result dev = devfs_root();
    ASSERT_TRUE(dev.is_ok()) << dev.status_string();
    MockAdmin mock_admin;
    fidl::ServerBindingGroup<fuchsia_hardware_power_statecontrol::Admin> binding;
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_power_statecontrol::Admin>();
    ASSERT_TRUE(endpoints.is_ok()) << endpoints.status_string();
    auto& [admin, server_end] = endpoints.value();
    binding.AddBinding(dispatcher(), std::move(server_end), &mock_admin,
                       fidl::kIgnoreBindingClosure);

    std::optional<fidl::ServerBindingGroup<fuchsia_fshost::Admin>> fshost_binding;
    fidl::ClientEnd<fuchsia_fshost::Admin> fshost_admin;
    if (mock_fshost.has_value()) {
      zx::result fshost_server_end = fidl::CreateEndpoints<fuchsia_fshost::Admin>(&fshost_admin);
      ASSERT_TRUE(fshost_server_end.is_ok()) << fshost_server_end.status_string();
      fshost_binding.emplace().AddBinding(dispatcher(), std::move(fshost_server_end.value()),
                                          &mock_fshost.value().get(), fidl::kIgnoreBindingClosure);
    } else {
      zx::result result = component::ConnectAt<fuchsia_fshost::Admin>(devmgr_.fshost_svc_dir());
      ASSERT_TRUE(result.is_ok()) << result.status_string();
      fshost_admin = std::move(result.value());
    }

    factory_reset::FactoryReset reset(dispatcher(), std::move(dev.value()), std::move(admin),
                                      std::move(fshost_admin));

    std::optional<zx_status_t> status;
    reset.Reset([&status](zx_status_t s) { status = s; });
    RunLoopUntil([&status]() { return status.has_value(); });
    EXPECT_EQ(status.value(), ZX_OK);

    cb(mock_admin);
  }

 private:
  void WaitForZxcrypt() {
    char data_block_path[PATH_MAX];
    // Second, wait for the data partition to be formatted.
    snprintf(data_block_path, sizeof(data_block_path), "%s/zxcrypt/unsealed/block",
             fvm_block_path_.c_str());
    ASSERT_EQ(WaitForDevice(data_block_path).status_value(), ZX_OK);
  }

  static void GetBlockSize(const fidl::ClientEnd<fuchsia_hardware_block::Block>& client_end,
                           ssize_t* out_size) {
    const fidl::WireResult result = fidl::WireCall(client_end)->GetInfo();
    ASSERT_TRUE(result.ok()) << result.status_string();
    const fit::result response = result.value();
    *out_size = response.value()->info.block_size;
  }

  static void WriteBlocks(const fidl::ClientEnd<fuchsia_hardware_block::Block>& client_end,
                          void* buffer, size_t buffer_size, size_t offset) {
    zx_status_t status = block_client::SingleWriteBytes(client_end, buffer, buffer_size, offset);
    ASSERT_EQ(status, ZX_OK) << zx_status_get_string(status);
  }

  void CreateRamdisk() {
    ASSERT_EQ(WaitForDevice(kRamCtlPath).status_value(), ZX_OK);
    ASSERT_EQ(ramdisk_create_at(devfs_root_fd().get(), kBlockSize, kBlockCount, &ramdisk_client_),
              ZX_OK);
    ASSERT_EQ(
        fs_management::FvmInitPreallocated(fidl::UnownedClientEnd<fuchsia_hardware_block::Block>(
                                               ramdisk_get_block_interface(ramdisk_client_)),
                                           kDeviceSize, kDeviceSize, kSliceSize),
        ZX_OK);
  }

  void BindFvm() {
    const std::string_view driver = "fvm.cm";
    fidl::UnownedClientEnd<fuchsia_device::Controller> channel(
        ramdisk_get_block_controller_interface(ramdisk_client_));
    const fidl::WireResult result =
        fidl::WireCall(channel)->Bind(fidl::StringView::FromExternal(driver));
    ASSERT_TRUE(result->is_ok()) << result.FormatDescription();
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok()) << zx_status_get_string(response.error_value());
  }

  void CreateFvmPartition() {
    BindFvm();
    char fvm_path[PATH_MAX];
    snprintf(fvm_path, PATH_MAX, "%s/fvm", ramdisk_get_path(ramdisk_client_));
    zx::result channel = WaitForDevice(fvm_path);
    ASSERT_EQ(channel.status_value(), ZX_OK);

    // Allocate a FVM partition with the data guid but don't actually format the partition.
    // FvmAllocatePartitionWithDevfs waits for the device to be enumerated.
    zx::result devfs = devfs_root();
    ASSERT_TRUE(devfs.is_ok()) << devfs.status_string();
    fidl::ClientEnd<fuchsia_hardware_block_volume::VolumeManager> client_end(
        std::move(channel.value()));
    zx::result device = fs_management::FvmAllocatePartitionWithDevfs(
        devfs.value(), client_end, 1, GUID_DATA_VALUE, uuid::Uuid::Generate(), kDataName, 0);
    ASSERT_TRUE(device.is_ok()) << device.status_string();

    fvm_block_path_ = fvm_path;
    fvm_block_path_.append("/");
    fvm_block_path_.append(kDataName);
    fvm_block_path_.append("-p-1/block");
  }

  zx::result<zx::channel> WaitForDevice(const std::string& path) const {
    printf("wait for device %s\n", path.c_str());
    return device_watcher::RecursiveWaitForFile(devfs_root_fd().get(), path.c_str());
  }

  ramdisk_client_t* ramdisk_client_;
  std::string fvm_block_path_;
  IsolatedDevmgr devmgr_;
};

// Tests that FactoryReset can find the correct block device and overwrite its
// superblocks, causing it to look like an unknown partition (which upon reboot
// will cause recovery to happen).
TEST_F(FactoryResetTest, CanShredVolume) {
  // Set up a normal zxcrypt superblock
  CreateZxcrypt();

  WithPartitionHasFormat([](fs_management::DiskFormat format) {
    EXPECT_EQ(format, fs_management::kDiskFormatZxcrypt);
  });

  RunReset([](const MockAdmin& mock_admin) { EXPECT_TRUE(mock_admin.suspend_called()); });

  WithPartitionHasFormat([](fs_management::DiskFormat format) {
    EXPECT_EQ(format, fs_management::kDiskFormatUnknown);
  });
}

TEST_F(FactoryResetTest, ShredsVolumeWithInvalidSuperblockIfMagicPresent) {
  // This test ensures that even if we can't unseal the zxcrypt device, we can
  // still wipe it.

  // Set up a corrupted zxcrypt superblock -- just enough to recognize the
  // magic, but not enough to successfully unseal the device.
  CreateCorruptedZxcrypt();

  WithPartitionHasFormat([](fs_management::DiskFormat format) {
    EXPECT_EQ(format, fs_management::kDiskFormatZxcrypt);
  });

  // Verify that we re-shred that superblock anyway when we run factory reset.
  RunReset([](const MockAdmin& mock_admin) { EXPECT_TRUE(mock_admin.suspend_called()); });

  WithPartitionHasFormat([](fs_management::DiskFormat format) {
    EXPECT_EQ(format, fs_management::kDiskFormatUnknown);
  });
}

TEST_F(FactoryResetTest, DoesntShredUnknownVolumeType) {
  CreateFakeBlobfs();

  WithPartitionHasFormat([](fs_management::DiskFormat format) {
    EXPECT_EQ(format, fs_management::kDiskFormatBlobfs);
  });

  RunReset([](const MockAdmin& mock_admin) { EXPECT_TRUE(mock_admin.suspend_called()); });

  // Expect factory reset to still succeed, but to not touch the block device.
  // In a world where fshost knew more about expected topology, we'd want to
  // shred this block device anyway, but that won't happen until we have a
  // clearer block device topology story.
  WithPartitionHasFormat([](fs_management::DiskFormat format) {
    EXPECT_EQ(format, fs_management::kDiskFormatBlobfs);
  });
}

TEST_F(FactoryResetTest, ShredsFxfs) {
  CreateFakeFxfs();

  WithPartitionHasFormat(
      [](fs_management::DiskFormat format) { EXPECT_EQ(format, fs_management::kDiskFormatFxfs); });

  RunReset([](const MockAdmin& mock_admin) { EXPECT_TRUE(mock_admin.suspend_called()); });

  WithPartitionHasFormat(
      [](fs_management::DiskFormat format) { EXPECT_NE(format, fs_management::kDiskFormatFxfs); });
}

TEST_F(FactoryResetTest, ShredUsingFshostMock) {
  // For now, the fshost component in the test environment does not support the ShredDataVolume
  // method, so this tests that we actually call that method.  The other tests are all testing that
  // the fallback behaviour works as intended.

  MockFshostAdmin mock_fshost;
  RunReset([](const MockAdmin& mock_admin) { EXPECT_TRUE(mock_admin.suspend_called()); },
           mock_fshost);
  EXPECT_TRUE(mock_fshost.shred_data_volume_called());
}

}  // namespace
