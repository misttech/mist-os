// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_PAVER_TEST_TEST_UTILS_H_
#define SRC_STORAGE_LIB_PAVER_TEST_TEST_UTILS_H_

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fidl/fuchsia.sysinfo/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/directory.h>
#include <lib/fzl/vmo-mapper.h>

#include <memory>

#include <fbl/ref_ptr.h>
#include <fbl/unique_fd.h>
#include <ramdevice-client-test/ramnandctl.h>
#include <ramdevice-client/ramdisk.h>
#include <ramdevice-client/ramnand.h>
#include <zxtest/zxtest.h>

#include "src/storage/lib/paver/abr-client.h"
#include "src/storage/lib/paver/astro.h"
#include "src/storage/lib/paver/device-partitioner.h"
#include "src/storage/lib/paver/luis.h"
#include "src/storage/lib/paver/moonflower.h"
#include "src/storage/lib/paver/nelson.h"
#include "src/storage/lib/paver/sherlock.h"
#include "src/storage/lib/paver/uefi.h"
#include "src/storage/lib/paver/vim3.h"
#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/service.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"

constexpr uint64_t kBlockSize = 0x1000;
constexpr uint32_t kBlockCount = 0x100;
constexpr uint64_t kGptBlockCount = 2048;

constexpr uint32_t kOobSize = 8;
constexpr uint32_t kPageSize = 2048;
constexpr uint32_t kPagesPerBlock = 128;
constexpr uint32_t kSkipBlockSize = kPageSize * kPagesPerBlock;
constexpr uint32_t kNumBlocks = 400;

class PaverTest : public zxtest::Test {
 protected:
  void SetUp() override {
    paver::DevicePartitionerFactory::Register(std::make_unique<paver::AstroPartitionerFactory>());
    paver::DevicePartitionerFactory::Register(std::make_unique<paver::NelsonPartitionerFactory>());
    paver::DevicePartitionerFactory::Register(
        std::make_unique<paver::SherlockPartitionerFactory>());
    paver::DevicePartitionerFactory::Register(
        std::make_unique<paver::MoonflowerPartitionerFactory>());
    paver::DevicePartitionerFactory::Register(std::make_unique<paver::LuisPartitionerFactory>());
    paver::DevicePartitionerFactory::Register(std::make_unique<paver::Vim3PartitionerFactory>());
    paver::DevicePartitionerFactory::Register(std::make_unique<paver::UefiPartitionerFactory>());
    paver::DevicePartitionerFactory::Register(std::make_unique<paver::DefaultPartitionerFactory>());
  }
};

struct DeviceAndController {
  zx::channel device;
  fidl::ClientEnd<fuchsia_device::Controller> controller;
};

zx::result<DeviceAndController> GetNewConnections(
    fidl::UnownedClientEnd<fuchsia_device::Controller> controller);

struct PartitionDescription {
  std::string name;
  uuid::Uuid type;
  uint64_t start;
  uint64_t length;
  // Instance is last since it is often elided.  If unset, a generated instance is used.
  std::optional<uuid::Uuid> instance;
};

class BlockDevice {
 public:
  static void Create(const fbl::unique_fd& devfs_root, const uint8_t* guid,
                     std::unique_ptr<BlockDevice>* device);

  static void Create(const fbl::unique_fd& devfs_root, const uint8_t* guid, uint64_t block_count,
                     std::unique_ptr<BlockDevice>* device);

  static void Create(const fbl::unique_fd& devfs_root, const uint8_t* guid, uint64_t block_count,
                     uint32_t block_size, std::unique_ptr<BlockDevice>* device);

  static void CreateFromVmo(const fbl::unique_fd& devfs_root, const uint8_t* guid, zx::vmo vmo,
                            uint32_t block_size, std::unique_ptr<BlockDevice>* device);

  static void CreateWithGpt(const fbl::unique_fd& devfs_root, uint64_t block_count,
                            uint32_t block_size,
                            const std::vector<PartitionDescription>& init_partitions,
                            std::unique_ptr<BlockDevice>* device);

  ~BlockDevice() { ramdisk_destroy(client_); }

  fidl::UnownedClientEnd<fuchsia_hardware_block::Block> block_interface() const {
    return fidl::UnownedClientEnd<fuchsia_hardware_block::Block>(
        ramdisk_get_block_interface(client_));
  }

  fidl::UnownedClientEnd<fuchsia_hardware_block_volume::Volume> volume_interface() const {
    return fidl::UnownedClientEnd<fuchsia_hardware_block_volume::Volume>(
        ramdisk_get_block_interface(client_));
  }

  fidl::UnownedClientEnd<fuchsia_device::Controller> block_controller_interface() const {
    return fidl::UnownedClientEnd<fuchsia_device::Controller>(
        ramdisk_get_block_controller_interface(client_));
  }

