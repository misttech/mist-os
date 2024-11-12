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
#include "src/storage/lib/paver/kola.h"
#include "src/storage/lib/paver/luis.h"
#include "src/storage/lib/paver/sherlock.h"
#include "src/storage/lib/paver/test/test-utils.h"
#include "src/storage/lib/paver/x64.h"

namespace abr {

using device_watcher::RecursiveWaitForFile;
using driver_integration_test::IsolatedDevmgr;
using paver::KolaGptEntryAttributes;

TEST(AstroAbrTests, CreateFails) {
  IsolatedDevmgr devmgr;
  IsolatedDevmgr::Args args;
  args.disable_block_watcher = false;
  args.board_name = "sherlock";

  ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr));
  ASSERT_OK(RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform").status_value());

  zx::result devices = paver::BlockDevices::Create(devmgr.devfs_root().duplicate());
  ASSERT_OK(devices);
  std::shared_ptr<paver::Context> context;
  zx::result partitioner = paver::AstroPartitionerFactory().New(*devices, devmgr.RealmExposedDir(),
                                                                paver::Arch::kArm64, context, {});
  ASSERT_NOT_OK(partitioner);
}

TEST(SherlockAbrTests, CreateFails) {
  IsolatedDevmgr devmgr;
  IsolatedDevmgr::Args args;
  args.disable_block_watcher = false;
  args.board_name = "astro";

  ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr));
  ASSERT_OK(RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform").status_value());

  zx::result devices = paver::BlockDevices::Create(devmgr.devfs_root().duplicate());
  ASSERT_OK(devices);
  std::shared_ptr<paver::Context> context;
  zx::result partitioner = paver::SherlockPartitionerFactory().New(
      *devices, devmgr.RealmExposedDir(), paver::Arch::kArm64, context, {});
  ASSERT_NOT_OK(partitioner);
}

TEST(KolaAbrTests, CreateFails) {
  IsolatedDevmgr devmgr;
  IsolatedDevmgr::Args args;
  args.disable_block_watcher = false;
  args.board_name = "astro";

  ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr));
  ASSERT_OK(RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform").status_value());

  zx::result devices = paver::BlockDevices::Create(devmgr.devfs_root().duplicate());
  ASSERT_OK(devices);
  std::shared_ptr<paver::Context> context;
  zx::result partitioner = paver::KolaPartitionerFactory().New(*devices, devmgr.RealmExposedDir(),
                                                               paver::Arch::kArm64, context, {});
  ASSERT_NOT_OK(partitioner);
}

TEST(LuisAbrTests, CreateFails) {
  IsolatedDevmgr devmgr;
  IsolatedDevmgr::Args args;
  args.disable_block_watcher = false;
  args.board_name = "astro";

  ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr));
  ASSERT_OK(RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform").status_value());

  zx::result devices = paver::BlockDevices::Create(devmgr.devfs_root().duplicate());
  ASSERT_OK(devices);
  std::shared_ptr<paver::Context> context;
  zx::result partitioner = paver::LuisPartitionerFactory().New(*devices, devmgr.RealmExposedDir(),
                                                               paver::Arch::kArm64, context, {});
  ASSERT_NOT_OK(partitioner);
}

TEST(X64AbrTests, CreateFails) {
  IsolatedDevmgr devmgr;
  IsolatedDevmgr::Args args;
  args.disable_block_watcher = false;
  args.board_name = "x64";

  ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr));
  ASSERT_OK(RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform").status_value());

  zx::result devices = paver::BlockDevices::Create(devmgr.devfs_root().duplicate());
  ASSERT_OK(devices);
  std::shared_ptr<paver::Context> context;
  zx::result partitioner = paver::X64PartitionerFactory().New(*devices, devmgr.RealmExposedDir(),
                                                              paver::Arch::kX64, context, {});
  ASSERT_NOT_OK(partitioner);
}

