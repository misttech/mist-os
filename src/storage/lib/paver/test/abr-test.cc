// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/abr/abr.h"

#include <endian.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/cksum.h>
#include <lib/driver-integration-test/fixture.h>
#include <lib/fdio/directory.h>
#include <zircon/hw/gpt.h>

#include <algorithm>
#include <iostream>

#include <mock-boot-arguments/server.h>
#include <zxtest/zxtest.h>

#include "src/lib/uuid/uuid.h"
#include "src/storage/lib/block_client/cpp/remote_block_device.h"
#include "src/storage/lib/paver/abr-client.h"
#include "src/storage/lib/paver/astro.h"
#include "src/storage/lib/paver/luis.h"
#include "src/storage/lib/paver/sherlock.h"
#include "src/storage/lib/paver/test/test-utils.h"
#include "src/storage/lib/paver/utils.h"
#include "src/storage/lib/paver/violet.h"
#include "src/storage/lib/paver/x64.h"

namespace {

using device_watcher::RecursiveWaitForFile;
using driver_integration_test::IsolatedDevmgr;
using paver::BlockWatcherPauser;

TEST(AstroAbrTests, CreateFails) {
  IsolatedDevmgr devmgr;
  IsolatedDevmgr::Args args;
  args.disable_block_watcher = false;
  args.board_name = "sherlock";

  ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr));
  ASSERT_OK(RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform").status_value());

  fidl::ClientEnd<fuchsia_io::Directory> svc_root;
  ASSERT_NOT_OK(
      paver::AstroAbrClientFactory().New(devmgr.devfs_root().duplicate(), svc_root, nullptr));
}

TEST(SherlockAbrTests, CreateFails) {
  IsolatedDevmgr devmgr;
  IsolatedDevmgr::Args args;
  args.disable_block_watcher = false;
  args.board_name = "astro";

  ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr));
  ASSERT_OK(RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform").status_value());

  ASSERT_NOT_OK(paver::SherlockAbrClientFactory().Create(devmgr.devfs_root().duplicate(),
                                                         devmgr.fshost_svc_dir(), nullptr));
}

TEST(LuisAbrTests, CreateFails) {
  IsolatedDevmgr devmgr;
  IsolatedDevmgr::Args args;
  args.disable_block_watcher = false;
  args.board_name = "astro";

  ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr));
  ASSERT_OK(RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform").status_value());

  ASSERT_NOT_OK(paver::LuisAbrClientFactory().Create(devmgr.devfs_root().duplicate(),
                                                     devmgr.fshost_svc_dir(), nullptr));
}

TEST(VioletAbrTests, CreateFails) {
  IsolatedDevmgr devmgr;
  IsolatedDevmgr::Args args;
  args.disable_block_watcher = false;
  args.board_name = "astro";

  ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr));
  ASSERT_OK(RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform").status_value());

  ASSERT_NOT_OK(paver::VioletAbrClientFactory().Create(devmgr.devfs_root().duplicate(),
                                                       devmgr.fshost_svc_dir(), nullptr));
}

TEST(X64AbrTests, CreateFails) {
  IsolatedDevmgr devmgr;
  IsolatedDevmgr::Args args;
  args.disable_block_watcher = false;
  args.board_name = "x64";

  ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr));
  ASSERT_OK(RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform").status_value());

  ASSERT_NOT_OK(paver::X64AbrClientFactory().Create(devmgr.devfs_root().duplicate(),
                                                    devmgr.fshost_svc_dir(), nullptr));
}

