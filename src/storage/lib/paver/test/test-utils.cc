// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/test/test-utils.h"

#include <fidl/fuchsia.device/cpp/wire.h>
#include <lib/component/incoming/cpp/clone.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/directory.h>
#include <lib/zbi-format/partition.h>
#include <lib/zx/vmo.h>
#include <limits.h>

#include <memory>
#include <optional>
#include <string_view>

#include <fbl/string.h>
#include <fbl/vector.h>
#include <gpt/gpt.h>
#include <zxtest/zxtest.h>

#include "src/lib/uuid/uuid.h"
#include "src/storage/lib/block_client/cpp/fake_block_device.h"

namespace {

void CreateBadBlockMap(void* buffer) {
  // Set all entries in first BBT to be good blocks.
  constexpr uint8_t kBlockGood = 0;
  memset(buffer, kBlockGood, kPageSize);

  struct OobMetadata {
    uint32_t magic;
    int16_t program_erase_cycles;
    uint16_t generation;
  };

  constexpr size_t oob_offset{static_cast<size_t>(kPageSize) * kPagesPerBlock * kNumBlocks};
  auto* oob = reinterpret_cast<OobMetadata*>(reinterpret_cast<uintptr_t>(buffer) + oob_offset);
  oob->magic = 0x7462626E;  // "nbbt"
  oob->program_erase_cycles = 0;
  oob->generation = 1;
}

}  // namespace

zx::result<DeviceAndController> GetNewConnections(
    fidl::UnownedClientEnd<fuchsia_device::Controller> controller) {
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_device::Controller>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  if (fidl::OneWayError response =
          fidl::WireCall(controller)->ConnectToController(std::move(endpoints->server));
      !response.ok()) {
    return zx::error(response.status());
  }
  zx::result device_endpoints = fidl::CreateEndpoints<fuchsia_device::Controller>();
  if (device_endpoints.is_error()) {
    return device_endpoints.take_error();
  }
  if (fidl::OneWayError response =
          fidl::WireCall(controller)->ConnectToDeviceFidl(device_endpoints->server.TakeChannel());
      !response.ok()) {
    return zx::error(response.status());
  }
  return zx::ok(DeviceAndController{
      .device = device_endpoints->client.TakeChannel(),
      .controller = std::move(endpoints->client),
  });
}

void BlockDevice::Create(const fbl::unique_fd& devfs_root, const uint8_t* guid,
                         std::unique_ptr<BlockDevice>* device) {
  ramdisk_client_t* client;
  ASSERT_OK(ramdisk_create_at_with_guid(devfs_root.get(), kBlockSize, kBlockCount, guid,
                                        ZBI_PARTITION_GUID_LEN, &client));
  device->reset(new BlockDevice(client, kBlockCount, kBlockSize));
}

void BlockDevice::Create(const fbl::unique_fd& devfs_root, const uint8_t* guid,
                         uint64_t block_count, std::unique_ptr<BlockDevice>* device) {
  ramdisk_client_t* client;
  ASSERT_OK(ramdisk_create_at_with_guid(devfs_root.get(), kBlockSize, block_count, guid,
                                        ZBI_PARTITION_GUID_LEN, &client));
  device->reset(new BlockDevice(client, block_count, kBlockSize));
}

void BlockDevice::Create(const fbl::unique_fd& devfs_root, const uint8_t* guid,
                         uint64_t block_count, uint32_t block_size,
                         std::unique_ptr<BlockDevice>* device) {
  ramdisk_client_t* client;
  ASSERT_OK(ramdisk_create_at_with_guid(devfs_root.get(), block_size, block_count, guid,
                                        ZBI_PARTITION_GUID_LEN, &client));
  device->reset(new BlockDevice(client, block_count, block_size));
}

void BlockDevice::CreateFromVmo(const fbl::unique_fd& devfs_root, const uint8_t* guid, zx::vmo vmo,
                                uint32_t block_size, std::unique_ptr<BlockDevice>* device) {
  ramdisk_client_t* client;
  uint64_t block_count;
  ASSERT_OK(vmo.get_size(&block_count));
  block_count /= block_size;
  ASSERT_OK(ramdisk_create_at_from_vmo_with_params(devfs_root.get(), vmo.release(), block_size,
                                                   guid, ZBI_PARTITION_GUID_LEN, &client));
  device->reset(new BlockDevice(client, block_count, block_size));
}

void BlockDevice::CreateWithGpt(const fbl::unique_fd& devfs_root, uint64_t block_count,
                                uint32_t block_size,
                                const std::vector<PartitionDescription>& init_partitions,
                                std::unique_ptr<BlockDevice>* device) {
  auto dev = std::make_unique<block_client::FakeBlockDevice>(block_count, block_size);
  zx::result contents = dev->VmoChildReference();
  ASSERT_OK(contents);
  zx::result gpt_result = gpt::GptDevice::Create(std::move(dev), block_size, block_count);
  ASSERT_OK(gpt_result);
  std::unique_ptr<gpt::GptDevice> gpt = std::move(*gpt_result);
  ASSERT_OK(gpt->Sync());

  for (auto& part : init_partitions) {
    uuid::Uuid instance = part.instance.value_or(uuid::Uuid::Generate());
    ASSERT_OK(gpt->AddPartition(part.name.c_str(), part.type.bytes(), instance.bytes(), part.start,
                                part.length, 0),
              "%s", part.name.c_str());
  }
  ASSERT_OK(gpt->Sync());

  constexpr const uint8_t kEmptyGuid[ZBI_PARTITION_GUID_LEN] = {0};
  ASSERT_NO_FATAL_FAILURE(
      CreateFromVmo(devfs_root, kEmptyGuid, std::move(*contents), block_size, device));
}

