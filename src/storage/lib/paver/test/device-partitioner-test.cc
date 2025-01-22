// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/device-partitioner.h"

#include <dirent.h>
#include <fcntl.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.fshost/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <fidl/fuchsia.hardware.power.statecontrol/cpp/wire.h>
#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <fidl/fuchsia.scheduler/cpp/wire.h>
#include <fidl/fuchsia.system.state/cpp/common_types.h>
#include <fidl/fuchsia.system.state/cpp/fidl.h>
#include <fidl/fuchsia.system.state/cpp/markers.h>
#include <fidl/fuchsia.system.state/cpp/wire.h>
#include <fidl/fuchsia.tracing.provider/cpp/wire.h>
#include <lib/abr/util.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/driver-integration-test/fixture.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/fzl/owned-vmo-mapper.h>
#include <lib/sys/cpp/testing/component_context_provider.h>
#include <lib/sys/cpp/testing/service_directory_provider.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <unistd.h>
#include <zircon/errors.h>
#include <zircon/hw/gpt.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <array>
#include <format>
#include <memory>
#include <span>
#include <string_view>
#include <utility>

#include <fbl/unique_fd.h>
#include <gpt/gpt.h>
#include <soc/aml-common/aml-guid.h>
#include <zxtest/zxtest.h>

#include "src/lib/files/directory.h"
#include "src/lib/uuid/uuid.h"
#include "src/storage/lib/block_client/cpp/fake_block_device.h"
#include "src/storage/lib/block_client/cpp/remote_block_device.h"
#include "src/storage/lib/paver/android.h"
#include "src/storage/lib/paver/luis.h"
#include "src/storage/lib/paver/moonflower.h"
#include "src/storage/lib/paver/nelson.h"
#include "src/storage/lib/paver/sherlock.h"
#include "src/storage/lib/paver/system_shutdown_state.h"
#include "src/storage/lib/paver/test/test-utils.h"
#include "src/storage/lib/paver/utils.h"
#include "src/storage/lib/paver/x64.h"

namespace paver {
extern zx_duration_t g_wipe_timeout;
}

namespace {

constexpr fidl::UnownedClientEnd<fuchsia_io::Directory> kInvalidSvcRoot =
    fidl::UnownedClientEnd<fuchsia_io::Directory>(ZX_HANDLE_INVALID);

constexpr uint64_t kMebibyte{UINT64_C(1024) * 1024};
constexpr uint64_t kGibibyte{kMebibyte * 1024};

using device_watcher::RecursiveWaitForFile;
using driver_integration_test::IsolatedDevmgr;
using fuchsia_system_state::SystemPowerState;
using fuchsia_system_state::SystemStateTransition;
using paver::PartitionSpec;
using uuid::Uuid;

namespace fio = fuchsia_io;

// New Type GUID's
constexpr uint8_t kDurableBootType[GPT_GUID_LEN] = GPT_DURABLE_BOOT_TYPE_GUID;
constexpr uint8_t kVbMetaType[GPT_GUID_LEN] = GPT_VBMETA_ABR_TYPE_GUID;
constexpr uint8_t kZirconType[GPT_GUID_LEN] = GPT_ZIRCON_ABR_TYPE_GUID;
constexpr uint8_t kNewFvmType[GPT_GUID_LEN] = GPT_FVM_TYPE_GUID;

// Legacy Type GUID's
constexpr uint8_t kBootloaderType[GPT_GUID_LEN] = GUID_BOOTLOADER_VALUE;
constexpr uint8_t kEfiType[GPT_GUID_LEN] = GUID_EFI_VALUE;
constexpr uint8_t kZirconAType[GPT_GUID_LEN] = GUID_ZIRCON_A_VALUE;
constexpr uint8_t kZirconBType[GPT_GUID_LEN] = GUID_ZIRCON_B_VALUE;
constexpr uint8_t kZirconRType[GPT_GUID_LEN] = GUID_ZIRCON_R_VALUE;
constexpr uint8_t kVbMetaAType[GPT_GUID_LEN] = GUID_VBMETA_A_VALUE;
constexpr uint8_t kVbMetaBType[GPT_GUID_LEN] = GUID_VBMETA_B_VALUE;
constexpr uint8_t kVbMetaRType[GPT_GUID_LEN] = GUID_VBMETA_R_VALUE;
constexpr uint8_t kFvmType[GPT_GUID_LEN] = GUID_FVM_VALUE;
constexpr uint8_t kEmptyType[GPT_GUID_LEN] = GUID_EMPTY_VALUE;
constexpr uint8_t kAbrMetaType[GPT_GUID_LEN] = GUID_ABR_META_VALUE;
constexpr uint8_t kStateLinuxGuid[GPT_GUID_LEN] = GUID_LINUX_FILESYSTEM_DATA_VALUE;

constexpr uint8_t kBoot0Type[GPT_GUID_LEN] = GUID_EMMC_BOOT1_VALUE;
constexpr uint8_t kBoot1Type[GPT_GUID_LEN] = GUID_EMMC_BOOT2_VALUE;

constexpr uint8_t kDummyType[GPT_GUID_LEN] = {0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47,
                                              0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d, 0xe4};

struct PartitionDescription {
  std::string name;
  uuid::Uuid type;
  uint64_t start;
  uint64_t length;
};

zx::result<fidl::ClientEnd<fuchsia_device::Controller>> ControllerFromBlock(BlockDevice* gpt) {
  if (!gpt) {
    return zx::ok(fidl::ClientEnd<fuchsia_device::Controller>());
  }
  auto [controller, controller_server] = fidl::Endpoints<fuchsia_device::Controller>::Create();
  if (zx_status_t status = fidl::WireCall(gpt->block_controller_interface())
                               ->ConnectToController(std::move(controller_server))
                               .status();
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(controller));
}

TEST(PartitionName, Bootloader) {
  EXPECT_STREQ(PartitionName(paver::Partition::kBootloaderA, paver::PartitionScheme::kNew),
               GPT_BOOTLOADER_A_NAME);
  EXPECT_STREQ(PartitionName(paver::Partition::kBootloaderB, paver::PartitionScheme::kNew),
               GPT_BOOTLOADER_B_NAME);
  EXPECT_STREQ(PartitionName(paver::Partition::kBootloaderR, paver::PartitionScheme::kNew),
               GPT_BOOTLOADER_R_NAME);
  EXPECT_STREQ(PartitionName(paver::Partition::kBootloaderA, paver::PartitionScheme::kLegacy),
               GUID_EFI_NAME);
  EXPECT_STREQ(PartitionName(paver::Partition::kBootloaderB, paver::PartitionScheme::kLegacy),
               GUID_EFI_NAME);
  EXPECT_STREQ(PartitionName(paver::Partition::kBootloaderR, paver::PartitionScheme::kLegacy),
               GUID_EFI_NAME);
}

TEST(PartitionName, AbrMetadata) {
  EXPECT_STREQ(PartitionName(paver::Partition::kAbrMeta, paver::PartitionScheme::kNew),
               GPT_DURABLE_BOOT_NAME);
  EXPECT_STREQ(PartitionName(paver::Partition::kAbrMeta, paver::PartitionScheme::kLegacy),
               GUID_ABR_META_NAME);
}

TEST(PartitionSpec, ToStringDefaultContentType) {
  // This is a bit of a change-detector test since we don't actually care about
  // the string value, but it's the cleanest way to check that the string is
  // 1) non-empty and 2) doesn't contain a type suffix.
  EXPECT_EQ(PartitionSpec(paver::Partition::kZirconA).ToString(), "Zircon A");
  EXPECT_EQ(PartitionSpec(paver::Partition::kVbMetaB).ToString(), "VBMeta B");
}

TEST(PartitionSpec, ToStringWithContentType) {
  EXPECT_EQ(PartitionSpec(paver::Partition::kZirconA, "foo").ToString(), "Zircon A (foo)");
  EXPECT_EQ(PartitionSpec(paver::Partition::kVbMetaB, "a b c").ToString(), "VBMeta B (a b c)");
}

class GptDevicePartitionerTests : public PaverTest {
 protected:
  explicit GptDevicePartitionerTests(fbl::String board_name = fbl::String(),
                                     uint32_t block_size = 512, std::string slot_suffix = "")
      : board_name_(std::move(board_name)),
        slot_suffix_(std::move(slot_suffix)),
        block_size_(block_size) {}

  void SetUp() override {
    PaverTest::SetUp();
    num_devices_ = 0;
    paver::g_wipe_timeout = 0;
    IsolatedDevmgr::Args args = BaseDevmgrArgs();
    args.board_name = board_name_;
    if (!slot_suffix_.empty()) {
      args.fake_boot_args = std::make_unique<FakeBootArgs>(slot_suffix_);
    }
    ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr_));