class CurrentSlotUuidTest : public zxtest::Test {
 protected:
  static constexpr int kBlockSize = 512;
  static constexpr int kDiskBlocks = 1024;
  static constexpr uint8_t kEmptyType[GPT_GUID_LEN] = GUID_EMPTY_VALUE;
  static constexpr uint8_t kZirconType[GPT_GUID_LEN] = GPT_ZIRCON_ABR_TYPE_GUID;
  static constexpr uint8_t kTestUuid[GPT_GUID_LEN] = {0x00, 0x11, 0x22, 0x33, 0x44, 0x55,
                                                      0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
                                                      0xcc, 0xdd, 0xee, 0xff};
  CurrentSlotUuidTest() {
    IsolatedDevmgr::Args args;
    args.disable_block_watcher = true;

    ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr_));
    ASSERT_OK(RecursiveWaitForFile(devmgr_.devfs_root().get(), "sys/platform/ram-disk/ramctl")
                  .status_value());
    ASSERT_NO_FATAL_FAILURE(
        BlockDevice::Create(devmgr_.devfs_root(), kEmptyType, kDiskBlocks, kBlockSize, &disk_));
  }

  void CreateDiskWithPartition(const char* partition) {
    zx::result new_connection = GetNewConnections(disk_->block_controller_interface());
    ASSERT_OK(new_connection);
    fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> volume(
        std::move(new_connection->device));
    zx::result remote_device = block_client::RemoteBlockDevice::Create(
        std::move(volume), std::move(new_connection->controller));
    ASSERT_OK(remote_device);
    zx::result gpt_result = gpt::GptDevice::Create(std::move(remote_device.value()),
                                                   /*blocksize=*/disk_->block_size(),
                                                   /*blocks=*/disk_->block_count());
    ASSERT_OK(gpt_result);
    gpt_ = std::move(gpt_result.value());
    ASSERT_OK(gpt_->Sync());
    ASSERT_OK(gpt_->AddPartition(partition, kZirconType, kTestUuid,
                                 2 + gpt_->EntryArrayBlockCount(), 10, 0));
    ASSERT_OK(gpt_->Sync());

    fidl::WireResult result =
        fidl::WireCall(disk_->block_controller_interface())->Rebind(fidl::StringView("gpt.cm"));
    ASSERT_TRUE(result.ok(), "%s", result.FormatDescription().c_str());
    ASSERT_TRUE(result->is_ok(), "%s", zx_status_get_string(result->error_value()));

    ASSERT_OK(RecursiveWaitForFile(devmgr_.devfs_root().get(),
                                   "sys/platform/ram-disk/ramctl/ramdisk-0/block/part-000/block")
                  .status_value());
  }

  fidl::ClientEnd<fuchsia_io::Directory> GetSvcRoot() { return devmgr_.fshost_svc_dir(); }

  IsolatedDevmgr devmgr_;
  std::unique_ptr<BlockDevice> disk_;
  std::unique_ptr<gpt::GptDevice> gpt_;
};

TEST_F(CurrentSlotUuidTest, TestZirconAIsSlotA) {
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithPartition("zircon-a"));

  auto result = abr::PartitionUuidToConfiguration(devmgr_.devfs_root(), uuid::Uuid(kTestUuid));
  ASSERT_OK(result);
  ASSERT_EQ(result.value(), fuchsia_paver::wire::Configuration::kA);
}

TEST_F(CurrentSlotUuidTest, TestZirconAWithUnderscore) {
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithPartition("zircon_a"));

  auto result = abr::PartitionUuidToConfiguration(devmgr_.devfs_root(), uuid::Uuid(kTestUuid));
  ASSERT_OK(result);
  ASSERT_EQ(result.value(), fuchsia_paver::wire::Configuration::kA);
}

TEST_F(CurrentSlotUuidTest, TestZirconAMixedCase) {
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithPartition("ZiRcOn-A"));

  auto result = abr::PartitionUuidToConfiguration(devmgr_.devfs_root(), uuid::Uuid(kTestUuid));
  ASSERT_OK(result);
  ASSERT_EQ(result.value(), fuchsia_paver::wire::Configuration::kA);
}

TEST_F(CurrentSlotUuidTest, TestZirconB) {
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithPartition("zircon_b"));

  auto result = abr::PartitionUuidToConfiguration(devmgr_.devfs_root(), uuid::Uuid(kTestUuid));
  ASSERT_OK(result);
  ASSERT_EQ(result.value(), fuchsia_paver::wire::Configuration::kB);
}

TEST_F(CurrentSlotUuidTest, TestZirconR) {
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithPartition("ZIRCON-R"));

  auto result = abr::PartitionUuidToConfiguration(devmgr_.devfs_root(), uuid::Uuid(kTestUuid));
  ASSERT_OK(result);
  ASSERT_EQ(result.value(), fuchsia_paver::wire::Configuration::kRecovery);
}

TEST_F(CurrentSlotUuidTest, TestInvalid) {
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithPartition("ZERCON-R"));

  auto result = abr::PartitionUuidToConfiguration(devmgr_.devfs_root(), uuid::Uuid(kTestUuid));
  ASSERT_TRUE(result.is_error());
  ASSERT_EQ(result.error_value(), ZX_ERR_NOT_SUPPORTED);
}

TEST(CurrentSlotTest, TestA) {
  auto result = abr::CurrentSlotToConfiguration("_a");
  ASSERT_OK(result);
  ASSERT_EQ(result.value(), fuchsia_paver::wire::Configuration::kA);
}

TEST(CurrentSlotTest, TestB) {
  auto result = abr::CurrentSlotToConfiguration("_b");
  ASSERT_OK(result);
  ASSERT_EQ(result.value(), fuchsia_paver::wire::Configuration::kB);
}

