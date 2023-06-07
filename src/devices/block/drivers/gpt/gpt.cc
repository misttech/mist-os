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
#include <zircon/syscalls.h>

#include <algorithm>
#include <memory>

#include <bind/fuchsia/block/cpp/bind.h>
#include <bind/fuchsia/gpt/cpp/bind.h>
#include <fbl/alloc_checker.h>

#include "lib/ddk/driver.h"
#include "src/devices/block/lib/common/include/common.h"
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

class PlaceholderDevice;
using PlaceholderDeviceType = ddk::Device<PlaceholderDevice>;

class PlaceholderDevice : public PlaceholderDeviceType {
 public:
  explicit PlaceholderDevice(zx_device_t* parent) : PlaceholderDeviceType(parent) {}
  void DdkRelease() { delete this; }
};

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

void gpt_read_sync_complete(void* cookie, zx_status_t status, block_op_t* bop) {
  // Pass 32bit status back to caller via 32bit command field
  // Saves from needing custom structs, etc.
  bop->command.flags = status;
  sync_completion_signal(static_cast<sync_completion_t*>(cookie));
}

zx_status_t ReadBlocks(block_impl_protocol_t* block_protocol, size_t block_op_size,
                       const block_info_t& block_info, uint32_t block_count, uint64_t block_offset,
                       uint8_t* out_buffer) {
  zx_status_t status;
  sync_completion_t completion;
  std::unique_ptr<uint8_t[]> bop_buffer(new uint8_t[block_op_size]);
  block_op_t* bop = reinterpret_cast<block_op_t*>(bop_buffer.get());
  zx::vmo vmo;
  size_t vmo_size = static_cast<size_t>(block_count) * block_info.block_size;
  if ((status = zx::vmo::create(vmo_size, 0, &vmo)) != ZX_OK) {
    zxlogf(ERROR, "gpt: VMO create failed(%d)", status);
    return status;
  }

  bop->command = {.opcode = BLOCK_OPCODE_READ, .flags = 0};
  bop->rw.vmo = vmo.get();
  bop->rw.length = block_count;
  bop->rw.offset_dev = block_offset;
  bop->rw.offset_vmo = 0;

  block_protocol->ops->queue(block_protocol->ctx, bop, gpt_read_sync_complete, &completion);
  sync_completion_wait(&completion, ZX_TIME_INFINITE);
  if (bop->command.flags != ZX_OK) {
    zxlogf(ERROR, "gpt: error %d reading GPT", bop->command.flags);
    return static_cast<zx_status_t>(bop->command.flags);
  }

  return vmo.read(out_buffer, 0, vmo_size);
}

zx_status_t Bind(void* ctx, zx_device_t* parent) {
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

  uint32_t gpt_block_count = static_cast<uint32_t>(result.value());
  size_t gpt_buffer_size = static_cast<size_t>(gpt_block_count) * block_info.block_size;
  std::unique_ptr<uint8_t[]> buffer(new uint8_t[gpt_buffer_size]);
  std::unique_ptr<GptDevice> gpt;

  // sanity check the default txn size with the block size
  if ((kMaxPartitionTableSize % block_info.block_size) ||
      (kMaxPartitionTableSize < block_info.block_size)) {
    zxlogf(ERROR, "gpt: default txn size=%lu is not aligned to blksize=%u!", kMaxPartitionTableSize,
           block_info.block_size);
    return ZX_ERR_BAD_STATE;
  }

  zx_status_t status =
      ReadBlocks(&block_protocol, block_op_size, block_info, gpt_block_count, 1, buffer.get());
  if (status != ZX_OK) {
    return status;
  }

  if ((status = GptDevice::Load(buffer.get(), gpt_buffer_size, block_info.block_size,
                                block_info.block_count, &gpt)) != ZX_OK) {
    zxlogf(ERROR, "gpt: failed to load gpt- %s", HeaderStatusToCString(status));
    return status;
  }

  zxlogf(TRACE, "gpt: found gpt header");

  bool has_partition = false;
  unsigned int partitions;
  for (partitions = 0; partitions < gpt->EntryCount(); partitions++) {
    zx::result<gpt_partition_t*> p = gpt->GetPartition(partitions);
    if (!p.is_ok()) {
      continue;
    }
    gpt_partition_t* entry = p.value();
    has_partition = true;

    auto result = ValidateEntry(entry);
    ZX_DEBUG_ASSERT(result.is_ok());
    ZX_DEBUG_ASSERT(result.value() == true);

    fbl::AllocChecker ac;
    std::unique_ptr<PartitionDevice> device(new (&ac) PartitionDevice(parent, &block_protocol));
    if (!ac.check()) {
      zxlogf(ERROR, "gpt: out of memory");
      return ZX_ERR_NO_MEMORY;
    }

    char partition_guid[GPT_GUID_STRLEN] = {};
    uint8_to_guid_string(partition_guid, entry->guid);

    char partition_name[kMaxUtf8NameLen] = {};
    if (GetPartitionName(*entry, partition_name, sizeof(partition_name)).is_error()) {
      zxlogf(ERROR, "gpt: bad partition name, ignoring entry");
      continue;
    }
    bool ignore = false;
    if (metadata.is_ok() && metadata->has_partition_info()) {
      apply_guid_map(metadata->partition_info(), partition_name, entry->type);
      ignore = ignore_device(metadata->partition_info(), partition_name);
    }

    char type_guid[GPT_GUID_STRLEN] = {};
    uint8_to_guid_string(type_guid, entry->type);
    zxlogf(TRACE,
           "gpt: partition=%u type=%s guid=%s name=%s first=0x%" PRIx64 " last=0x%" PRIx64 "\n",
           partitions, type_guid, partition_guid, partition_name, entry->first, entry->last);

    block_info.block_count = entry->last - entry->first + 1;
    device->SetInfo(entry, &block_info, block_op_size);

    if ((status = device->Add(partitions, ignore)) != ZX_OK) {
      return status;
    }
    device.release();
  }

  if (!has_partition) {
    auto placeholder = std::make_unique<PlaceholderDevice>(parent);
    status = placeholder->DdkAdd("placeholder", DEVICE_ADD_NON_BINDABLE);
    if (status != ZX_OK) {
      zxlogf(ERROR, "gpt: failed to add placeholder %s", zx_status_get_string(status));
      return status;
    }
    // Placeholder is managed by ddk.
    [[maybe_unused]] auto p = placeholder.release();
    return ZX_OK;
  }

  return ZX_OK;
}

constexpr zx_driver_ops_t gpt_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = Bind;
  return ops;
}();

}  // namespace gpt

ZIRCON_DRIVER(gpt, gpt::gpt_driver_ops, "zircon", "0.1");