  fidl::ClientEnd<fuchsia_device::Controller> ConnectToController() const {
    fidl::ClientEnd<fuchsia_device::Controller> controller;
    zx::result controller_server = fidl::CreateEndpoints(&controller);
    ZX_ASSERT(controller_server.is_ok());
    fidl::OneWayStatus status = fidl::WireCall(block_controller_interface())
                                    ->ConnectToController(std::move(*controller_server));
    ZX_ASSERT(status.status() == ZX_OK);
    return controller;
  }

  // Block count and block size of this device.
  uint64_t block_count() const { return block_count_; }
  uint32_t block_size() const { return block_size_; }

  void Read(const zx::vmo& vmo, size_t size, size_t dev_offset, size_t vmo_offset) const;

 private:
  BlockDevice(ramdisk_client_t* client, uint64_t block_count, uint32_t block_size)
      : client_(client), block_count_(block_count), block_size_(block_size) {}

  ramdisk_client_t* client_;
  const uint64_t block_count_;
  const uint32_t block_size_;
};

class SkipBlockDevice {
 public:
  static void Create(fbl::unique_fd devfs_root, fuchsia_hardware_nand::wire::RamNandInfo nand_info,
                     std::unique_ptr<SkipBlockDevice>* device);

  fbl::unique_fd devfs_root() { return ctl_->devfs_root().duplicate(); }

  fzl::VmoMapper& mapper() { return mapper_; }

  ~SkipBlockDevice() = default;

 private:
  SkipBlockDevice(std::unique_ptr<ramdevice_client_test::RamNandCtl> ctl,
                  ramdevice_client::RamNand ram_nand, fzl::VmoMapper mapper)
      : ctl_(std::move(ctl)), ram_nand_(std::move(ram_nand)), mapper_(std::move(mapper)) {}

  std::unique_ptr<ramdevice_client_test::RamNandCtl> ctl_;
  ramdevice_client::RamNand ram_nand_;
  fzl::VmoMapper mapper_;
};

// Dummy DevicePartition implementation meant to be used for testing. All functions are no-ops, i.e.
// they silently pass without doing anything. Tests can inherit from this class and override
// functions that are relevant for their test cases; this class provides an easy way to inherit from
// DevicePartitioner which is an abstract class.
class FakeDevicePartitioner : public paver::DevicePartitioner {
 public:
  zx::result<std::unique_ptr<abr::Client>> CreateAbrClient() const override { ZX_ASSERT(false); }

  const paver::BlockDevices& Devices() const override { ZX_ASSERT(false); }

  fidl::UnownedClientEnd<fuchsia_io::Directory> SvcRoot() const override { ZX_ASSERT(false); }

  bool IsFvmWithinFtl() const override { return false; }

  bool SupportsPartition(const paver::PartitionSpec& spec) const override { return true; }

  zx::result<std::unique_ptr<paver::PartitionClient>> FindPartition(
      const paver::PartitionSpec& spec) const override {
    return zx::ok(nullptr);
  }

  zx::result<> WipeFvm() const override { return zx::ok(); }

  zx::result<> ResetPartitionTables() const override { return zx::ok(); }

  zx::result<> ValidatePayload(const paver::PartitionSpec& spec,
                               std::span<const uint8_t> data) const override {
    return zx::ok();
  }
};

// Defines a PartitionClient that reads and writes to a partition backed by a VMO in memory.
// Used for testing.
class FakePartitionClient : public paver::PartitionClient {
 public:
  FakePartitionClient(size_t block_count, size_t block_size);
  explicit FakePartitionClient(size_t block_count);

  zx::result<size_t> GetBlockSize() override;
  zx::result<size_t> GetPartitionSize() override;
  zx::result<> Read(const zx::vmo& vmo, size_t size) override;
  zx::result<> Write(const zx::vmo& vmo, size_t vmo_size) override;
  zx::result<> Trim() override;
  zx::result<> Flush() override;

 protected:
  zx::vmo partition_;
  size_t block_size_;
  size_t partition_size_;
};

class FakeBootArgs : public fidl::WireServer<fuchsia_boot::Arguments> {
 public:
  explicit FakeBootArgs(std::string slot_suffix = "-a");

  void GetString(GetStringRequestView request, GetStringCompleter::Sync& completer) override;
  void GetStrings(GetStringsRequestView request, GetStringsCompleter::Sync& completer) override;
  void GetBool(GetBoolRequestView request, GetBoolCompleter::Sync& completer) override;
  void GetBools(GetBoolsRequestView request, GetBoolsCompleter::Sync& completer) override;
  void Collect(CollectRequestView request, CollectCompleter::Sync& completer) override;

  void SetAstroSysConfigAbrWearLeveling(bool value) { astro_sysconfig_abr_wear_leveling_ = value; }
  void AddStringArgs(std::string key, std::string value) {
    string_args_[std::move(key)] = std::move(value);
  }

 private:
  fidl::ServerBindingGroup<fuchsia_boot::Arguments> bindings_;

  bool astro_sysconfig_abr_wear_leveling_ = false;
  std::unordered_map<std::string, std::string> string_args_;
};

#endif  // SRC_STORAGE_LIB_PAVER_TEST_TEST_UTILS_H_
