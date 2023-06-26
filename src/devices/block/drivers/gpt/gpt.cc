// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gpt.h"

#include <assert.h>
#include <fidl/fuchsia.hardware.gpt.metadata/cpp/wire.h>
#include <inttypes.h>
#include <lib/cksum.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/sync/completion.h>
#include <lib/zx/vmo.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/param.h>
#include <threads.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/syscalls.h>

#include <algorithm>
#include <memory>
#include <mutex>

#include <bind/fuchsia/block/cpp/bind.h>
#include <bind/fuchsia/gpt/cpp/bind.h>
#include <fbl/alloc_checker.h>
#include <safemath/checked_math.h>

#include "lib/ddk/driver.h"
#include "src/devices/block/lib/common/include/common.h"
#include "src/lib/storage/block_client/cpp/block_device.h"
#include "zircon/errors.h"
#include "zircon/status.h"

namespace gpt {

namespace {

constexpr size_t kDeviceNameLength = 40;
struct Guid {
  uint32_t data1;
  uint16_t data2;
  uint16_t data3;
  uint8_t data4[8];
};

template <class Facet>
struct DeletableFacet : Facet {
  template <class... Args>
  explicit DeletableFacet(Args&&... args) : Facet(std::forward<Args>(args)...) {}
};

void uint8_to_guid_string(char* dst, const uint8_t* src) {
  const Guid* guid = reinterpret_cast<const Guid*>(src);
  sprintf(dst, "%08X-%04X-%04X-%02X%02X-%02X%02X%02X%02X%02X%02X", guid->data1, guid->data2,
          guid->data3, guid->data4[0], guid->data4[1], guid->data4[2], guid->data4[3],
          guid->data4[4], guid->data4[5], guid->data4[6], guid->data4[7]);
}

void apply_guid_map(const fidl::VectorView<fuchsia_hardware_gpt_metadata::wire::PartitionInfo>& map,
                    const char* name, uint8_t* type) {
  for (const auto& entry : map) {
    if (entry.name.get() == name && entry.options.has_type_guid_override()) {
      memcpy(type, entry.options.type_guid_override().value.data(), GPT_GUID_LEN);
      return;
    }
  }
}

bool ignore_device(const fidl::VectorView<fuchsia_hardware_gpt_metadata::wire::PartitionInfo>& map,
                   const char* name) {
  for (const auto& entry : map) {
    if (entry.name.get() == name) {
      return entry.options.has_block_driver_should_ignore_device()
                 ? entry.options.block_driver_should_ignore_device()
                 : false;
    }
  }
  return false;
}

}  // namespace

void PartitionDevice::BlockImplQuery(block_info_t* info_out, size_t* block_op_size_out) {
  memcpy(info_out, &info_, sizeof(info_));
  *block_op_size_out = block_op_size_;
}

void PartitionDevice::BlockImplQueue(block_op_t* bop, block_impl_queue_callback completion_cb,
                                     void* cookie) {
  switch (bop->command.opcode) {
    case BLOCK_OPCODE_READ:
    case BLOCK_OPCODE_WRITE: {
      gpt_entry_t* entry = &gpt_entry_;
      size_t max = EntryBlockCount(entry).value();

      if (zx_status_t status = block::CheckIoRange(bop->rw, max); status != ZX_OK) {
        completion_cb(cookie, status, bop);
        return;
      }

      // Adjust for partition starting block
      bop->rw.offset_dev += entry->first;
      break;
    }
    case BLOCK_OPCODE_TRIM: {
      gpt_entry_t* entry = &gpt_entry_;
      size_t max = EntryBlockCount(entry).value();

      if (zx_status_t status = block::CheckIoRange(bop->trim, max); status != ZX_OK) {
        completion_cb(cookie, status, bop);
        return;
      }

      bop->trim.offset_dev += entry->first;
      break;
    }
    case BLOCK_OPCODE_FLUSH:
      break;
    default:
      completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, bop);
      return;
  }

  block_impl_queue(&block_protocol_, bop, completion_cb, cookie);
}

void PartitionDevice::DdkRelease() { delete this; }

static_assert(GPT_GUID_LEN == GUID_LENGTH, "GUID length mismatch");

zx_status_t PartitionDevice::BlockPartitionGetGuid(guidtype_t guid_type, guid_t* out_guid) {
  switch (guid_type) {
    case GUIDTYPE_TYPE:
      memcpy(out_guid, gpt_entry_.type, GPT_GUID_LEN);
      return ZX_OK;
    case GUIDTYPE_INSTANCE:
      memcpy(out_guid, gpt_entry_.guid, GPT_GUID_LEN);
      return ZX_OK;
    default:
      return ZX_ERR_INVALID_ARGS;
  }
}