    ASSERT_OK(RecursiveWaitForFile(devmgr_.devfs_root().get(), "sys/platform/ram-disk/ramctl")
                  .status_value());
    ASSERT_OK(RecursiveWaitForFile(devmgr_.devfs_root().get(), "sys/platform").status_value());
  }

  virtual IsolatedDevmgr::Args BaseDevmgrArgs() {
    IsolatedDevmgr::Args args;
    args.disable_block_watcher = false;
    return args;
  }

  fidl::ClientEnd<fuchsia_io::Directory> RealmExposedDir() { return devmgr_.RealmExposedDir(); }

  // Create a disk with the default size for a BlockDevice.
  void CreateDisk(std::unique_ptr<BlockDevice>* disk) {
    ASSERT_NO_FATAL_FAILURE(BlockDevice::Create(devmgr_.devfs_root(), kEmptyType, disk));
  }

  // Create a disk with the given size in bytes.
  void CreateDisk(uint64_t bytes, std::unique_ptr<BlockDevice>* disk) {
    ASSERT_TRUE(bytes % block_size_ == 0);
    uint64_t num_blocks = bytes / block_size_;

    ASSERT_NO_FATAL_FAILURE(
        BlockDevice::Create(devmgr_.devfs_root(), kEmptyType, num_blocks, block_size_, disk));
  }

  // Create a disk with the given size in bytes and the given type.
  void CreateDisk(uint64_t bytes, const uint8_t* type, std::unique_ptr<BlockDevice>* disk) {
    ASSERT_TRUE(bytes % block_size_ == 0);
    uint64_t num_blocks = bytes / block_size_;

    ASSERT_NO_FATAL_FAILURE(
        BlockDevice::Create(devmgr_.devfs_root(), type, num_blocks, block_size_, disk));
  }

  // Create a disk with some initial contents.
  void CreateDiskWithContents(std::unique_ptr<BlockDevice>* disk, zx::vmo contents,
                              const uint8_t* type_guid = kEmptyType) {
    ASSERT_NO_FATAL_FAILURE(BlockDevice::CreateFromVmo(devmgr_.devfs_root(), type_guid,
                                                       std::move(contents), block_size_, disk));
  }

  // Creates a GPT-formatted device with `init_partitions`.
  void CreateDiskWithGpt(std::unique_ptr<BlockDevice>* disk, size_t size = 0,
                         const std::vector<PartitionDescription>& init_partitions = {}) {
    uint64_t num_blocks = std::max(size / block_size_, kGptBlockCount);
    auto dev = std::make_unique<block_client::FakeBlockDevice>(num_blocks, block_size_);
    zx::result contents = dev->VmoChildReference();
    ASSERT_OK(contents);
    zx::result gpt_result = gpt::GptDevice::Create(std::move(dev), block_size_, num_blocks);
    ASSERT_OK(gpt_result);
    std::unique_ptr<gpt::GptDevice> gpt = std::move(*gpt_result);
    ASSERT_OK(gpt->Sync());

    for (auto& part : init_partitions) {
      ASSERT_OK(gpt->AddPartition(part.name.c_str(), part.type.bytes(), Uuid::Generate().bytes(),
                                  part.start, part.length, 0),
                "%s", part.name.c_str());
    }
    ASSERT_OK(gpt->Sync());

    ASSERT_NO_FATAL_FAILURE(CreateDiskWithContents(disk, std::move(*contents)));
  }

  // Creates a GPT-formatted device with EFI partition
  void CreateDiskWithUefiGpt(std::unique_ptr<BlockDevice>* disk, size_t size = 0) {
    return CreateDiskWithGpt(
        disk, size,
        {
            PartitionDescription{GUID_EFI_NAME, Uuid(kEfiType), 0x8023, 0x8000},
        });
  }

  void ReadBlocks(const BlockDevice* blk_dev, size_t offset_in_blocks, size_t size_in_blocks,
                  uint8_t* out) const {
    zx::result block_client = paver::BlockPartitionClient::Create(
        std::make_unique<paver::DevfsVolumeConnector>(blk_dev->ConnectToController()));

    zx::vmo vmo;
    const size_t vmo_size = size_in_blocks * block_size_;
    ASSERT_OK(zx::vmo::create(vmo_size, 0, &vmo));
    ASSERT_OK(block_client->Read(vmo, vmo_size, offset_in_blocks, 0));
    ASSERT_OK(vmo.read(out, 0, vmo_size));
  }

  void WriteBlocks(const BlockDevice* blk_dev, size_t offset_in_blocks, size_t size_in_blocks,
                   uint8_t* buffer) const {
    zx::result block_client = paver::BlockPartitionClient::Create(
        std::make_unique<paver::DevfsVolumeConnector>(blk_dev->ConnectToController()));

    zx::vmo vmo;
    const size_t vmo_size = size_in_blocks * block_size_;
    ASSERT_OK(zx::vmo::create(vmo_size, 0, &vmo));
    ASSERT_OK(vmo.write(buffer, 0, vmo_size));
    ASSERT_OK(block_client->Write(vmo, vmo_size, offset_in_blocks, 0));
  }

  void ValidateBlockContent(const BlockDevice* blk_dev, size_t offset_in_blocks,
                            size_t size_in_blocks, uint8_t value) {
    std::vector<uint8_t> buffer(size_in_blocks * block_size_);
    ASSERT_NO_FATAL_FAILURE(ReadBlocks(blk_dev, offset_in_blocks, size_in_blocks, buffer.data()));
    for (size_t i = 0; i < buffer.size(); i++) {
      ASSERT_EQ(value, buffer[i], "at index: %zu", i);
    }
  }

  // Ensure that the partitions published to fshost match the expected list.
  void EnsurePartitionsMatch(std::span<const PartitionDescription> expected) {
    std::vector<fidl::ClientEnd<fuchsia_hardware_block_partition::Partition>> devices;
    ASSERT_NO_FATAL_FAILURE(FindAllBlockDevices(&devices));
    std::vector<PartitionDescription> actual;
    for (auto& device : devices) {
      if (std::optional<PartitionDescription> desc = GetPartitionDescription(device); desc) {
        actual.push_back(*desc);
      }
    }

    for (const auto& part : expected) {
      auto match = std::find_if(actual.cbegin(), actual.cend(),
                                [&part](const PartitionDescription& actual_part) {
                                  return actual_part.name == part.name;
                                });
      ASSERT_TRUE(match != actual.end(), "Partition %s not found", part.name.c_str());
      EXPECT_EQ(part.type, match->type, "Partition %s wrong guid", part.name.c_str());
      EXPECT_EQ(part.start, match->start, "Partition %s wrong start", part.name.c_str());
      EXPECT_EQ(part.length, match->length, "Partition %s wrong length", part.name.c_str());
    }
  }

  static std::optional<PartitionDescription> GetPartitionDescription(
      fidl::UnownedClientEnd<fuchsia_hardware_block_partition::Partition> client) {
    fidl::WireResult metadata = fidl::WireCall(client)->GetMetadata();
    if (!metadata.ok() || !metadata->value()->has_name() || !metadata->value()->has_type_guid()) {
      // Ignore non-Partition devices.
      return std::nullopt;
    }

    const auto& value = metadata->value();
    return PartitionDescription{
        .name = std::string(value->name().cbegin(), value->name().cend()),
        .type = uuid::Uuid(value->type_guid().value.data()),
        .start = value->start_block_offset(),
        .length = value->num_blocks(),
    };
  }

  // The legacy devfs-based stack publishes drivers asynchronously, which means that some tests need
  // to wait for block devices to be published to work properly.
  void WaitForBlockDevices(size_t num_expected) {
    if (BaseDevmgrArgs().enable_storage_host) {
      // Storage-host publishes synchronously so we can stub this out.
      return;
    }
    ASSERT_GT(num_expected, 0);
    num_devices_ += num_expected;
    std::string path = std::format("class/block/{:03d}", num_devices_ - 1);
    ASSERT_OK(RecursiveWaitForFile(devmgr_.devfs_root().get(), path.c_str()).status_value());
  }

  void FindAllBlockDevices(
      std::vector<fidl::ClientEnd<fuchsia_hardware_block_partition::Partition>>* out) {
    if (BaseDevmgrArgs().enable_storage_host) {
      ASSERT_NO_FATAL_FAILURE(FindAllBlockDevicesStorageHost(out));
      return;
    }
    std::vector<std::string> block_devices;
    ASSERT_TRUE(
        files::ReadDirContentsAt(devmgr_.devfs_root().get(), "class/block", &block_devices));
    for (const auto& device : block_devices) {
      std::string path = std::format("class/block/{}/device_controller", device);
      fdio_cpp::UnownedFdioCaller caller(devmgr_.devfs_root());
      zx::result controller =
          component::ConnectAt<fuchsia_device::Controller>(caller.directory(), path);
      ASSERT_OK(controller);
      zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_block_partition::Partition>();
      ASSERT_OK(endpoints);
      auto [client, server] = std::move(*endpoints);
      fidl::OneWayError response =
          fidl::WireCall(*controller)->ConnectToDeviceFidl(server.TakeChannel());
      ASSERT_TRUE(response.ok());
      out->push_back(std::move(client));
    }
  }

  void FindAllBlockDevicesStorageHost(
      std::vector<fidl::ClientEnd<fuchsia_hardware_block_partition::Partition>>* out) {
    fidl::ClientEnd svc_root = RealmExposedDir();
    fbl::unique_fd fd;
    ASSERT_OK(fdio_fd_create(svc_root.TakeHandle().release(), fd.reset_and_get_address()));
    std::vector<std::string> entries;
    ASSERT_TRUE(files::ReadDirContentsAt(fd.get(), "fuchsia.storage.partitions.PartitionService",
                                         &entries));
    for (const auto& entry : entries) {
      std::string path =
          std::format("fuchsia.storage.partitions.PartitionService/{}/volume", entry);
      fdio_cpp::UnownedFdioCaller caller(fd.get());
      zx::result partition = component::ConnectAt<fuchsia_hardware_block_partition::Partition>(
          caller.directory(), path);
      ASSERT_OK(partition);
      out->push_back(std::move(*partition));
    }
  }

  IsolatedDevmgr devmgr_;
  fbl::String board_name_;
  std::string slot_suffix_;
  size_t num_devices_ = 0;
  const uint32_t block_size_;
};

class FakeSystemStateTransition final : public fidl::WireServer<SystemStateTransition> {
 public:
  void GetTerminationSystemState(GetTerminationSystemStateCompleter::Sync& completer) override {
    completer.Reply(state_);
  }
  void GetMexecZbis(GetMexecZbisCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void SetTerminationSystemState(SystemPowerState state) { state_ = state; }

 private:
  fidl::ServerBindingGroup<SystemStateTransition> bindings_;
  SystemPowerState state_ = SystemPowerState::kFullyOn;
};

class FakeSvc {
 public:
  explicit FakeSvc(async_dispatcher_t* dispatcher) {
    zx::result server_end = fidl::CreateEndpoints(&root_);
    ASSERT_OK(server_end);
    async::PostTask(dispatcher, [dispatcher,
                                 &fake_system_shutdown_state = fake_system_shutdown_state_,
                                 server_end = std::move(server_end.value())]() mutable {
      component::OutgoingDirectory outgoing{dispatcher};
      ASSERT_OK(outgoing.AddUnmanagedProtocol<SystemStateTransition>(
          [&fake_system_shutdown_state, dispatcher](fidl::ServerEnd<SystemStateTransition> server) {
            fidl::BindServer(dispatcher, std::move(server), &fake_system_shutdown_state);
          }));

      ASSERT_OK(outgoing.Serve(std::move(server_end)));

      // Stash the outgoing directory on the dispatcher so that the dtor runs on the dispatcher
      // thread.
      async::PostDelayedTask(
          dispatcher, [outgoing = std::move(outgoing)]() {}, zx::duration::infinite());
    });
  }

  FakeSystemStateTransition& fake_system_shutdown_state() { return fake_system_shutdown_state_; }

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> svc() {
    return component::ConnectAt<fuchsia_io::Directory>(
        root_, component::OutgoingDirectory::kServiceDirectory);
  }