TEST(CurrentSlotTest, TestR) {
  auto result = abr::CurrentSlotToConfiguration("_r");
  ASSERT_OK(result);
  ASSERT_EQ(result.value(), fuchsia_paver::wire::Configuration::kRecovery);
}

TEST(CurrentSlotTest, TestInvalid) {
  auto result = abr::CurrentSlotToConfiguration("_x");
  ASSERT_TRUE(result.is_error());
  ASSERT_EQ(result.error_value(), ZX_ERR_NOT_SUPPORTED);
}

class FakePartitionClient final : public paver::PartitionClient {
 public:
  FakePartitionClient(size_t block_size, size_t partition_size)
      : block_size_(block_size), partition_size_(partition_size) {}

  zx::result<size_t> GetBlockSize() final {
    if (result_ == ZX_OK) {
      return zx::ok(block_size_);
    }
    return zx::error(result_);
  }
  zx::result<size_t> GetPartitionSize() final {
    if (result_ == ZX_OK) {
      return zx::ok(partition_size_);
    }
    return zx::error(result_);
  }
  zx::result<> Read(const zx::vmo& vmo, size_t size) final {
    if (size > partition_size_) {
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }
    return zx::make_result(result_);
  }
  zx::result<> Write(const zx::vmo& vmo, size_t vmo_size) final {
    if (vmo_size > partition_size_) {
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }
    return zx::make_result(result_);
  }
  zx::result<> Trim() final { return zx::make_result(result_); }
  zx::result<> Flush() final { return zx::make_result(result_); }

  void set_result(zx_status_t result) { result_ = result; }

 private:
  size_t block_size_;
  size_t partition_size_;

  zx_status_t result_ = ZX_OK;
};

class OneShotFlagsTest : public zxtest::Test {
 public:
  void SetUp() override {
    auto partition_client = std::make_unique<FakePartitionClient>(10, 100);
    auto abr_partition_client = abr::AbrPartitionClient::Create(std::move(partition_client));
    ASSERT_OK(abr_partition_client);
    abr_client_ = std::move(abr_partition_client.value());

    // Clear flags
    ASSERT_OK(abr_client_->GetAndClearOneShotFlags());
  }

  std::unique_ptr<abr::Client> abr_client_;
};

TEST_F(OneShotFlagsTest, ClearFlags) {
  // Set some flags to see that they are cleared
  ASSERT_OK(abr_client_->SetOneShotRecovery());
  ASSERT_OK(abr_client_->SetOneShotBootloader());

  // First get flags would return flags
  auto abr_flags_res = abr_client_->GetAndClearOneShotFlags();
  ASSERT_OK(abr_flags_res);
  EXPECT_NE(abr_flags_res.value(), kAbrDataOneShotFlagNone);

  // Second get flags should be cleared
  abr_flags_res = abr_client_->GetAndClearOneShotFlags();
  ASSERT_OK(abr_flags_res);
  EXPECT_EQ(abr_flags_res.value(), kAbrDataOneShotFlagNone);
}

TEST_F(OneShotFlagsTest, SetOneShotRecovery) {
  ASSERT_OK(abr_client_->SetOneShotRecovery());

  // Check if flag is set
  auto abr_flags_res = abr_client_->GetAndClearOneShotFlags();
  ASSERT_OK(abr_flags_res);
  EXPECT_TRUE(AbrIsOneShotRecoveryBootSet(abr_flags_res.value()));
  EXPECT_FALSE(AbrIsOneShotBootloaderBootSet(abr_flags_res.value()));
}

TEST_F(OneShotFlagsTest, SetOneShotBootloader) {
  ASSERT_OK(abr_client_->SetOneShotBootloader());

  // Check if flag is set
  auto abr_flags_res = abr_client_->GetAndClearOneShotFlags();
  ASSERT_OK(abr_flags_res);
  EXPECT_TRUE(AbrIsOneShotBootloaderBootSet(abr_flags_res.value()));
  EXPECT_FALSE(AbrIsOneShotRecoveryBootSet(abr_flags_res.value()));
}

TEST_F(OneShotFlagsTest, Set2Flags) {
  ASSERT_OK(abr_client_->SetOneShotBootloader());
  ASSERT_OK(abr_client_->SetOneShotRecovery());

  // Check if flag is set
  auto abr_flags_res = abr_client_->GetAndClearOneShotFlags();
  ASSERT_OK(abr_flags_res);
  EXPECT_TRUE(AbrIsOneShotBootloaderBootSet(abr_flags_res.value()));
  EXPECT_TRUE(AbrIsOneShotRecoveryBootSet(abr_flags_res.value()));
}

}  // namespace