static_assert(GPT_NAME_LEN <= MAX_PARTITION_NAME_LENGTH, "Partition name length mismatch");

zx_status_t PartitionDevice::BlockPartitionGetName(char* out_name, size_t capacity) {
  return GetPartitionName(gpt_entry_, out_name, capacity).status_value();
}

zx_status_t PartitionDevice::DdkGetProtocol(uint32_t proto_id, void* out) {
  auto* proto = static_cast<ddk::AnyProtocol*>(out);
  switch (proto_id) {
    case ZX_PROTOCOL_BLOCK_IMPL: {
      proto->ops = &block_impl_protocol_ops_;
      proto->ctx = this;
      return ZX_OK;
    }
    case ZX_PROTOCOL_BLOCK_PARTITION: {
      proto->ops = &block_partition_protocol_ops_;
      proto->ctx = this;
      return ZX_OK;
    }
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
}

void PartitionDevice::SetInfo(gpt_entry_t* entry, block_info_t* info, size_t op_size) {
  memcpy(&gpt_entry_, entry, sizeof(gpt_entry_));
  memcpy(&info_, info, sizeof(info_));
  block_op_size_ = op_size;
}

zx_status_t PartitionDevice::Add(uint32_t partition_number, bool ignore_device) {
  char name[kDeviceNameLength];
  snprintf(name, sizeof(name), "part-%03u", partition_number);

  // Previously validated in Bind().
  ZX_ASSERT(GetPartitionName(gpt_entry_, partition_name_, sizeof(partition_name_)).is_ok());
  uint8_to_guid_string(partition_type_guid_, gpt_entry_.type);

  const zx_device_str_prop_t str_props[]{
      {bind_fuchsia_gpt::PARTITIONNAME.c_str(), str_prop_str_val(partition_name_)},
      {bind_fuchsia_gpt::PARTITIONTYPEGUID.c_str(), str_prop_str_val(partition_type_guid_)},
      {bind_fuchsia_block::IGNOREDEVICE.c_str(), str_prop_bool_val(ignore_device)},
  };

  zx_status_t status =
      DdkAdd(ddk::DeviceAddArgs(name).set_str_props({str_props, std::size(str_props)}));
  if (status != ZX_OK) {
    zxlogf(ERROR, "gpt: DdkAdd failed (%d)", status);
  }
  return status;
}

// A thin wrapper around BlockDevice, implemented via block_impl_protocol_t.
// Allows compatibility with GptDevice.
class BlockClient : public block_client::BlockDevice {
 public:
  explicit BlockClient(const block_impl_protocol_t& protocol) {
    protocol_ = protocol;
    block_info_t info;
    protocol_.ops->query(protocol_.ctx, &info, &block_op_size_);
    ZX_ASSERT(block_op_size_ > 0);
  }

  zx::result<std::string> GetTopologicalPath() const override {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  zx::result<> Rebind(std::string_view url_suffix) const override {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  zx_status_t VolumeGetInfo(
      fuchsia_hardware_block_volume::wire::VolumeManagerInfo* out_manager_info,
      fuchsia_hardware_block_volume::wire::VolumeInfo* out_volume_info) const override {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t VolumeQuerySlices(const uint64_t* slices, size_t slices_count,
                                fuchsia_hardware_block_volume::wire::VsliceRange* out_ranges,
                                size_t* out_ranges_count) const override {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t VolumeExtend(uint64_t offset, uint64_t length) override {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t VolumeShrink(uint64_t offset, uint64_t length) override {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t FifoTransaction(block_fifo_request_t* requests, size_t count) override {
    ZX_ASSERT_MSG(count == 1, "Only single-length txns supported");
    auto buf = std::make_unique<uint8_t[]>(block_op_size_);
    auto bop = reinterpret_cast<block_op_t*>(buf.get());
    {
      std::lock_guard guard(lock_);
      const auto& it = vmos_.find(requests->vmoid);
      if (it == vmos_.end()) {
        return ZX_ERR_INVALID_ARGS;
      }

      bop->command.opcode = requests->command.opcode;
      bop->rw.vmo = it->second;
      bop->rw.length = requests->length;
      bop->rw.offset_dev = requests->dev_offset;
      bop->rw.offset_vmo = requests->vmo_offset;
    }

    struct Context {
      sync_completion_t completion;
      zx_status_t status;
    } context;
    block_impl_queue(
        &protocol_, bop,
        [](void* cookie, zx_status_t status, block_op_t* bop) {
          Context* context = static_cast<Context*>(cookie);
          context->status = status;
          sync_completion_signal(&context->completion);
        },
        &context);
    sync_completion_wait(&context.completion, ZX_TIME_INFINITE);
    if (context.status != ZX_OK) {
      zxlogf(ERROR, "gpt: error while interacting with GPT: %s",
             zx_status_get_string(context.status));
      return context.status;
    }
    return ZX_OK;
  }

  zx_status_t BlockGetInfo(fuchsia_hardware_block::wire::BlockInfo* out_info) const override {
    block_info_t info;
    uint64_t block_op_size;
    protocol_.ops->query(protocol_.ctx, &info, &block_op_size);
    out_info->block_size = info.block_size;
    out_info->block_count = info.block_count;
    out_info->max_transfer_size = info.max_transfer_size;
    auto flags = fuchsia_hardware_block::Flag::TryFrom(info.flags);
    if (!flags.has_value()) {
      zxlogf(ERROR, "Bad flags from underlying block device: %u", info.flags);
      return ZX_ERR_IO;
    }
    out_info->flags = *flags;
    return ZX_OK;
  }

  zx_status_t BlockAttachVmo(const zx::vmo& vmo, storage::Vmoid* out_vmoid) final {
    std::lock_guard guard(lock_);
    vmoid_t vmoid = next_vmoid_;
    next_vmoid_ += 1;
    if (next_vmoid_ == VMOID_INVALID) {
      // Skip VMOID_INVALID, which occurs on wrap.
      ++next_vmoid_;
    }
    if (auto it = vmos_.emplace(vmoid, vmo.get()); !it.second) {
      zxlogf(ERROR, "Reused vmoid %u", vmoid);
      return ZX_ERR_NO_RESOURCES;
    }
    *out_vmoid = storage::Vmoid(vmoid);
    return ZX_OK;
  }

  zx_status_t BlockDetachVmo(storage::Vmoid vmoid) final {
    std::lock_guard guard(lock_);
    if (auto erased = vmos_.erase(vmoid.TakeId()); erased != 1) {
      return ZX_ERR_BAD_HANDLE;
    }
    return ZX_OK;
  }

 private:
  block_impl_protocol_t protocol_;
  size_t block_op_size_;
  std::mutex lock_;
  std::map<vmoid_t, zx_handle_t> vmos_ __TA_GUARDED(lock_);
  vmoid_t next_vmoid_ __TA_GUARDED(lock_) = 1;
};

void PartitionManager::DdkRelease() { delete this; }

zx_status_t PartitionManager::Load() {
  block_info_t block_info;
  size_t block_op_size;
  block_protocol_.ops->query(block_protocol_.ctx, &block_info, &block_op_size);

  uint32_t num_partitions = 0;
  for (uint32_t partition = 0; partition < gpt_->EntryCount(); ++partition) {
    zx::result p = gpt_->GetPartition(partition);
    if (!p.is_ok()) {
      continue;
    }
    if (zx_status_t status = AddPartition(block_info, block_op_size, partition); status != ZX_OK) {
      if (status == ZX_ERR_NEXT) {
        continue;
      }
      return status;
    }
    ++num_partitions;
  }
  zxlogf(INFO, "gpt: Loaded %u partitions", num_partitions);

  return ZX_OK;
}

zx_status_t PartitionManager::AddPartition(block_info_t block_info, size_t block_op_size,
                                           uint32_t partition_index) {
  zx::result p = gpt_->GetPartition(partition_index);
  if (p.is_error()) {
    return p.status_value();
  }
  gpt_partition_t* entry = p.value();

  auto result = ValidateEntry(entry);
  ZX_DEBUG_ASSERT(result.is_ok());
  ZX_DEBUG_ASSERT(result.value() == true);

  auto device = std::make_unique<PartitionDevice>(parent(), block_protocol_);

  char partition_guid[GPT_GUID_STRLEN] = {};
  uint8_to_guid_string(partition_guid, entry->guid);

  char partition_name[kMaxUtf8NameLen] = {};
  if (GetPartitionName(*entry, partition_name, sizeof(partition_name)).is_error()) {
    zxlogf(ERROR, "gpt: bad partition name, ignoring entry");
    return ZX_ERR_NEXT;
  }
  bool ignore = false;
  if (metadata_ && (*metadata_)->has_partition_info()) {
    apply_guid_map((*metadata_)->partition_info(), partition_name, entry->type);
    ignore = ignore_device((*metadata_)->partition_info(), partition_name);
  }

  char type_guid[GPT_GUID_STRLEN] = {};
  uint8_to_guid_string(type_guid, entry->type);
  zxlogf(TRACE,
         "gpt: partition=%u type=%s guid=%s name=%s first=0x%" PRIx64 " last=0x%" PRIx64 "\n",
         partition_index, type_guid, partition_guid, partition_name, entry->first, entry->last);

  block_info.block_count = entry->last - entry->first + 1;
  device->SetInfo(entry, &block_info, block_op_size);

  // It would be nicer if we made these devices children of the PartitionManager (i.e. "gpt"
  // device), instead, they are siblings.  Fixing this will require updating a bunch of hard-coded
  // topological paths.
  if (zx_status_t status = device->Add(partition_index, ignore); status != ZX_OK) {
    return status;
  }
  [[maybe_unused]] auto ptr = device.release();
  return ZX_OK;
}

zx_status_t PartitionManager::Bind(void* ctx, zx_device_t* parent) {
  auto metadata = ddk::GetEncodedMetadata<fuchsia_hardware_gpt_metadata::wire::GptInfo>(
      parent, DEVICE_METADATA_GPT_INFO);
  if (!metadata.is_ok() && metadata.status_value() != ZX_ERR_NOT_FOUND) {
    zxlogf(ERROR, "gpt: GetEncodedMetadata failed: %s",
           zx_status_get_string(metadata.status_value()));
    return metadata.status_value();
  }

  block_impl_protocol_t block_protocol;
  if (device_get_protocol(parent, ZX_PROTOCOL_BLOCK, &block_protocol) != ZX_OK) {
    zxlogf(ERROR, "gpt: ERROR: block device parent does not support block protocol");
    return ZX_ERR_NOT_SUPPORTED;
  }

  block_info_t block_info;
  size_t block_op_size;
  block_protocol.ops->query(block_protocol.ctx, &block_info, &block_op_size);

  auto result = MinimumBlocksPerCopy(block_info.block_size);
  if (result.is_error()) {
    zxlogf(ERROR, "gpt: block_size(%u) minimum blocks failed: %d", block_info.block_size,
           result.error());
    return result.error();
  } else if (result.value() > UINT32_MAX) {
    zxlogf(ERROR, "gpt: number of blocks(%lu) required for gpt is too large!", result.value());
    return ZX_ERR_OUT_OF_RANGE;
  }

  auto minimum_device_blocks = MinimumBlockDeviceSize(block_info.block_size);
  if (minimum_device_blocks.is_error()) {
    zxlogf(ERROR, "gpt: failed to get minimum device blocks for block_size(%u)",
           block_info.block_size);
    return minimum_device_blocks.error();
  }

  if (block_info.block_count <= minimum_device_blocks.value()) {
    zxlogf(ERROR, "gpt: block device too small to hold GPT required:%lu found:%lu",
           minimum_device_blocks.value(), block_info.block_count);
    return ZX_ERR_NO_SPACE;
  }

  // sanity check the default txn size with the block size
  if ((kMaxPartitionTableSize % block_info.block_size) ||
      (kMaxPartitionTableSize < block_info.block_size)) {
    zxlogf(ERROR, "gpt: default txn size=%lu is not aligned to blksize=%u!", kMaxPartitionTableSize,
           block_info.block_size);
    return ZX_ERR_BAD_STATE;
  }

  auto client = std::make_unique<BlockClient>(block_protocol);
  zx::result gpt_result =
      GptDevice::Create(std::move(client), block_info.block_size, block_info.block_count);
  if (gpt_result.is_error()) {
    zxlogf(ERROR, "gpt: failed to load gpt- %s", HeaderStatusToCString(gpt_result.error_value()));
    return gpt_result.error_value();
  }
  std::unique_ptr<GptDevice>& gpt = gpt_result.value();

  zxlogf(TRACE, "gpt: found gpt header");

  std::optional<ddk::DecodedMetadata<fuchsia_hardware_gpt_metadata::wire::GptInfo>> metadata_opt;
  if (metadata.is_ok()) {
    metadata_opt = std::move(*metadata);
  }
  auto manager = std::make_unique<PartitionManager>(std::move(gpt), std::move(metadata_opt),
                                                    std::move(block_protocol), parent);
  if (zx_status_t status =
          manager->DdkAdd(ddk::DeviceAddArgs("gpt").set_flags(DEVICE_ADD_NON_BINDABLE));
      status != ZX_OK) {
    zxlogf(ERROR, "gpt: failed to DdkAdd: %s", zx_status_get_string(status));
    return status;
  }
  {
    std::lock_guard guard(manager->lock_);
    if (zx_status_t status = manager->Load(); status != ZX_OK) {
      zxlogf(ERROR, "gpt: failed to load partition tables: %s", zx_status_get_string(status));
      return status;
    }
  }
  // The PartitionManager object is owned by the DDK, now that it has been
  // added. It will be deleted when the device is released.
  [[maybe_unused]] auto ptr = manager.release();
  return ZX_OK;
}

void PartitionManager::GetInfoLocked(fuchsia_hardware_block_volume::wire::VolumeManagerInfo* info) {
  info->slice_count = gpt_->TotalBlockCount();
  info->slice_size = gpt_->BlockSize();
  info->assigned_slice_count = 0;
  info->maximum_slice_count = 0;
  info->max_virtual_slice = 0;
}

zx::result<uint32_t> PartitionManager::AllocatePartitionLocked(
    uint64_t slice_count, const fuchsia_hardware_block_partition::wire::Guid& type,
    const fuchsia_hardware_block_partition::wire::Guid& instance, fidl::StringView name) {
  uint64_t offset;
  zx_status_t status = gpt_->FindFreeRange(slice_count, &offset);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  std::string name_str(name.cbegin(), name.cend());
  zx::result partition_index = gpt_->AddPartition(name_str.c_str(), type.value.data(),
                                                  instance.value.data(), offset, slice_count, 0);
  if (partition_index.is_error()) {
    zxlogf(ERROR, "Failed to add GPT partition: %s", partition_index.status_string());
    return partition_index.take_error();
  }

  if (status = gpt_->Sync(); status != ZX_OK) {
    zxlogf(ERROR, "Failed to sync GPT: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(*partition_index);
}

void PartitionManager::AllocatePartition(AllocatePartitionRequestView request,
                                         AllocatePartitionCompleter::Sync& completer) {
  zxlogf(INFO, "AllocatePartition %lu blocks", request->slice_count);
  zx_status_t status;
  {
    std::lock_guard guard(lock_);
    zx::result<uint32_t> partition_index = AllocatePartitionLocked(
        request->slice_count, request->type, request->instance, request->name);
    status = partition_index.status_value();
    if (partition_index.is_ok()) {
      block_info_t block_info;
      size_t block_op_size;
      block_protocol_.ops->query(block_protocol_.ctx, &block_info, &block_op_size);
      if (status = AddPartition(block_info, block_op_size, *partition_index); status != ZX_OK) {
        if (status == ZX_ERR_NEXT) {
          status = ZX_ERR_BAD_STATE;
        }
        zxlogf(ERROR, "Failed to add partition: %s", zx_status_get_string(status));
      } else {
        zxlogf(INFO, "Added partition, id %u", *partition_index);
      }
    } else {
      zxlogf(ERROR, "Failed to allocate partition: %s", zx_status_get_string(status));
    }
  }
  completer.Reply(status);
}

void PartitionManager::GetInfo(GetInfoCompleter::Sync& completer) {
  fidl::Arena allocator;
  fidl::ObjectView<fuchsia_hardware_block_volume::wire::VolumeManagerInfo> info(allocator);
  {
    std::lock_guard guard(lock_);
    GetInfoLocked(info.get());
  }
  completer.Reply(ZX_OK, info);
}

void PartitionManager::Activate(ActivateRequestView request, ActivateCompleter::Sync& completer) {
  completer.Reply(ZX_ERR_NOT_SUPPORTED);
}
void PartitionManager::GetPartitionLimit(GetPartitionLimitRequestView request,
                                         GetPartitionLimitCompleter::Sync& completer) {
  completer.Reply(ZX_ERR_NOT_SUPPORTED, 0);
}
void PartitionManager::SetPartitionLimit(SetPartitionLimitRequestView request,
                                         SetPartitionLimitCompleter::Sync& completer) {
  completer.Reply(ZX_ERR_NOT_SUPPORTED);
}
void PartitionManager::SetPartitionName(SetPartitionNameRequestView request,
                                        SetPartitionNameCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

constexpr zx_driver_ops_t gpt_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = PartitionManager::Bind;
  return ops;
}();

}  // namespace gpt

ZIRCON_DRIVER(gpt, gpt::gpt_driver_ops, "zircon", "0.1");