 private:
  FakeSystemStateTransition fake_system_shutdown_state_;
  fidl::ClientEnd<fuchsia_io::Directory> root_;
};

class EfiDevicePartitionerTests : public GptDevicePartitionerTests {
 protected:
  EfiDevicePartitionerTests() : GptDevicePartitionerTests(fbl::String()) {
    EXPECT_OK(loop_.StartThread("efi-devicepartitioner-tests-loop"));
  }

  ~EfiDevicePartitionerTests() { loop_.Shutdown(); }

  // Create a DevicePartition for a device.
  zx::result<std::unique_ptr<paver::DevicePartitioner>> CreatePartitioner(
      BlockDevice* gpt = nullptr) {
    return CreatePartitioner(gpt, RealmExposedDir());
  }

  virtual zx::result<std::unique_ptr<paver::DevicePartitioner>> CreatePartitioner(
      BlockDevice* gpt, fidl::ClientEnd<fuchsia_io::Directory> svc_root) {
    zx::result controller = ControllerFromBlock(gpt);
    if (controller.is_error()) {
      return controller.take_error();
    }
    zx::result devices = paver::BlockDevices::CreateDevfs(devmgr_.devfs_root().duplicate());
    if (devices.is_error()) {
      return devices.take_error();
    }
    std::shared_ptr<paver::Context> context;
    return paver::EfiDevicePartitioner::Initialize(*devices, std::move(svc_root), paver::Arch::kX64,
                                                   std::move(controller.value()), context);
  }

  void ResetPartitionTablesTest();

  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
};

TEST_F(EfiDevicePartitionerTests, InitializeWithoutGptFails) {
  std::unique_ptr<BlockDevice> dev;
  ASSERT_NO_FATAL_FAILURE(CreateDisk(&dev));

  ASSERT_NOT_OK(CreatePartitioner({}));
}

TEST_F(EfiDevicePartitionerTests, InitializeTwoCandidatesWithoutFvmFails) {
  std::unique_ptr<BlockDevice> gpt, gpt2;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithGpt(&gpt));
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithGpt(&gpt2));

  ASSERT_NOT_OK(CreatePartitioner({}));
}

TEST_F(EfiDevicePartitionerTests, InitializeWithMultipleCandidateGPTsFailsWithoutExplicitDevice) {
  // Set up a two valid GPTs.
  std::unique_ptr<BlockDevice> gpt, gpt2;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithUefiGpt(&gpt, 64 * kGibibyte));
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithUefiGpt(&gpt2, 64 * kGibibyte));

  ASSERT_OK(CreatePartitioner(gpt.get()));
  ASSERT_OK(CreatePartitioner(gpt2.get()));

  // Note that this time we don't pass in a block device fd.
  ASSERT_NOT_OK(CreatePartitioner({}));
}

TEST_F(EfiDevicePartitionerTests, FindOldBootloaderPartitionName) {
  std::unique_ptr<BlockDevice> gpt;
  ASSERT_NO_FATAL_FAILURE(
      CreateDiskWithGpt(&gpt, 64 * kGibibyte,
                        {
                            PartitionDescription{"efi-system", Uuid(kEfiType), 0x22, 0x8000},
                        }));

  auto partitioner = CreatePartitioner(gpt.get());
  ASSERT_OK(partitioner);
  ASSERT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderA)));
}

TEST_F(EfiDevicePartitionerTests, SupportsPartition) {
  std::unique_ptr<BlockDevice> gpt;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithUefiGpt(&gpt, 64 * kGibibyte));

  zx::result status = CreatePartitioner(gpt.get());
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kBootloaderA)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconA)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconB)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconR)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaA)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaB)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaR)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kAbrMeta)));
  EXPECT_TRUE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));
  EXPECT_TRUE(partitioner->SupportsPartition(
      PartitionSpec(paver::Partition::kFuchsiaVolumeManager, paver::kOpaqueVolumeContentType)));

  // Unsupported partition type.
  EXPECT_FALSE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kUnknown)));

  // Unsupported content type.
  EXPECT_FALSE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconA, "foo_type")));
}

TEST_F(EfiDevicePartitionerTests, ValidatePayload) {
  std::unique_ptr<BlockDevice> gpt;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithUefiGpt(&gpt, 64 * kGibibyte));

  zx::result status = CreatePartitioner(gpt.get());
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  // Test invalid partitions.
  ASSERT_NOT_OK(partitioner->ValidatePayload(PartitionSpec(paver::Partition::kZirconA),
                                             std::span<uint8_t>()));
  ASSERT_NOT_OK(partitioner->ValidatePayload(PartitionSpec(paver::Partition::kZirconB),
                                             std::span<uint8_t>()));
  ASSERT_NOT_OK(partitioner->ValidatePayload(PartitionSpec(paver::Partition::kZirconR),
                                             std::span<uint8_t>()));

  // Non-kernel partitions are not validated.
  ASSERT_OK(partitioner->ValidatePayload(PartitionSpec(paver::Partition::kAbrMeta),
                                         std::span<uint8_t>()));
}

TEST_F(EfiDevicePartitionerTests, OnStopRebootBootloader) {
  std::unique_ptr<BlockDevice> gpt;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithGpt(
      &gpt, 64 * kGibibyte,
      {
          PartitionDescription{GUID_EFI_NAME, Uuid(kEfiType), 0x8023, 0x8000},
          PartitionDescription{GUID_ABR_META_NAME, Uuid(kAbrMetaType), 0x10023, 0x8},
      }));

  {
    FakeSvc fake_svc(loop_.dispatcher());
    zx::result svc = fake_svc.svc();
    EXPECT_OK(svc);

    zx::result partitioner_status = CreatePartitioner(gpt.get(), std::move(svc.value()));
    ASSERT_OK(partitioner_status);
    std::unique_ptr<paver::DevicePartitioner> partitioner = std::move(partitioner_status.value());

    ASSERT_NO_FATAL_FAILURE(WaitForBlockDevices(2));

    // Set Termination system state to "reboot to bootloader"
    fake_svc.fake_system_shutdown_state().SetTerminationSystemState(
        SystemPowerState::kRebootBootloader);

    // Trigger OnStop event that should set one shot flag
    ASSERT_OK(partitioner->OnStop());

    // Verify ABR flags
    auto partition = partitioner->FindPartition(paver::PartitionSpec(paver::Partition::kAbrMeta));
    ASSERT_OK(partition);
    auto abr_partition_client = abr::AbrPartitionClient::Create(std::move(partition.value()));
    ASSERT_OK(abr_partition_client);
    auto abr_flags_res = abr_partition_client.value()->GetAndClearOneShotFlags();
    ASSERT_OK(abr_flags_res);
    EXPECT_TRUE(AbrIsOneShotBootloaderBootSet(abr_flags_res.value()));
    EXPECT_FALSE(AbrIsOneShotRecoveryBootSet(abr_flags_res.value()));
  }
}

TEST_F(EfiDevicePartitionerTests, OnStopRebootRecovery) {
  std::unique_ptr<BlockDevice> gpt;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithGpt(
      &gpt, 64 * kGibibyte,
      {
          PartitionDescription{GUID_EFI_NAME, Uuid(kEfiType), 0x8023, 0x8000},
          PartitionDescription{GUID_ABR_META_NAME, Uuid(kAbrMetaType), 0x10023, 0x8},
      }));

  {
    FakeSvc fake_svc(loop_.dispatcher());
    zx::result svc = fake_svc.svc();
    EXPECT_OK(svc);

    zx::result partitioner_status = CreatePartitioner(gpt.get(), std::move(svc.value()));
    ASSERT_OK(partitioner_status);
    std::unique_ptr<paver::DevicePartitioner> partitioner = std::move(partitioner_status.value());

    ASSERT_NO_FATAL_FAILURE(WaitForBlockDevices(2));

    // Set Termination system state to "reboot to bootloader"
    fake_svc.fake_system_shutdown_state().SetTerminationSystemState(
        SystemPowerState::kRebootRecovery);

    // Trigger OnStop event that should set one shot flag
    ASSERT_OK(partitioner->OnStop());

    // Verify ABR flags
    auto partition = partitioner->FindPartition(paver::PartitionSpec(paver::Partition::kAbrMeta));
    ASSERT_OK(partition);
    auto abr_partition_client = abr::AbrPartitionClient::Create(std::move(partition.value()));
    ASSERT_OK(abr_partition_client);
    auto abr_flags_res = abr_partition_client.value()->GetAndClearOneShotFlags();
    ASSERT_OK(abr_flags_res);
    EXPECT_FALSE(AbrIsOneShotBootloaderBootSet(abr_flags_res.value()));
    EXPECT_TRUE(AbrIsOneShotRecoveryBootSet(abr_flags_res.value()));
  }
}

class EfiDevicePartitionerWithStorageHostTests : public EfiDevicePartitionerTests {
 protected:
  IsolatedDevmgr::Args BaseDevmgrArgs() override {
    IsolatedDevmgr::Args args;
    args.enable_storage_host = true;
    args.netboot = true;
    args.disable_block_watcher = true;
    return args;
  }

  zx::result<std::unique_ptr<paver::DevicePartitioner>> CreatePartitioner(
      BlockDevice* gpt, fidl::ClientEnd<fuchsia_io::Directory> svc_root) override {
    zx::result controller = ControllerFromBlock(gpt);
    if (controller.is_error()) {
      return controller.take_error();
    }

    zx::result devices = paver::BlockDevices::CreateFromPartitionService(devmgr_.RealmExposedDir());
    if (devices.is_error()) {
      return devices.take_error();
    }

    std::shared_ptr<paver::Context> context;
    return paver::EfiDevicePartitioner::Initialize(*devices, svc_root, paver::Arch::kX64,
                                                   std::move(controller.value()), context);
  }
};

