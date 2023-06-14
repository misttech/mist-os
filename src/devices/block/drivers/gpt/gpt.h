// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_GPT_GPT_H_
#define SRC_DEVICES_BLOCK_DRIVERS_GPT_GPT_H_

#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fidl/fuchsia.hardware.gpt.metadata/cpp/wire.h>
#include <fuchsia/hardware/block/driver/c/banjo.h>
#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <fuchsia/hardware/block/partition/c/banjo.h>
#include <fuchsia/hardware/block/partition/cpp/banjo.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/sync/completion.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <ddktl/device.h>
#include <fbl/macros.h>
#include <gpt/c/gpt.h>
#include <gpt/gpt.h>

namespace gpt {

class PartitionDevice;
using DeviceType = ddk::Device<PartitionDevice, ddk::GetProtocolable>;

class PartitionDevice : public DeviceType,
                        public ddk::BlockImplProtocol<PartitionDevice, ddk::base_protocol>,
                        public ddk::BlockPartitionProtocol<PartitionDevice> {
 public:
  PartitionDevice(zx_device_t* parent, const block_impl_protocol_t& proto) : DeviceType(parent) {
    memcpy(&block_protocol_, &proto, sizeof(block_protocol_));
  }

  DISALLOW_COPY_ASSIGN_AND_MOVE(PartitionDevice);

  void SetInfo(gpt_entry_t* entry, block_info_t* info, size_t op_size);

  // Add device to devhost device list. Once added, the device cannot be deleted directly,
  // AsyncRemove() must be called to schedule an Unbind() and Release().
  zx_status_t Add(uint32_t partition_number, bool ignore_device);

  // Block protocol implementation.
  void BlockImplQuery(block_info_t* info_out, size_t* block_op_size_out);
  void BlockImplQueue(block_op_t* bop, block_impl_queue_callback completion_cb, void* cookie);

  // Device protocol.
  void DdkRelease();
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out);

  // Partition protocol implementation.
  zx_status_t BlockPartitionGetGuid(guidtype_t guid_type, guid_t* out_guid);
  zx_status_t BlockPartitionGetName(char* out_name, size_t capacity);

 private:
  size_t block_op_size_ = 0;
  block_impl_protocol_t block_protocol_{};
  gpt_entry_t gpt_entry_{};
  block_info_t info_{};
  char partition_name_[kMaxUtf8NameLen]{};
  char partition_type_guid_[GPT_GUID_STRLEN]{};
};

class PartitionManager;
using ManagerDeviceType =
    ddk::Device<PartitionManager,
                ddk::Messageable<fuchsia_hardware_block_volume::VolumeManager>::Mixin>;

class PartitionManager : public ManagerDeviceType {
 public:
  PartitionManager(
      std::unique_ptr<GptDevice> gpt,
      std::optional<ddk::DecodedMetadata<fuchsia_hardware_gpt_metadata::wire::GptInfo>> metadata,
      const block_impl_protocol_t& protocol, zx_device_t* parent)
      : ManagerDeviceType(parent), metadata_(std::move(metadata)), gpt_(std::move(gpt)) {
    memcpy(&block_protocol_, &protocol, sizeof(protocol));
  }

  // Device bind() interface.
  static zx_status_t Bind(void* ctx, zx_device_t* parent);

  // Add device to devhost device list. Once added, the device cannot be deleted directly,
  // AsyncRemove() must be called to schedule an Unbind() and Release().
  zx_status_t Add();

  // DDK methods
  void DdkRelease();

  // VolumeManager protocol
  void AllocatePartition(AllocatePartitionRequestView request,
                         AllocatePartitionCompleter::Sync& completer) override;
  void GetInfo(GetInfoCompleter::Sync& completer) override;
  void Activate(ActivateRequestView request, ActivateCompleter::Sync& completer) override;
  void GetPartitionLimit(GetPartitionLimitRequestView request,
                         GetPartitionLimitCompleter::Sync& completer) override;
  void SetPartitionLimit(SetPartitionLimitRequestView request,
                         SetPartitionLimitCompleter::Sync& completer) override;
  void SetPartitionName(SetPartitionNameRequestView request,
                        SetPartitionNameCompleter::Sync& completer) override;

 private:
  PartitionManager(PartitionManager&&) = delete;
  PartitionManager(const PartitionManager&) = delete;
  PartitionManager& operator=(PartitionManager&&) = delete;
  PartitionManager& operator=(const PartitionManager&) = delete;

  zx_status_t Load() __TA_REQUIRES(lock_);
  // Binds a partition device for the given entry.  It's assumed that the entry has been validated.
  zx_status_t AddPartition(block_info_t block_info, size_t block_op_size, uint32_t partition_index)
      __TA_REQUIRES(lock_);

  void GetInfoLocked(fuchsia_hardware_block_volume::wire::VolumeManagerInfo* info)
      __TA_REQUIRES(lock_);

  // Returns the newly created partition number.
  zx::result<uint32_t> AllocatePartitionLocked(
      uint64_t slice_count, const fuchsia_hardware_block_partition::wire::Guid& type,
      const fuchsia_hardware_block_partition::wire::Guid& instance, fidl::StringView name)
      __TA_REQUIRES(lock_);

  block_impl_protocol_t block_protocol_;
  std::optional<ddk::DecodedMetadata<fuchsia_hardware_gpt_metadata::wire::GptInfo>> metadata_;
  std::mutex lock_;
  std::unique_ptr<GptDevice> gpt_ __TA_GUARDED(lock_);
};

}  // namespace gpt

#endif  // SRC_DEVICES_BLOCK_DRIVERS_GPT_GPT_H_