class CurrentSlotUuidTest : public PaverTest {
 protected:
  static constexpr int kBlockSize = 512;
  static constexpr int kDiskBlocks = 1024;
  static constexpr uint8_t kEmptyType[GPT_GUID_LEN] = GUID_EMPTY_VALUE;
  static constexpr uint8_t kZirconType[GPT_GUID_LEN] = GPT_ZIRCON_ABR_TYPE_GUID;
  static constexpr uint8_t kTestUuid[GPT_GUID_LEN] = {0x00, 0x11, 0x22, 0x33, 0x44, 0x55,
                                                      0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
                                                      0xcc, 0xdd, 0xee, 0xff};
  void SetUp() override {
    PaverTest::SetUp();
    args_.disable_block_watcher = true;

    ASSERT_OK(IsolatedDevmgr::Create(&args_, &devmgr_));
    ASSERT_OK(RecursiveWaitForFile(devmgr_.devfs_root().get(), "sys/platform/ram-disk/ramctl")
                  .status_value());
    ASSERT_NO_FATAL_FAILURE(
        BlockDevice::Create(devmgr_.devfs_root(), kEmptyType, kDiskBlocks, kBlockSize, &disk_));
  }

  zx::result<std::unique_ptr<gpt::GptDevice>> CreateGptDevice() {
    zx::result new_connection = GetNewConnections(disk_->block_controller_interface());
    if (new_connection.is_error()) {
      return new_connection.take_error();
    }
    fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> volume(
        std::move(new_connection->device));
    zx::result remote_device = block_client::RemoteBlockDevice::Create(
        std::move(volume), std::move(new_connection->controller));
    if (remote_device.is_error()) {
      return remote_device.take_error();
    }
    return gpt::GptDevice::Create(std::move(remote_device.value()),
                                  /*blocksize=*/disk_->block_size(),
                                  /*blocks=*/disk_->block_count());
  }

  void CreatePartitionOnDisk(const char* name, const uint8_t* type_guid = kZirconType) {
    zx::result gpt_result = CreateGptDevice();
    ASSERT_OK(gpt_result);
    std::unique_ptr<gpt::GptDevice> gpt = std::move(gpt_result.value());
    ASSERT_OK(gpt->Sync());
    static uint64_t partition_offset = 2 + gpt->EntryArrayBlockCount();
    ASSERT_OK(gpt->AddPartition(name, type_guid, kTestUuid, partition_offset, 10, 0));
    partition_offset += 10;  // Adjust partition offset for the next invocation of this method.
    ASSERT_OK(gpt->Sync());

    fidl::WireResult result =
        fidl::WireCall(disk_->block_controller_interface())->Rebind(fidl::StringView("gpt.cm"));
    ASSERT_TRUE(result.ok(), "%s", result.FormatDescription().c_str());
    ASSERT_TRUE(result->is_ok(), "%s", zx_status_get_string(result->error_value()));

    ASSERT_OK(RecursiveWaitForFile(devmgr_.devfs_root().get(),
                                   "sys/platform/ram-disk/ramctl/ramdisk-0/block/part-000/block")
                  .status_value());
  }

  fidl::ClientEnd<fuchsia_io::Directory> GetSvcRoot() { return devmgr_.RealmExposedDir(); }

  IsolatedDevmgr::Args args_;
  IsolatedDevmgr devmgr_;
  std::unique_ptr<BlockDevice> disk_;
};

TEST_F(CurrentSlotUuidTest, TestZirconAIsSlotA) {
  ASSERT_NO_FATAL_FAILURE(CreatePartitionOnDisk("zircon-a"));

  zx::result devices = paver::BlockDevices::Create(devmgr_.devfs_root().duplicate());
  ASSERT_OK(devices);
  auto result = abr::PartitionUuidToConfiguration(*devices, uuid::Uuid(kTestUuid));
  ASSERT_OK(result);
  ASSERT_EQ(result.value(), fuchsia_paver::wire::Configuration::kA);
}

TEST_F(CurrentSlotUuidTest, TestZirconAWithUnderscore) {
  ASSERT_NO_FATAL_FAILURE(CreatePartitionOnDisk("zircon_a"));

  zx::result devices = paver::BlockDevices::Create(devmgr_.devfs_root().duplicate());
  ASSERT_OK(devices);
  auto result = abr::PartitionUuidToConfiguration(*devices, uuid::Uuid(kTestUuid));
  ASSERT_OK(result);
  ASSERT_EQ(result.value(), fuchsia_paver::wire::Configuration::kA);
}