void EfiDevicePartitionerTests::ResetPartitionTablesTest() {
  const Uuid etc_guid = Uuid::Generate();
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(
      CreateDiskWithGpt(&gpt_dev, 64 * kGibibyte,
                        {
                            PartitionDescription{"efi", Uuid(kEfiType), 0x22, 0x1},
                            PartitionDescription{"efi-system", Uuid(kEfiType), 0x23, 0x8000},
                            PartitionDescription{GUID_EFI_NAME, Uuid(kEfiType), 0x8023, 0x8000},
                            PartitionDescription{"ZIRCON-A", Uuid(kZirconAType), 0x10023, 0x1},
                            PartitionDescription{"zircon_b", Uuid(kZirconBType), 0x10024, 0x1},
                            PartitionDescription{"zircon r", Uuid(kZirconRType), 0x10025, 0x1},
                            PartitionDescription{"vbmeta-a", Uuid(kVbMetaAType), 0x10026, 0x1},
                            PartitionDescription{"VBMETA_B", Uuid(kVbMetaBType), 0x10027, 0x1},
                            PartitionDescription{"VBMETA R", Uuid(kVbMetaRType), 0x10028, 0x1},
                            PartitionDescription{"abrmeta", Uuid(kAbrMetaType), 0x10029, 0x1},
                            PartitionDescription{"FVM", Uuid(kFvmType), 0x10030, 0x1},
                            PartitionDescription{"etc", etc_guid, 0x10031, 0x400},
                        }));

  // Create EFI device partitioner and initialise partition tables.
  zx::result status = CreatePartitioner();
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  ASSERT_NO_FATAL_FAILURE(WaitForBlockDevices(1 + 12));

  ASSERT_OK(partitioner->ResetPartitionTables());

  ASSERT_NO_FATAL_FAILURE(WaitForBlockDevices(12));

  // Ensure the final partition layout looks like we expect it to.
  // Non-Fuchsia partitions ought to have been preserved at their old offsets, and Fuchsia
  // partitions should be dynamically allocated in a first-fit manner.
  // For clarity they are listed in order of non-Fuchsia partitions followed by Fuchsia partitions,
  // but the order is not necessarily representative of the GPT partition table entries.
  const std::array<PartitionDescription, 12> partitions_at_end{
      // Preserved Non-Fuchsia partitions
      PartitionDescription{"efi", Uuid(kEfiType), 0x22, 0x1},
      PartitionDescription{"efi-system", Uuid(kEfiType), 0x23, 0x8000},
      PartitionDescription{"etc", etc_guid, 0x10031, 0x400},
      // Reallocated Fuchsia partitions
      PartitionDescription{GUID_EFI_NAME, Uuid(kEfiType), 0x8023, 0x8000},
      PartitionDescription{GUID_ZIRCON_A_NAME, Uuid(kZirconAType), 0x10431, 0x40000},
      PartitionDescription{GUID_ZIRCON_B_NAME, Uuid(kZirconBType), 0x50431, 0x40000},
      PartitionDescription{GUID_ZIRCON_R_NAME, Uuid(kZirconRType), 0x90431, 0x60000},
      PartitionDescription{GUID_VBMETA_A_NAME, Uuid(kVbMetaAType), 0xf0431, 0x80},
      PartitionDescription{GUID_VBMETA_B_NAME, Uuid(kVbMetaBType), 0xf04b1, 0x80},
      PartitionDescription{GUID_VBMETA_R_NAME, Uuid(kVbMetaRType), 0xf0531, 0x80},
      PartitionDescription{GUID_ABR_META_NAME, Uuid(kAbrMetaType), 0x10023, 0x8},
      PartitionDescription{GUID_FVM_NAME, Uuid(kFvmType), 0xf05b1, 0x7000000},
  };
  ASSERT_NO_FATAL_FAILURE(EnsurePartitionsMatch(partitions_at_end));

  // Make sure we can find the important partitions.
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kAbrMeta)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));
  EXPECT_OK(partitioner->FindPartition(
      PartitionSpec(paver::Partition::kFuchsiaVolumeManager, paver::kOpaqueVolumeContentType)));

  // Check that we found the correct bootloader partition.
  auto status2 = partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderA));
  EXPECT_OK(status2);

  auto status3 = status2->GetPartitionSize();
  EXPECT_OK(status3);
  EXPECT_EQ(status3.value(), 0x8000 * block_size_);
}

TEST_F(EfiDevicePartitionerTests, ResetPartitionTables) {
  ASSERT_NO_FATAL_FAILURE(ResetPartitionTablesTest());
}

TEST_F(EfiDevicePartitionerWithStorageHostTests, ResetPartitionTables) {
  ASSERT_NO_FATAL_FAILURE(ResetPartitionTablesTest());
}

TEST_F(EfiDevicePartitionerWithStorageHostTests, InitializeWithoutGptFails) {
  std::unique_ptr<BlockDevice> gpt;
  ASSERT_NO_FATAL_FAILURE(CreateDisk(64 * kGibibyte, &gpt));

  ASSERT_NOT_OK(EfiDevicePartitionerTests::CreatePartitioner());
}

TEST_F(EfiDevicePartitionerWithStorageHostTests, InitializeWithoutFvmSucceeds) {
  std::unique_ptr<BlockDevice> gpt;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithUefiGpt(&gpt, 64 * kGibibyte));

  ASSERT_OK(EfiDevicePartitionerTests::CreatePartitioner());
}

TEST_F(EfiDevicePartitionerWithStorageHostTests, InitializePartitionsWithoutExplicitDevice) {
  std::unique_ptr<BlockDevice> gpt;
  ASSERT_NO_FATAL_FAILURE(
      CreateDiskWithGpt(&gpt, 64 * kGibibyte,
                        {
                            PartitionDescription{"efi", Uuid(kEfiType), 0x22, 0x8000},
                            PartitionDescription{GUID_EFI_NAME, Uuid(kEfiType), 0x8023, 0x8000},
                        }));

  ASSERT_NOT_OK(EfiDevicePartitionerTests::CreatePartitioner(gpt.get()));

  // Note that this time we don't pass in a block device fd.
  ASSERT_OK(EfiDevicePartitionerTests::CreatePartitioner({}));
}

class FixedDevicePartitionerTests : public PaverTest {
 protected:
  void SetUp() override {
    PaverTest::SetUp();
    IsolatedDevmgr::Args args;
    args.disable_block_watcher = false;

    ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr_));

    ASSERT_OK(RecursiveWaitForFile(devmgr_.devfs_root().get(), "sys/platform/ram-disk/ramctl")
                  .status_value());
  }

  IsolatedDevmgr devmgr_;
};

TEST_F(FixedDevicePartitionerTests, UseBlockInterfaceTest) {
  zx::result devices = paver::BlockDevices::CreateDevfs(devmgr_.devfs_root().duplicate());
  ASSERT_OK(devices);
  auto status = paver::FixedDevicePartitioner::Initialize(*devices, {});
  ASSERT_OK(status);
  ASSERT_FALSE(status->IsFvmWithinFtl());
}

TEST_F(FixedDevicePartitionerTests, WipeFvmTest) {
  zx::result devices = paver::BlockDevices::CreateDevfs(devmgr_.devfs_root().duplicate());
  ASSERT_OK(devices);
  auto status = paver::FixedDevicePartitioner::Initialize(*devices, {});
  ASSERT_OK(status);
  ASSERT_OK(status->WipeFvm());
}

TEST_F(FixedDevicePartitionerTests, FindPartitionTest) {
  std::unique_ptr<BlockDevice> fvm, bootloader, zircon_a, zircon_b, zircon_r, vbmeta_a, vbmeta_b,
      vbmeta_r;
  ASSERT_NO_FATAL_FAILURE(BlockDevice::Create(devmgr_.devfs_root(), kBootloaderType, &bootloader));
  ASSERT_NO_FATAL_FAILURE(BlockDevice::Create(devmgr_.devfs_root(), kZirconAType, &zircon_a));
  ASSERT_NO_FATAL_FAILURE(BlockDevice::Create(devmgr_.devfs_root(), kZirconBType, &zircon_b));
  ASSERT_NO_FATAL_FAILURE(BlockDevice::Create(devmgr_.devfs_root(), kZirconRType, &zircon_r));
  ASSERT_NO_FATAL_FAILURE(BlockDevice::Create(devmgr_.devfs_root(), kVbMetaAType, &vbmeta_a));
  ASSERT_NO_FATAL_FAILURE(BlockDevice::Create(devmgr_.devfs_root(), kVbMetaBType, &vbmeta_b));
  ASSERT_NO_FATAL_FAILURE(BlockDevice::Create(devmgr_.devfs_root(), kVbMetaRType, &vbmeta_r));
  ASSERT_NO_FATAL_FAILURE(BlockDevice::Create(devmgr_.devfs_root(), kFvmType, &fvm));

  std::shared_ptr<paver::Context> context = std::make_shared<paver::Context>();
  zx::result devices = paver::BlockDevices::CreateDevfs(devmgr_.devfs_root().duplicate());
  ASSERT_OK(devices);
  zx::result partitioner_result = paver::DevicePartitionerFactory::Create(
      *devices, kInvalidSvcRoot, paver::Arch::kArm64, context);
  ASSERT_OK(partitioner_result);
  std::unique_ptr partitioner = std::move(partitioner_result.value());

  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));
}

TEST_F(FixedDevicePartitionerTests, SupportsPartitionTest) {
  zx::result devices = paver::BlockDevices::CreateDevfs(devmgr_.devfs_root().duplicate());
  ASSERT_OK(devices);
  auto status = paver::FixedDevicePartitioner::Initialize(*devices, {});
  ASSERT_OK(status);
  auto& partitioner = status.value();

  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kBootloaderA)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconA)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconB)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconR)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaA)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaB)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaR)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kAbrMeta)));
  EXPECT_TRUE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));

  // Unsupported partition type.
  EXPECT_FALSE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kUnknown)));

  // Unsupported content type.
  EXPECT_FALSE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconA, "foo_type")));
}

class SherlockPartitionerTests : public GptDevicePartitionerTests {
 protected:
  SherlockPartitionerTests() : GptDevicePartitionerTests("sherlock") {}

  // Create a DevicePartition for a device.
  zx::result<std::unique_ptr<paver::DevicePartitioner>> CreatePartitioner(BlockDevice* gpt) {
    fidl::ClientEnd<fuchsia_io::Directory> svc_root = RealmExposedDir();
    zx::result controller = ControllerFromBlock(gpt);
    if (controller.is_error()) {
      return controller.take_error();
    }
    zx::result devices = paver::BlockDevices::CreateDevfs(devmgr_.devfs_root().duplicate());
    if (devices.is_error()) {
      return devices.take_error();
    }
    return paver::SherlockPartitioner::Initialize(*devices, svc_root,
                                                  std::move(controller.value()));
  }
};

TEST_F(SherlockPartitionerTests, InitializeWithoutGptFails) {
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDisk(&gpt_dev));

  ASSERT_NOT_OK(CreatePartitioner(nullptr));
}