void BlockDevice::Read(const zx::vmo& vmo, size_t size, size_t dev_offset,
                       size_t vmo_offset) const {
  auto block_client = paver::BlockPartitionClient::Create(
      std::make_unique<paver::DevfsVolumeConnector>(ConnectToController()));
  ASSERT_OK(block_client);
  ASSERT_OK(block_client->Read(vmo, size, dev_offset, vmo_offset));
}

void SkipBlockDevice::Create(fbl::unique_fd devfs_root,
                             fuchsia_hardware_nand::wire::RamNandInfo nand_info,
                             std::unique_ptr<SkipBlockDevice>* device) {
  fzl::VmoMapper mapper;
  zx::vmo vmo;
  ASSERT_OK(
      mapper.CreateAndMap(static_cast<size_t>(kPageSize + kOobSize) * kPagesPerBlock * kNumBlocks,
                          ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr, &vmo));
  memset(mapper.start(), 0xff, mapper.size());
  CreateBadBlockMap(mapper.start());
  vmo.op_range(ZX_VMO_OP_CACHE_CLEAN_INVALIDATE, 0, mapper.size(), nullptr, 0);
  ASSERT_OK(vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &nand_info.vmo));

  std::unique_ptr<ramdevice_client_test::RamNandCtl> ctl;
  ASSERT_OK(ramdevice_client_test::RamNandCtl::Create(std::move(devfs_root), &ctl));
  std::optional<ramdevice_client::RamNand> ram_nand;
  ASSERT_OK(ctl->CreateRamNand(std::move(nand_info), &ram_nand));
  device->reset(new SkipBlockDevice(std::move(ctl), *std::move(ram_nand), std::move(mapper)));
}

FakePartitionClient::FakePartitionClient(size_t block_count, size_t block_size)
    : block_size_(block_size) {
  partition_size_ = block_count * block_size;
  zx_status_t status = zx::vmo::create(partition_size_, ZX_VMO_RESIZABLE, &partition_);
  if (status != ZX_OK) {
    partition_size_ = 0;
  }
}

FakePartitionClient::FakePartitionClient(size_t block_count)
    : FakePartitionClient(block_count, zx_system_get_page_size()) {}

zx::result<size_t> FakePartitionClient::GetBlockSize() { return zx::ok(block_size_); }

zx::result<size_t> FakePartitionClient::GetPartitionSize() { return zx::ok(partition_size_); }

zx::result<> FakePartitionClient::Read(const zx::vmo& vmo, size_t size) {
  if (partition_size_ == 0) {
    return zx::ok();
  }

  fzl::VmoMapper mapper;
  if (auto status = mapper.Map(vmo, 0, size, ZX_VM_PERM_WRITE); status != ZX_OK) {
    return zx::error(status);
  }
  return zx::make_result(partition_.read(mapper.start(), 0, size));
}

zx::result<> FakePartitionClient::Write(const zx::vmo& vmo, size_t size) {
  if (size > partition_size_) {
    size_t new_size = fbl::round_up(size, block_size_);
    zx_status_t status = partition_.set_size(new_size);
    if (status != ZX_OK) {
      return zx::error(status);
    }
    partition_size_ = new_size;
  }

  fzl::VmoMapper mapper;
  if (auto status = mapper.Map(vmo, 0, size, ZX_VM_PERM_READ | ZX_VM_ALLOW_FAULTS);
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::make_result(partition_.write(mapper.start(), 0, size));
}

zx::result<> FakePartitionClient::Trim() {
  zx_status_t status = partition_.set_size(0);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  partition_size_ = 0;
  return zx::ok();
}

zx::result<> FakePartitionClient::Flush() { return zx::ok(); }

FakeBootArgs::FakeBootArgs(std::string slot_suffix) {
  AddStringArgs("zvb.current_slot", std::move(slot_suffix));
}

void FakeBootArgs::GetString(GetStringRequestView request, GetStringCompleter::Sync& completer) {
  auto iter = string_args_.find(std::string(request->key.data(), request->key.size()));
  if (iter != string_args_.end()) {
    completer.Reply(fidl::StringView::FromExternal(iter->second));
  } else {
    completer.Reply({});
  }
}

void FakeBootArgs::GetStrings(GetStringsRequestView request, GetStringsCompleter::Sync& completer) {
  std::vector<fidl::StringView> response;
  for (const auto& key : request->keys) {
    auto iter = string_args_.find(std::string(key.data(), key.size()));
    if (iter != string_args_.end()) {
      response.push_back(fidl::StringView::FromExternal(iter->second));
    }
  }
  response.push_back(fidl::StringView());
  completer.Reply(fidl::VectorView<fidl::StringView>::FromExternal(response));
}

void FakeBootArgs::GetBool(GetBoolRequestView request, GetBoolCompleter::Sync& completer) {
  if (strncmp(request->key.data(), "astro.sysconfig.abr-wear-leveling",
              sizeof("astro.sysconfig.abr-wear-leveling")) == 0) {
    completer.Reply(astro_sysconfig_abr_wear_leveling_);
  } else {
    completer.Reply(request->defaultval);
  }
}
void FakeBootArgs::GetBools(GetBoolsRequestView request, GetBoolsCompleter::Sync& completer) {}
void FakeBootArgs::Collect(CollectRequestView request, CollectCompleter::Sync& completer) {}