TEST_F(CurrentSlotUuidTest, TestZirconAMixedCase) {
  ASSERT_NO_FATAL_FAILURE(CreatePartitionOnDisk("ZiRcOn-A"));

  zx::result devices = paver::BlockDevices::Create(devmgr_.devfs_root().duplicate());
  ASSERT_OK(devices);
  auto result = abr::PartitionUuidToConfiguration(*devices, uuid::Uuid(kTestUuid));
  ASSERT_OK(result);
  ASSERT_EQ(result.value(), fuchsia_paver::wire::Configuration::kA);
}

TEST_F(CurrentSlotUuidTest, TestZirconB) {
  ASSERT_NO_FATAL_FAILURE(CreatePartitionOnDisk("zircon_b"));

  zx::result devices = paver::BlockDevices::Create(devmgr_.devfs_root().duplicate());
  ASSERT_OK(devices);
  auto result = abr::PartitionUuidToConfiguration(*devices, uuid::Uuid(kTestUuid));
  ASSERT_OK(result);
  ASSERT_EQ(result.value(), fuchsia_paver::wire::Configuration::kB);
}

TEST_F(CurrentSlotUuidTest, TestZirconR) {
  ASSERT_NO_FATAL_FAILURE(CreatePartitionOnDisk("ZIRCON-R"));

  zx::result devices = paver::BlockDevices::Create(devmgr_.devfs_root().duplicate());
  ASSERT_OK(devices);
  auto result = abr::PartitionUuidToConfiguration(*devices, uuid::Uuid(kTestUuid));
  ASSERT_OK(result);
  ASSERT_EQ(result.value(), fuchsia_paver::wire::Configuration::kRecovery);
}