TEST_F(SherlockPartitionerTests, FindPartitionNewGuids) {
  std::unique_ptr<BlockDevice> gpt_dev;
  constexpr uint64_t kBlockCount = 0x748034;
  ASSERT_NO_FATAL_FAILURE(
      CreateDiskWithGpt(&gpt_dev, kBlockCount * block_size_,
                        // partition size / location is arbitrary
                        {
                            {GPT_DURABLE_BOOT_NAME, Uuid(kDurableBootType), 0x10400, 0x10000},
                            {GPT_VBMETA_A_NAME, Uuid(kVbMetaType), 0x20400, 0x10000},
                            {GPT_VBMETA_B_NAME, Uuid(kVbMetaType), 0x30400, 0x10000},
                            {GPT_VBMETA_R_NAME, Uuid(kVbMetaType), 0x40400, 0x10000},
                            {GPT_ZIRCON_A_NAME, Uuid(kZirconType), 0x50400, 0x10000},
                            {GPT_ZIRCON_B_NAME, Uuid(kZirconType), 0x60400, 0x10000},
                            {GPT_ZIRCON_R_NAME, Uuid(kZirconType), 0x70400, 0x10000},
                            {GPT_FVM_NAME, Uuid(kNewFvmType), 0x80400, 0x10000},
                        }));

  zx::result status = CreatePartitioner(gpt_dev.get());
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  // Make sure we can find the important partitions.
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kAbrMeta)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));
}

TEST_F(SherlockPartitionerTests, FindPartitionNewGuidsWithWrongTypeGUIDS) {
  // Due to a bootloader bug (b/173801312), the type GUID's may be reset in certain conditions.
  // This test verifies that the sherlock partitioner only looks at the partition name.
  constexpr uint64_t kBlockCount = 0x748034;
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(
      CreateDiskWithGpt(&gpt_dev, kBlockCount * block_size_,
                        {
                            {GPT_DURABLE_BOOT_NAME, Uuid(kStateLinuxGuid), 0x10400, 0x10000},
                            {GPT_VBMETA_A_NAME, Uuid(kStateLinuxGuid), 0x20400, 0x10000},
                            {GPT_VBMETA_B_NAME, Uuid(kStateLinuxGuid), 0x30400, 0x10000},
                            {GPT_VBMETA_R_NAME, Uuid(kStateLinuxGuid), 0x40400, 0x10000},
                            {GPT_ZIRCON_A_NAME, Uuid(kStateLinuxGuid), 0x50400, 0x10000},
                            {GPT_ZIRCON_B_NAME, Uuid(kStateLinuxGuid), 0x60400, 0x10000},
                            {GPT_ZIRCON_R_NAME, Uuid(kStateLinuxGuid), 0x70400, 0x10000},
                            {GPT_FVM_NAME, Uuid(kStateLinuxGuid), 0x80400, 0x10000},
                        }));

  zx::result status = CreatePartitioner(gpt_dev.get());
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  // Make sure we can find the important partitions.
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kAbrMeta)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));
}

TEST_F(SherlockPartitionerTests, FindPartitionSecondary) {
  constexpr uint64_t kBlockCount = 0x748034;
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(
      CreateDiskWithGpt(&gpt_dev, kBlockCount * block_size_,
                        {
                            {GPT_DURABLE_BOOT_NAME, Uuid(kStateLinuxGuid), 0x10400, 0x10000},
                            {GPT_VBMETA_A_NAME, Uuid(kStateLinuxGuid), 0x20400, 0x10000},
                            {GPT_VBMETA_B_NAME, Uuid(kStateLinuxGuid), 0x30400, 0x10000},
                            // Removed vbmeta_r to validate that it is not found
                            {"boot", Uuid(kStateLinuxGuid), 0x50400, 0x10000},
                            {"system", Uuid(kStateLinuxGuid), 0x60400, 0x10000},
                            {"recovery", Uuid(kStateLinuxGuid), 0x70400, 0x10000},
                            {GPT_FVM_NAME, Uuid(kStateLinuxGuid), 0x80400, 0x10000},
                        }));

  zx::result status = CreatePartitioner(gpt_dev.get());
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  // Make sure we can find the important partitions.
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kAbrMeta)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaB)));
  EXPECT_NOT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));
}

TEST_F(SherlockPartitionerTests, ShouldNotFindPartitionBoot) {
  constexpr uint64_t kBlockCount = 0x748034;
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(
      CreateDiskWithGpt(&gpt_dev, kBlockCount * block_size_,
                        {
                            {"bootloader", Uuid(kStateLinuxGuid), 0x10400, 0x10000},
                        }));

  zx::result status = CreatePartitioner(gpt_dev.get());
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  // Make sure we can't find zircon_a, which is aliased to "boot". The GPT logic would
  // previously only check prefixes, so "boot" would match with "bootloader".
  EXPECT_NOT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconA)));
}

TEST_F(SherlockPartitionerTests, FindBootloader) {
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithGpt(&gpt_dev));

  zx::result status = CreatePartitioner(gpt_dev.get());
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  // No boot0/boot1 yet, we shouldn't be able to find the bootloader.
  ASSERT_NOT_OK(
      partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderA, "skip_metadata")));

  std::unique_ptr<BlockDevice> boot0_dev, boot1_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDisk(kBlockCount * kBlockSize, kBoot0Type, &boot0_dev));
  ASSERT_NO_FATAL_FAILURE(CreateDisk(kBlockCount * kBlockSize, kBoot1Type, &boot1_dev));

  // Now it should succeed.
  ASSERT_OK(
      partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderA, "skip_metadata")));
}

TEST_F(SherlockPartitionerTests, SupportsPartition) {
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithGpt(&gpt_dev, 64 * kMebibyte));

  zx::result status = CreatePartitioner(gpt_dev.get());
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  EXPECT_TRUE(partitioner->SupportsPartition(
      PartitionSpec(paver::Partition::kBootloaderA, "skip_metadata")));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconA)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconB)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconR)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaA)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaB)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaR)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kAbrMeta)));
  EXPECT_TRUE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));

  // Unsupported partition type.
  EXPECT_FALSE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kUnknown)));

  // Unsupported content type.
  EXPECT_FALSE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconA, "foo_type")));
}

class MoonflowerPartitionerTests : public GptDevicePartitionerTests {
 protected:
  MoonflowerPartitionerTests() : GptDevicePartitionerTests("sorrel") {}

  // Create a DevicePartition for a device.
  zx::result<std::unique_ptr<paver::DevicePartitioner>> CreatePartitioner(BlockDevice* gpt) {
    fidl::ClientEnd<fuchsia_io::Directory> svc_root = RealmExposedDir();
    zx::result controller = ControllerFromBlock(gpt);
    if (controller.is_error()) {
      return controller.take_error();
    }
    zx::result devices = paver::BlockDevices::CreateDevfs(devmgr_.devfs_root().duplicate());
    if (devices.is_error()) {
      return devices.take_error();
    }
    return paver::MoonflowerPartitioner::Initialize(*devices, svc_root,
                                                    std::move(controller.value()));
  }
};

TEST_F(MoonflowerPartitionerTests, InitializeWithoutGptFails) {
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDisk(&gpt_dev));

  ASSERT_NOT_OK(CreatePartitioner({}));
}

TEST_F(MoonflowerPartitionerTests, InitializeWithoutFvmFails) {
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithGpt(&gpt_dev, 32 * kGibibyte));

  ASSERT_NOT_OK(CreatePartitioner({}));
}

TEST_F(MoonflowerPartitionerTests, FindPartition) {
  constexpr uint64_t kBlockCount = 0x748034;
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(
      CreateDiskWithGpt(&gpt_dev, kBlockCount * block_size_,
                        {
                            {GUID_ABR_META_NAME, Uuid(kAbrMetaType), 0x10400, 0x10000},
                            {"dtbo_a", Uuid(kDummyType), 0x30400, 0x10000},
                            {"dtbo_b", Uuid(kDummyType), 0x40400, 0x10000},
                            {"boot_a", Uuid(kZirconAType), 0x50400, 0x10000},
                            {"boot_b", Uuid(kZirconBType), 0x60400, 0x10000},
                            {"system_a", Uuid(kDummyType), 0x70400, 0x10000},
                            {"system_b", Uuid(kDummyType), 0x80400, 0x10000},
                            {GPT_VBMETA_A_NAME, Uuid(kVbMetaAType), 0x90400, 0x10000},
                            {GPT_VBMETA_B_NAME, Uuid(kVbMetaBType), 0xa0400, 0x10000},
                            {"reserved_a", Uuid(kDummyType), 0xc0400, 0x10000},
                            {"reserved_b", Uuid(kDummyType), 0xd0400, 0x10000},
                            {"reserved_c", Uuid(kVbMetaRType), 0xe0400, 0x10000},
                            {"cache", Uuid(kZirconRType), 0xf0400, 0x10000},
                            {"super", Uuid(kFvmType), 0x100400, 0x10000},
                        }));

  zx::result status = CreatePartitioner(gpt_dev.get());
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  // Make sure we can find the important partitions.
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderA, "dtbo")));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderB, "dtbo")));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));
}

TEST_F(MoonflowerPartitionerTests, SupportsPartition) {
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithGpt(&gpt_dev, 64 * kMebibyte));

  zx::result status = CreatePartitioner(gpt_dev.get());
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  EXPECT_TRUE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kBootloaderA, "dtbo")));
  EXPECT_TRUE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kBootloaderB, "dtbo")));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconA)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconB)));
  EXPECT_TRUE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));
  // Unsupported partition type.
  EXPECT_FALSE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kUnknown)));

  // Unsupported content type.
  EXPECT_FALSE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kAbrMeta, "foo_type")));
}

class LuisPartitionerTests : public GptDevicePartitionerTests {
 protected:
  LuisPartitionerTests() : GptDevicePartitionerTests("luis", 512, "_a") {}

  // Create a DevicePartition for a device.
  zx::result<std::unique_ptr<paver::DevicePartitioner>> CreatePartitioner(
      fidl::ClientEnd<fuchsia_device::Controller> device) {
    fidl::ClientEnd<fuchsia_io::Directory> svc_root = RealmExposedDir();
    zx::result devices = paver::BlockDevices::CreateDevfs(devmgr_.devfs_root().duplicate());
    if (devices.is_error()) {
      return devices.take_error();
    }
    return paver::LuisPartitioner::Initialize(*devices, svc_root, std::move(device));
  }
};

TEST_F(LuisPartitionerTests, InitializeWithoutGptFails) {
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDisk(&gpt_dev));

  ASSERT_NOT_OK(CreatePartitioner({}));
}

TEST_F(LuisPartitionerTests, InitializeWithoutFvmFails) {
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithGpt(&gpt_dev, 32 * kGibibyte));

  ASSERT_NOT_OK(CreatePartitioner({}));
}

TEST_F(LuisPartitionerTests, FindPartition) {
  // kBlockCount should be a value large enough to accommodate all partitions and blocks reserved
  // by gpt. The current value is copied from the case of sherlock. As of now, we assume they
  // have the same disk size requirement.
  constexpr uint64_t kBlockCount = 0x748034;
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(
      CreateDiskWithGpt(&gpt_dev, kBlockCount * block_size_,
                        {
                            {GPT_DURABLE_BOOT_NAME, Uuid(kDummyType), 0x10400, 0x10000},
                            {GPT_BOOTLOADER_A_NAME, Uuid(kDummyType), 0x30400, 0x10000},
                            {GPT_BOOTLOADER_B_NAME, Uuid(kDummyType), 0x40400, 0x10000},
                            {GPT_BOOTLOADER_R_NAME, Uuid(kDummyType), 0x50400, 0x10000},
                            {GPT_VBMETA_A_NAME, Uuid(kDummyType), 0x60400, 0x10000},
                            {GPT_VBMETA_B_NAME, Uuid(kDummyType), 0x70400, 0x10000},
                            {GPT_VBMETA_R_NAME, Uuid(kDummyType), 0x80400, 0x10000},
                            {GPT_ZIRCON_A_NAME, Uuid(kDummyType), 0x90400, 0x10000},
                            {GPT_ZIRCON_B_NAME, Uuid(kDummyType), 0xa0400, 0x10000},
                            {GPT_ZIRCON_R_NAME, Uuid(kDummyType), 0xb0400, 0x10000},
                            {GPT_FACTORY_NAME, Uuid(kDummyType), 0xc0400, 0x10000},
                            {GPT_FVM_NAME, Uuid(kDummyType), 0xe0400, 0x10000},
                        }));

  zx::result controller = ControllerFromBlock(gpt_dev.get());
  ASSERT_OK(controller);

  zx::result status = CreatePartitioner(std::move(controller.value()));
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  EXPECT_NOT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderA)));

  std::unique_ptr<BlockDevice> boot0_dev, boot1_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDisk(kBlockCount * kBlockSize, kBoot0Type, &boot0_dev));
  ASSERT_NO_FATAL_FAILURE(CreateDisk(kBlockCount * kBlockSize, kBoot1Type, &boot1_dev));

  // Make sure we can find the important partitions.
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kAbrMeta)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));
}

TEST_F(LuisPartitionerTests, CreateAbrClient) {
  constexpr uint64_t kBlockCount = 0x748034;
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(
      CreateDiskWithGpt(&gpt_dev, kBlockCount * block_size_,
                        {
                            {GPT_DURABLE_BOOT_NAME, Uuid(kDurableBootType), 0x10400, 0x10000},
                            {GPT_FVM_NAME, Uuid(kNewFvmType), 0x20400, 0x10000},
                        }));

  fidl::ClientEnd<fuchsia_io::Directory> svc_root = RealmExposedDir();
  std::shared_ptr<paver::Context> context;
  zx::result devices = paver::BlockDevices::CreateDevfs(devmgr_.devfs_root().duplicate());
  ASSERT_OK(devices);
  zx::result partitioner =
      paver::LuisPartitionerFactory().New(*devices, svc_root, paver::Arch::kArm64, context, {});
  ASSERT_OK(partitioner);
  EXPECT_OK(partitioner->CreateAbrClient());
}

TEST_F(LuisPartitionerTests, SupportsPartition) {
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithGpt(&gpt_dev, 64 * kMebibyte));

  zx::result controller = ControllerFromBlock(gpt_dev.get());
  ASSERT_OK(controller);

  zx::result status = CreatePartitioner(std::move(controller.value()));
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kBootloaderA)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kBootloaderB)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kBootloaderR)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconA)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconB)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconR)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaA)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaB)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaR)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kAbrMeta)));
  EXPECT_TRUE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));
  // Unsupported partition type.
  EXPECT_FALSE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kUnknown)));

  // Unsupported content type.
  EXPECT_FALSE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kAbrMeta, "foo_type")));
}

class NelsonPartitionerTests : public GptDevicePartitionerTests {
 protected:
  static constexpr size_t kNelsonBlockSize = 512;
  static constexpr size_t kTplSize = 1024;
  static constexpr size_t kBootloaderSize = paver::kNelsonBL2Size + kTplSize;
  static constexpr uint8_t kBL2ImageValue = 0x01;
  static constexpr uint8_t kTplImageValue = 0x02;
  static constexpr size_t kTplSlotAOffset = 0x3000;
  static constexpr size_t kTplSlotBOffset = 0x4000;
  static constexpr size_t kUserTplBlockCount = 0x1000;

  NelsonPartitionerTests() : GptDevicePartitionerTests("nelson", kNelsonBlockSize, "_a") {}

  // Create a DevicePartition for a device.
  zx::result<std::unique_ptr<paver::DevicePartitioner>> CreatePartitioner(BlockDevice* gpt) {
    fidl::ClientEnd<fuchsia_io::Directory> svc_root = RealmExposedDir();
    zx::result controller = ControllerFromBlock(gpt);
    if (controller.is_error()) {
      return controller.take_error();
    }
    zx::result devices = paver::BlockDevices::CreateDevfs(devmgr_.devfs_root().duplicate());
    if (devices.is_error()) {
      return devices.take_error();
    }
    return paver::NelsonPartitioner::Initialize(*devices, svc_root, std::move(controller.value()));
  }

  static void CreateBootloaderPayload(zx::vmo* out) {
    fzl::VmoMapper mapper;
    ASSERT_OK(
        mapper.CreateAndMap(kBootloaderSize, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr, out));
    uint8_t* start = static_cast<uint8_t*>(mapper.start());
    memset(start, kBL2ImageValue, paver::kNelsonBL2Size);
    memset(start + paver::kNelsonBL2Size, kTplImageValue, kTplSize);
  }

  void TestBootloaderWrite(const PartitionSpec& spec, uint8_t tpl_a_expected,
                           uint8_t tpl_b_expected) {
    std::unique_ptr<BlockDevice> gpt_dev, boot0, boot1;
    ASSERT_NO_FATAL_FAILURE(InitializeBlockDeviceForBootloaderTest(&gpt_dev, &boot0, &boot1));

    zx::result status = CreatePartitioner(gpt_dev.get());
    ASSERT_OK(status);
    std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();
    {
      auto partition_client = partitioner->FindPartition(spec);
      ASSERT_OK(partition_client);

      zx::vmo bootloader_payload;
      ASSERT_NO_FATAL_FAILURE(CreateBootloaderPayload(&bootloader_payload));
      ASSERT_OK(partition_client->Write(bootloader_payload, kBootloaderSize));
    }
    const size_t bl2_blocks = paver::kNelsonBL2Size / block_size_;
    const size_t tpl_blocks = kTplSize / block_size_;

    // info block stays unchanged. assume that storage data initialized as 0.
    ASSERT_NO_FATAL_FAILURE(ValidateBlockContent(boot0.get(), 0, 1, 0));
    ASSERT_NO_FATAL_FAILURE(ValidateBlockContent(boot0.get(), 1, bl2_blocks, kBL2ImageValue));
    ASSERT_NO_FATAL_FAILURE(
        ValidateBlockContent(boot0.get(), 1 + bl2_blocks, tpl_blocks, kTplImageValue));

    // info block stays unchanged
    ASSERT_NO_FATAL_FAILURE(ValidateBlockContent(boot1.get(), 0, 1, 0));
    ASSERT_NO_FATAL_FAILURE(ValidateBlockContent(boot1.get(), 1, bl2_blocks, kBL2ImageValue));
    ASSERT_NO_FATAL_FAILURE(
        ValidateBlockContent(boot1.get(), 1 + bl2_blocks, tpl_blocks, kTplImageValue));

    ASSERT_NO_FATAL_FAILURE(
        ValidateBlockContent(gpt_dev.get(), kTplSlotAOffset, tpl_blocks, tpl_a_expected));
    ASSERT_NO_FATAL_FAILURE(
        ValidateBlockContent(gpt_dev.get(), kTplSlotBOffset, tpl_blocks, tpl_b_expected));
  }

  void TestBootloaderRead(const PartitionSpec& spec, uint8_t tpl_a_data, uint8_t tpl_b_data,
                          zx::result<>* out_status, uint8_t* out) {
    std::unique_ptr<BlockDevice> gpt_dev, boot0, boot1;
    ASSERT_NO_FATAL_FAILURE(InitializeBlockDeviceForBootloaderTest(&gpt_dev, &boot0, &boot1));

    const size_t bl2_blocks = paver::kNelsonBL2Size / block_size_;
    const size_t tpl_blocks = kTplSize / block_size_;

    // Setup initial storage data
    struct initial_storage_data {
      const BlockDevice* blk_dev;
      uint64_t start_block;
      uint64_t size_in_blocks;
      uint8_t data;
    } initial_storage[] = {
        {boot0.get(), 1, bl2_blocks, kBL2ImageValue},               // bl2 in boot0
        {boot1.get(), 1, bl2_blocks, kBL2ImageValue},               // bl2 in boot1
        {boot0.get(), 1 + bl2_blocks, tpl_blocks, kTplImageValue},  // tpl in boot0
        {boot1.get(), 1 + bl2_blocks, tpl_blocks, kTplImageValue},  // tpl in boot1
        {gpt_dev.get(), kTplSlotAOffset, tpl_blocks, tpl_a_data},   // tpl_a
        {gpt_dev.get(), kTplSlotBOffset, tpl_blocks, tpl_b_data},   // tpl_b
    };
    for (auto& info : initial_storage) {
      std::vector<uint8_t> data(info.size_in_blocks * block_size_, info.data);
      ASSERT_NO_FATAL_FAILURE(
          WriteBlocks(info.blk_dev, info.start_block, info.size_in_blocks, data.data()));
    }

    zx::result status = CreatePartitioner(gpt_dev.get());
    ASSERT_OK(status);
    std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

    fzl::OwnedVmoMapper read_buf;
    ASSERT_OK(read_buf.CreateAndMap(kBootloaderSize, "test-read-bootloader"));
    auto partition_client = partitioner->FindPartition(spec);
    ASSERT_OK(partition_client);
    *out_status = partition_client->Read(read_buf.vmo(), kBootloaderSize);
    memcpy(out, read_buf.start(), kBootloaderSize);
  }

  static void ValidateBootloaderRead(const uint8_t* buf, uint8_t expected_bl2,
                                     uint8_t expected_tpl) {
    for (size_t i = 0; i < paver::kNelsonBL2Size; i++) {
      ASSERT_EQ(buf[i], expected_bl2, "bl2 mismatch at idx: %zu", i);
    }

    for (size_t i = 0; i < kTplSize; i++) {
      ASSERT_EQ(buf[i + paver::kNelsonBL2Size], expected_tpl, "tpl mismatch at idx: %zu", i);
    }
  }