TEST_F(CurrentSlotUuidTest, TestInvalid) {
  ASSERT_NO_FATAL_FAILURE(CreatePartitionOnDisk("ZERCON-R"));

  zx::result devices = paver::BlockDevices::Create(devmgr_.devfs_root().duplicate());
  ASSERT_OK(devices);
  auto result = abr::PartitionUuidToConfiguration(*devices, uuid::Uuid(kTestUuid));
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

class KolaAbrClientTest : public CurrentSlotUuidTest {
 protected:
  static constexpr uint8_t kFvmType[GPT_GUID_LEN] = GPT_FVM_TYPE_GUID;
  static constexpr uint8_t kVbMetaType[GPT_GUID_LEN] = GPT_VBMETA_ABR_TYPE_GUID;
  static constexpr uint8_t kBootloaderType[GPT_GUID_LEN] = GPT_BOOTLOADER_ABR_TYPE_GUID;

  void SetUp() override {
    args_.board_name = "kola";
    args_.boot_arg = "_a";
    CurrentSlotUuidTest::SetUp();

    ASSERT_NO_FATAL_FAILURE(CreatePartitionOnDisk("boot_a", kZirconType));
    ASSERT_NO_FATAL_FAILURE(CreatePartitionOnDisk("boot_b", kBootloaderType));
    ASSERT_NO_FATAL_FAILURE(CreatePartitionOnDisk("super", kFvmType));
    ASSERT_NO_FATAL_FAILURE(CreatePartitionOnDisk("vbmeta_a", kVbMetaType));
    ASSERT_NO_FATAL_FAILURE(CreatePartitionOnDisk("vbmeta_b", kBootloaderType));

    zx::result devices = paver::BlockDevices::Create(devmgr_.devfs_root().duplicate());
    ASSERT_OK(devices);
    std::shared_ptr<paver::Context> context;
    zx::result partitioner = paver::KolaPartitionerFactory().New(
        *devices, devmgr_.RealmExposedDir(), paver::Arch::kArm64, context, {});
    ASSERT_OK(partitioner);
    zx::result abr_client = partitioner->CreateAbrClient();
    ASSERT_OK(abr_client);
    partitioner_ = std::move(*partitioner);
    abr_client_ = std::move(*abr_client);
  }

  zx::result<KolaGptEntryAttributes> CheckPartitionState(uint32_t index, std::string_view name,
                                                         const uint8_t* type_guid) {
    zx::result gpt_result = CreateGptDevice();
    if (gpt_result.is_error()) {
      return gpt_result.take_error();
    }
    std::unique_ptr<gpt::GptDevice> gpt = std::move(gpt_result.value());
    auto gpt_entry = gpt->GetPartition(index);
    if (gpt_entry.is_error()) {
      return gpt_entry.take_error();
    }

    char cstring_name[GPT_NAME_LEN / 2 + 1] = {0};
    ::utf16_to_cstring(cstring_name, reinterpret_cast<const uint16_t*>(gpt_entry->name),
                       sizeof(cstring_name));
    const std::string_view partition_name = cstring_name;
    EXPECT_EQ(partition_name, name);

    EXPECT_EQ(memcmp(gpt_entry->type, type_guid, GPT_GUID_LEN), 0);

    return zx::ok(KolaGptEntryAttributes{gpt_entry->flags});
  }

  void AbrClientFlush() { ASSERT_OK(abr_client_->Flush()); }

  std::unique_ptr<paver::DevicePartitioner> partitioner_;
  std::unique_ptr<abr::Client> abr_client_;
};

TEST_F(KolaAbrClientTest, KolaTest) {
  // Initial active slot A.
  ASSERT_OK(abr_client_->MarkSlotActive(kAbrSlotIndexA));
  ASSERT_OK(abr_client_->MarkSlotSuccessful(kAbrSlotIndexA));
  AbrClientFlush();

  zx::result<KolaGptEntryAttributes> attributes = CheckPartitionState(0, "boot_a", kZirconType);
  ASSERT_OK(attributes);
  EXPECT_EQ(attributes->priority(), KolaGptEntryAttributes::kKolaMaxPriority);
  EXPECT_EQ(attributes->active(), true);
  EXPECT_EQ(attributes->retry_count(), 0);
  EXPECT_EQ(attributes->boot_success(), true);
  EXPECT_EQ(attributes->unbootable(), false);
  attributes = CheckPartitionState(1, "boot_b", kBootloaderType);
  ASSERT_OK(attributes);
  EXPECT_EQ(attributes->priority(), KolaGptEntryAttributes::kKolaMaxPriority - 1);
  EXPECT_EQ(attributes->active(), false);
  EXPECT_EQ(attributes->retry_count(), 0);
  EXPECT_EQ(attributes->boot_success(), false);
  EXPECT_EQ(attributes->unbootable(), true);
  ASSERT_OK(CheckPartitionState(2, "super", kFvmType));
  ASSERT_OK(CheckPartitionState(3, "vbmeta_a", kVbMetaType));
  ASSERT_OK(CheckPartitionState(4, "vbmeta_b", kBootloaderType));

  ASSERT_OK(abr_client_->MarkSlotActive(kAbrSlotIndexB));
  AbrClientFlush();

  attributes = CheckPartitionState(0, "boot_a", kBootloaderType);
  ASSERT_OK(attributes);
  EXPECT_EQ(attributes->priority(), KolaGptEntryAttributes::kKolaMaxPriority - 1);
  EXPECT_EQ(attributes->active(), false);
  EXPECT_EQ(attributes->retry_count(), 0);
  EXPECT_EQ(attributes->boot_success(), true);
  EXPECT_EQ(attributes->unbootable(), false);
  attributes = CheckPartitionState(1, "boot_b", kZirconType);
  ASSERT_OK(attributes);
  EXPECT_EQ(attributes->priority(), KolaGptEntryAttributes::kKolaMaxPriority);
  EXPECT_EQ(attributes->active(), true);
  EXPECT_EQ(attributes->retry_count(), kAbrMaxTriesRemaining);
  EXPECT_EQ(attributes->boot_success(), false);
  EXPECT_EQ(attributes->unbootable(), false);
  ASSERT_OK(CheckPartitionState(2, "super", kFvmType));
  ASSERT_OK(CheckPartitionState(3, "vbmeta_a", kBootloaderType));
  ASSERT_OK(CheckPartitionState(4, "vbmeta_b", kVbMetaType));

  ASSERT_OK(abr_client_->MarkSlotSuccessful(kAbrSlotIndexB));
  AbrClientFlush();

  attributes = CheckPartitionState(0, "boot_a", kBootloaderType);
  ASSERT_OK(attributes);
  EXPECT_EQ(attributes->priority(), KolaGptEntryAttributes::kKolaMaxPriority - 1);
  EXPECT_EQ(attributes->active(), false);
  EXPECT_EQ(attributes->retry_count(), kAbrMaxTriesRemaining);
  EXPECT_EQ(attributes->boot_success(), false);
  EXPECT_EQ(attributes->unbootable(), false);
  attributes = CheckPartitionState(1, "boot_b", kZirconType);
  ASSERT_OK(attributes);
  EXPECT_EQ(attributes->priority(), KolaGptEntryAttributes::kKolaMaxPriority);
  EXPECT_EQ(attributes->active(), true);
  EXPECT_EQ(attributes->retry_count(), 0);
  EXPECT_EQ(attributes->boot_success(), true);
  EXPECT_EQ(attributes->unbootable(), false);
  ASSERT_OK(CheckPartitionState(2, "super", kFvmType));
  ASSERT_OK(CheckPartitionState(3, "vbmeta_a", kBootloaderType));
  ASSERT_OK(CheckPartitionState(4, "vbmeta_b", kVbMetaType));

  ASSERT_OK(abr_client_->MarkSlotActive(kAbrSlotIndexA));
  AbrClientFlush();

  attributes = CheckPartitionState(0, "boot_a", kZirconType);
  ASSERT_OK(attributes);
  EXPECT_EQ(attributes->priority(), KolaGptEntryAttributes::kKolaMaxPriority);
  EXPECT_EQ(attributes->active(), true);
  EXPECT_EQ(attributes->retry_count(), kAbrMaxTriesRemaining);
  EXPECT_EQ(attributes->boot_success(), false);
  EXPECT_EQ(attributes->unbootable(), false);
  attributes = CheckPartitionState(1, "boot_b", kBootloaderType);
  ASSERT_OK(attributes);
  EXPECT_EQ(attributes->priority(), KolaGptEntryAttributes::kKolaMaxPriority - 1);
  EXPECT_EQ(attributes->active(), false);
  EXPECT_EQ(attributes->retry_count(), 0);
  EXPECT_EQ(attributes->boot_success(), true);
  EXPECT_EQ(attributes->unbootable(), false);
  ASSERT_OK(CheckPartitionState(2, "super", kFvmType));
  ASSERT_OK(CheckPartitionState(3, "vbmeta_a", kVbMetaType));
  ASSERT_OK(CheckPartitionState(4, "vbmeta_b", kBootloaderType));

  ASSERT_OK(abr_client_->MarkSlotSuccessful(kAbrSlotIndexA));
  AbrClientFlush();

  attributes = CheckPartitionState(0, "boot_a", kZirconType);
  ASSERT_OK(attributes);
  EXPECT_EQ(attributes->priority(), KolaGptEntryAttributes::kKolaMaxPriority);
  EXPECT_EQ(attributes->active(), true);
  EXPECT_EQ(attributes->retry_count(), 0);
  EXPECT_EQ(attributes->boot_success(), true);
  EXPECT_EQ(attributes->unbootable(), false);
  attributes = CheckPartitionState(1, "boot_b", kBootloaderType);
  ASSERT_OK(attributes);
  EXPECT_EQ(attributes->priority(), KolaGptEntryAttributes::kKolaMaxPriority - 1);
  EXPECT_EQ(attributes->active(), false);
  EXPECT_EQ(attributes->retry_count(), kAbrMaxTriesRemaining);
  EXPECT_EQ(attributes->boot_success(), false);
  EXPECT_EQ(attributes->unbootable(), false);
  ASSERT_OK(CheckPartitionState(2, "super", kFvmType));
  ASSERT_OK(CheckPartitionState(3, "vbmeta_a", kVbMetaType));
  ASSERT_OK(CheckPartitionState(4, "vbmeta_b", kBootloaderType));
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

class OneShotFlagsTest : public PaverTest {
 public:
  void SetUp() override {
    PaverTest::SetUp();
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

}  // namespace abr