  void InitializeBlockDeviceForBootloaderTest(std::unique_ptr<BlockDevice>* gpt_dev,
                                              std::unique_ptr<BlockDevice>* boot0,
                                              std::unique_ptr<BlockDevice>* boot1) {
    ASSERT_NO_FATAL_FAILURE(
        CreateDiskWithGpt(gpt_dev, 64 * kMebibyte,
                          {
                              {"tpl_a", Uuid(kDummyType), kTplSlotAOffset, kUserTplBlockCount},
                              {"tpl_b", Uuid(kDummyType), kTplSlotBOffset, kUserTplBlockCount},
                          }));

    ASSERT_NO_FATAL_FAILURE(CreateDisk(kUserTplBlockCount * kNelsonBlockSize, kBoot0Type, boot0));
    ASSERT_NO_FATAL_FAILURE(CreateDisk(kUserTplBlockCount * kNelsonBlockSize, kBoot1Type, boot1));
  }
};

TEST_F(NelsonPartitionerTests, InitializeWithoutGptFails) {
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDisk(&gpt_dev));

  ASSERT_NOT_OK(CreatePartitioner({}));
}

TEST_F(NelsonPartitionerTests, InitializeWithoutFvmSucceeds) {
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithGpt(&gpt_dev, 32 * kGibibyte));

  ASSERT_OK(CreatePartitioner({}));
}

TEST_F(NelsonPartitionerTests, FindPartition) {
  // kBlockCount should be a value large enough to accommodate all partitions and blocks reserved
  // by gpt. The current value is copied from the case of sherlock. The actual size of fvm
  // partition on nelson is yet to be finalized.
  constexpr uint64_t kBlockCount = 0x748034;
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(
      CreateDiskWithGpt(&gpt_dev, kBlockCount * block_size_,
                        {
                            // The initial gpt partitions are randomly chosen and does not
                            // necessarily reflect the actual gpt partition layout in product.
                            {GUID_ABR_META_NAME, Uuid(kAbrMetaType), 0x10400, 0x10000},
                            {"tpl_a", Uuid(kDummyType), 0x30400, 0x10000},
                            {"tpl_b", Uuid(kDummyType), 0x40400, 0x10000},
                            {"boot_a", Uuid(kZirconAType), 0x50400, 0x10000},
                            {"boot_b", Uuid(kZirconBType), 0x60400, 0x10000},
                            {"system_a", Uuid(kDummyType), 0x70400, 0x10000},
                            {"system_b", Uuid(kDummyType), 0x80400, 0x10000},
                            {GPT_VBMETA_A_NAME, Uuid(kVbMetaAType), 0x90400, 0x10000},
                            {GPT_VBMETA_B_NAME, Uuid(kVbMetaBType), 0xa0400, 0x10000},
                            {"reserved_a", Uuid(kDummyType), 0xc0400, 0x10000},
                            {"reserved_b", Uuid(kDummyType), 0xd0400, 0x10000},
                            {"reserved_c", Uuid(kVbMetaRType), 0xe0400, 0x10000},
                            {"cache", Uuid(kZirconRType), 0xf0400, 0x10000},
                            {"data", Uuid(kFvmType), 0x100400, 0x10000},
                        }));

  zx::result status = CreatePartitioner(gpt_dev.get());
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  EXPECT_NOT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderA)));

  std::unique_ptr<BlockDevice> boot0_dev, boot1_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDisk(kBlockCount * kBlockSize, kBoot0Type, &boot0_dev));
  ASSERT_NO_FATAL_FAILURE(CreateDisk(kBlockCount * kBlockSize, kBoot1Type, &boot1_dev));

  // Make sure we can find the important partitions.
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderA, "bl2")));
  EXPECT_OK(
      partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderA, "bootloader")));
  EXPECT_OK(
      partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderB, "bootloader")));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderA, "tpl")));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderB, "tpl")));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kAbrMeta)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaA)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaB)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaR)));
  EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));
}

TEST_F(NelsonPartitionerTests, CreateAbrClient) {
  constexpr uint64_t kBlockCount = 0x748034;
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(
      CreateDiskWithGpt(&gpt_dev, kBlockCount * block_size_,
                        {
                            {GUID_ABR_META_NAME, Uuid(kAbrMetaType), 0x10400, 0x10000},
                            {GPT_FVM_NAME, Uuid(kNewFvmType), 0x20400, 0x10000},
                        }));
  fidl::ClientEnd<fuchsia_io::Directory> svc_root = RealmExposedDir();
  std::shared_ptr<paver::Context> context;
  zx::result devices = paver::BlockDevices::CreateDevfs(devmgr_.devfs_root().duplicate());
  ASSERT_OK(devices);
  zx::result partitioner =
      paver::NelsonPartitionerFactory().New(*devices, svc_root, paver::Arch::kArm64, context, {});
  ASSERT_OK(partitioner);
  EXPECT_OK(partitioner->CreateAbrClient());
}

TEST_F(NelsonPartitionerTests, SupportsPartition) {
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithGpt(&gpt_dev, 64 * kMebibyte));

  zx::result status = CreatePartitioner(gpt_dev.get());
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kBootloaderA, "bl2")));
  EXPECT_TRUE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kBootloaderA, "bootloader")));
  EXPECT_TRUE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kBootloaderB, "bootloader")));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kBootloaderA, "tpl")));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kBootloaderB, "tpl")));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconA)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconB)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kZirconR)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaA)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaB)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kVbMetaR)));
  EXPECT_TRUE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kAbrMeta)));
  EXPECT_TRUE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));
  // Unsupported partition type.
  EXPECT_FALSE(partitioner->SupportsPartition(PartitionSpec(paver::Partition::kUnknown)));

  // Unsupported content type.
  EXPECT_FALSE(
      partitioner->SupportsPartition(PartitionSpec(paver::Partition::kAbrMeta, "foo_type")));
}

TEST_F(NelsonPartitionerTests, ValidatePayload) {
  std::unique_ptr<BlockDevice> gpt_dev;
  ASSERT_NO_FATAL_FAILURE(CreateDiskWithGpt(&gpt_dev, 64 * kMebibyte));

  zx::result status = CreatePartitioner(gpt_dev.get());
  ASSERT_OK(status);
  std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

  // Test invalid bootloader payload size.
  std::vector<uint8_t> payload_bl2_size(paver::kNelsonBL2Size);
  ASSERT_NOT_OK(
      partitioner->ValidatePayload(PartitionSpec(paver::Partition::kBootloaderA, "bootloader"),
                                   std::span<uint8_t>(payload_bl2_size)));
  ASSERT_NOT_OK(
      partitioner->ValidatePayload(PartitionSpec(paver::Partition::kBootloaderB, "bootloader"),
                                   std::span<uint8_t>(payload_bl2_size)));

  std::vector<uint8_t> payload_bl2_tpl_size(static_cast<size_t>(2) * 1024 * 1024);
  ASSERT_OK(
      partitioner->ValidatePayload(PartitionSpec(paver::Partition::kBootloaderA, "bootloader"),
                                   std::span<uint8_t>(payload_bl2_tpl_size)));
  ASSERT_OK(
      partitioner->ValidatePayload(PartitionSpec(paver::Partition::kBootloaderB, "bootloader"),
                                   std::span<uint8_t>(payload_bl2_tpl_size)));
}

TEST_F(NelsonPartitionerTests, WriteBootloaderA) {
  TestBootloaderWrite(PartitionSpec(paver::Partition::kBootloaderA, "bootloader"), kTplImageValue,
                      0x00);
}

TEST_F(NelsonPartitionerTests, WriteBootloaderB) {
  TestBootloaderWrite(PartitionSpec(paver::Partition::kBootloaderB, "bootloader"), 0x00,
                      kTplImageValue);
}

TEST_F(NelsonPartitionerTests, ReadBootloaderAFail) {
  auto spec = PartitionSpec(paver::Partition::kBootloaderA, "bootloader");
  std::vector<uint8_t> read_buf(kBootloaderSize);
  zx::result<> status = zx::ok();
  ASSERT_NO_FATAL_FAILURE(TestBootloaderRead(spec, 0x03, kTplImageValue, &status, read_buf.data()));
  ASSERT_NOT_OK(status);
}

TEST_F(NelsonPartitionerTests, ReadBootloaderBFail) {
  auto spec = PartitionSpec(paver::Partition::kBootloaderB, "bootloader");
  std::vector<uint8_t> read_buf(kBootloaderSize);
  zx::result<> status = zx::ok();
  ASSERT_NO_FATAL_FAILURE(TestBootloaderRead(spec, kTplImageValue, 0x03, &status, read_buf.data()));
  ASSERT_NOT_OK(status);
}

TEST_F(NelsonPartitionerTests, ReadBootloaderASucceed) {
  auto spec = PartitionSpec(paver::Partition::kBootloaderA, "bootloader");
  std::vector<uint8_t> read_buf(kBootloaderSize);
  zx::result<> status = zx::ok();
  ASSERT_NO_FATAL_FAILURE(TestBootloaderRead(spec, kTplImageValue, 0x03, &status, read_buf.data()));
  ASSERT_OK(status);
  ASSERT_NO_FATAL_FAILURE(ValidateBootloaderRead(read_buf.data(), kBL2ImageValue, kTplImageValue));
}

TEST_F(NelsonPartitionerTests, ReadBootloaderBSucceed) {
  std::vector<uint8_t> read_buf(kBootloaderSize);
  auto spec = PartitionSpec(paver::Partition::kBootloaderB, "bootloader");
  zx::result<> status = zx::ok();
  ASSERT_NO_FATAL_FAILURE(TestBootloaderRead(spec, 0x03, kTplImageValue, &status, read_buf.data()));
  ASSERT_OK(status);
  ASSERT_NO_FATAL_FAILURE(ValidateBootloaderRead(read_buf.data(), kBL2ImageValue, kTplImageValue));
}

class Vim3PartitionerTests : public GptDevicePartitionerTests {
 protected:
  static constexpr const char kDummyBootloaderHeader[] = "bootloader";

  Vim3PartitionerTests() : GptDevicePartitionerTests("vim3") {}

  // Create a DevicePartition for a device.
  zx::result<std::unique_ptr<paver::DevicePartitioner>> CreatePartitioner(
      BlockDevice* gpt = nullptr) {
    fidl::ClientEnd<fuchsia_io::Directory> svc_root = RealmExposedDir();
    zx::result controller = ControllerFromBlock(gpt);
    if (controller.is_error()) {
      return controller.take_error();
    }
    zx::result devices = paver::BlockDevices::CreateDevfs(devmgr_.devfs_root().duplicate());
    if (devices.is_error()) {
      return devices.take_error();
    }
    return paver::Vim3Partitioner::Initialize(*devices, svc_root, std::move(controller.value()));
  }

  void CreateBootloaderDevices(std::unique_ptr<BlockDevice>* boot0,
                               std::unique_ptr<BlockDevice>* boot1) {
    zx::vmo vmo0;
    ASSERT_OK(zx::vmo::create(32 * 1024 * 1024, 0, &vmo0));
    // Write the first two blocks with a placeholder value we check later in VerifyBootloaderDevice.
    ASSERT_OK(vmo0.write(kDummyBootloaderHeader, 0, strlen(kDummyBootloaderHeader)));
    ASSERT_OK(vmo0.write(kDummyBootloaderHeader, block_size_, strlen(kDummyBootloaderHeader)));
    ASSERT_NO_FATAL_FAILURE(CreateDiskWithContents(boot0, std::move(vmo0), kBoot0Type));

    zx::vmo vmo1;
    ASSERT_OK(zx::vmo::create(32 * 1024 * 1024, 0, &vmo1));
    ASSERT_OK(vmo1.write(kDummyBootloaderHeader, 0, strlen(kDummyBootloaderHeader)));
    ASSERT_OK(vmo1.write(kDummyBootloaderHeader, block_size_, strlen(kDummyBootloaderHeader)));
    ASSERT_NO_FATAL_FAILURE(CreateDiskWithContents(boot1, std::move(vmo1), kBoot1Type));
  }

  void VerifyBootloaderDevice(const BlockDevice* device) {
    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(block_size_, 0, &vmo));
    for (size_t block = 0; block < 2; ++block) {
      ASSERT_NO_FATAL_FAILURE(device->Read(vmo, block_size_, block, 0));
      char data[block_size_];
      ASSERT_OK(vmo.read(data, 0, block_size_));
      EXPECT_EQ(std::string_view(data, strlen(kDummyBootloaderHeader)),
                std::string_view(kDummyBootloaderHeader));
    }
  }

  void InitializeWithoutGptFailsTest() {
    std::unique_ptr<BlockDevice> boot0_dev;
    std::unique_ptr<BlockDevice> boot1_dev;
    std::unique_ptr<BlockDevice> gpt_dev;
    ASSERT_NO_FATAL_FAILURE(CreateBootloaderDevices(&boot0_dev, &boot1_dev));
    ASSERT_NO_FATAL_FAILURE(CreateDisk(&gpt_dev));
    ASSERT_NO_FATAL_FAILURE(WaitForBlockDevices(3));

    ASSERT_NOT_OK(CreatePartitioner());
    ASSERT_NO_FATAL_FAILURE(VerifyBootloaderDevice(boot0_dev.get()));
    ASSERT_NO_FATAL_FAILURE(VerifyBootloaderDevice(boot1_dev.get()));
  }

  void InitializeTest() {
    std::unique_ptr<BlockDevice> boot0_dev;
    std::unique_ptr<BlockDevice> boot1_dev;
    std::unique_ptr<BlockDevice> gpt_dev;
    ASSERT_NO_FATAL_FAILURE(CreateBootloaderDevices(&boot0_dev, &boot1_dev));
    constexpr uint64_t kBlockCount = 0x748034;
    ASSERT_NO_FATAL_FAILURE(
        CreateDiskWithGpt(&gpt_dev, kBlockCount * block_size_,
                          // partition size / location is arbitrary
                          {
                              {GPT_DURABLE_BOOT_NAME, Uuid(kDurableBootType), 0x10400, 0x10000},
                              {GPT_VBMETA_A_NAME, Uuid(kVbMetaType), 0x20400, 0x10000},
                              {GPT_VBMETA_B_NAME, Uuid(kVbMetaType), 0x30400, 0x10000},
                              {GPT_VBMETA_R_NAME, Uuid(kVbMetaType), 0x40400, 0x10000},
                              {GPT_ZIRCON_A_NAME, Uuid(kZirconType), 0x50400, 0x10000},
                              {GPT_ZIRCON_B_NAME, Uuid(kZirconType), 0x60400, 0x10000},
                              {GPT_ZIRCON_R_NAME, Uuid(kZirconType), 0x70400, 0x10000},
                              {GPT_FVM_NAME, Uuid(kNewFvmType), 0x80400, 0x10000},
                          }));
    ASSERT_NO_FATAL_FAILURE(WaitForBlockDevices(3 + 8));

    zx::result status = CreatePartitioner();
    ASSERT_OK(status);
    std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

    // Make sure we can find the important partitions.
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderA)));
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconA)));
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconB)));
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconR)));
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kAbrMeta)));
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaA)));
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaB)));
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaR)));
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));

    ASSERT_NO_FATAL_FAILURE(VerifyBootloaderDevice(boot0_dev.get()));
    ASSERT_NO_FATAL_FAILURE(VerifyBootloaderDevice(boot1_dev.get()));
  }
};

TEST_F(Vim3PartitionerTests, InitializeWithoutGptFails) {
  ASSERT_NO_FATAL_FAILURE(InitializeWithoutGptFailsTest());
}

TEST_F(Vim3PartitionerTests, Initialize) { ASSERT_NO_FATAL_FAILURE(InitializeTest()); }

class Vim3PartitionerWithStorageHostTests : public Vim3PartitionerTests {
 protected:
  IsolatedDevmgr::Args BaseDevmgrArgs() override {
    IsolatedDevmgr::Args args;
    args.enable_storage_host = true;
    return args;
  }
};

TEST_F(Vim3PartitionerWithStorageHostTests, InitializeWithoutGptFails) {
  ASSERT_NO_FATAL_FAILURE(InitializeWithoutGptFailsTest());
}

TEST_F(Vim3PartitionerWithStorageHostTests, Initialize) {
  ASSERT_NO_FATAL_FAILURE(InitializeTest());
}

class AndroidPartitionerTests : public GptDevicePartitionerTests {
 protected:
  AndroidPartitionerTests() = default;

  // Create a DevicePartition for a device.
  zx::result<std::unique_ptr<paver::DevicePartitioner>> CreatePartitioner(
      BlockDevice* gpt = nullptr) {
    fidl::ClientEnd<fuchsia_io::Directory> svc_root = RealmExposedDir();
    zx::result controller = ControllerFromBlock(gpt);
    if (controller.is_error()) {
      return controller.take_error();
    }
    zx::result devices = paver::BlockDevices::CreateDevfs(devmgr_.devfs_root().duplicate());
    if (devices.is_error()) {
      return devices.take_error();
    }
    return paver::AndroidDevicePartitioner::Initialize(*devices, svc_root, paver::Arch::kX64,
                                                       std::move(controller.value()), {});
  }

  void InitializeWithoutGptFailsTest() {
    std::unique_ptr<BlockDevice> primary_gpt_dev;
    ASSERT_NO_FATAL_FAILURE(CreateDisk(&primary_gpt_dev));
    ASSERT_NO_FATAL_FAILURE(WaitForBlockDevices(1));

    ASSERT_NOT_OK(CreatePartitioner());
  }

  void InitializeTest() {
    std::unique_ptr<BlockDevice> primary_gpt_dev;
    std::unique_ptr<BlockDevice> other_gpt_dev;
    constexpr uint64_t kBlockCount = 0x748034;
    ASSERT_NO_FATAL_FAILURE(
        CreateDiskWithGpt(&primary_gpt_dev, kBlockCount * block_size_,
                          // partition size / location is arbitrary
                          {
                              {"misc", Uuid(kAbrMetaType), 0x10400, 0x10000},
                              {"vbmeta_a", Uuid(kVbMetaType), 0x20400, 0x10000},
                              {"vbmeta_b", Uuid(kVbMetaType), 0x30400, 0x10000},
                              {"boot_a", Uuid(kBootloaderType), 0x50400, 0x10000},
                              {"boot_b", Uuid(kBootloaderType), 0x60400, 0x10000},
                              {"vendor_boot_a", Uuid(kZirconType), 0x70400, 0x10000},
                              {"vendor_boot_b", Uuid(kZirconType), 0x80400, 0x10000},
                              {"super", Uuid(kNewFvmType), 0x90400, 0x10000},
                          }));
    ASSERT_NO_FATAL_FAILURE(CreateDiskWithGpt(&other_gpt_dev, 512 * block_size_,
                                              // partition size / location is arbitrary
                                              {
                                                  {"vbmeta", Uuid(kVbMetaType), 0x30, 0x1},
                                                  {"frp", Uuid::Generate(), 0x31, 0x1},
                                              }));
    ASSERT_NO_FATAL_FAILURE(WaitForBlockDevices(2 + 2 + 8));

    zx::result status = CreatePartitioner();
    ASSERT_OK(status);
    std::unique_ptr<paver::DevicePartitioner>& partitioner = status.value();

    // Make sure we can find the important partitions.
    EXPECT_OK(
        partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderA, "boot_shim")));
    EXPECT_OK(
        partitioner->FindPartition(PartitionSpec(paver::Partition::kBootloaderB, "boot_shim")));
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconA)));
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kZirconB)));
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kAbrMeta)));
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaA)));
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kVbMetaB)));
    EXPECT_OK(partitioner->FindPartition(PartitionSpec(paver::Partition::kFuchsiaVolumeManager)));
  }
};

TEST_F(AndroidPartitionerTests, InitializeWithoutGptFails) {
  ASSERT_NO_FATAL_FAILURE(InitializeWithoutGptFailsTest());
}

TEST_F(AndroidPartitionerTests, Initialize) { ASSERT_NO_FATAL_FAILURE(InitializeTest()); }

class AndroidPartitionerWithStorageHostTests : public AndroidPartitionerTests {
 protected:
  IsolatedDevmgr::Args BaseDevmgrArgs() override {
    IsolatedDevmgr::Args args;
    args.enable_storage_host = true;
    return args;
  }
};

TEST_F(AndroidPartitionerWithStorageHostTests, InitializeWithoutGptFails) {
  ASSERT_NO_FATAL_FAILURE(InitializeWithoutGptFailsTest());
}

TEST_F(AndroidPartitionerWithStorageHostTests, Initialize) {
  ASSERT_NO_FATAL_FAILURE(InitializeTest());
}

}  // namespace
