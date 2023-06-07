// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/drivers/bootpart/bootpart.h"

#include <assert.h>
#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <fuchsia/hardware/block/partition/cpp/banjo.h>
#include <inttypes.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/stdcompat/span.h>
#include <lib/zbi-format/partition.h>
#include <lib/zbi-format/zbi.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>

#include "src/devices/block/lib/common/include/common.h"

namespace bootpart {

static fbl::String ToGuidString(const uint8_t* src) {
  const struct guid* guid = reinterpret_cast<const struct guid*>(src);
  return fbl::StringPrintf("%08X-%04X-%04X-%02X%02X-%02X%02X%02X%02X%02X%02X", guid->data1,
                           guid->data2, guid->data3, guid->data4[0], guid->data4[1], guid->data4[2],
                           guid->data4[3], guid->data4[4], guid->data4[5], guid->data4[6],
                           guid->data4[7]);
}

// implement device protocol:

void BootPartition::BlockImplQuery(block_info_t* out_info, uint64_t* out_block_op_size) {
  *out_info = block_info_;
  *out_block_op_size = block_op_size_;
}

void BootPartition::BlockImplQueue(block_op_t* bop, block_impl_queue_callback completion_cb,
                                   void* cookie) {
  switch (bop->command.opcode) {
    case BLOCK_OPCODE_READ:
    case BLOCK_OPCODE_WRITE: {
      if (zx_status_t status = block::CheckIoRange(bop->rw, block_info_.block_count);
          status != ZX_OK) {
        completion_cb(cookie, status, bop);
        return;
      }

      // Adjust for partition starting block
      bop->rw.offset_dev += zbi_partition_.first_block;
      break;
    }
    case BLOCK_OPCODE_FLUSH:
      break;
    default:
      completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, bop);
      return;
  }

  block_impl_client_.Queue(bop, completion_cb, cookie);
}

void BootPartition::DdkInit(ddk::InitTxn txn) {
  // Add empty partition map metadata to prevent this driver from binding to its child devices.
  zx_status_t status = device_add_metadata(zxdev(), DEVICE_METADATA_PARTITION_MAP, nullptr, 0);
  // Make the device visible after adding metadata. If there was an error, this will schedule
  // unbinding of the device.
  txn.Reply(status);
}

void BootPartition::DdkRelease() { delete this; }

static_assert(ZBI_PARTITION_GUID_LEN == GUID_LENGTH, "GUID length mismatch");

zx_status_t BootPartition::BlockPartitionGetGuid(guidtype_t guid_type, guid_t* out_guid) {
  switch (guid_type) {
    case GUIDTYPE_TYPE:
      memcpy(out_guid, zbi_partition_.type_guid, ZBI_PARTITION_GUID_LEN);
      return ZX_OK;
    case GUIDTYPE_INSTANCE:
      memcpy(out_guid, zbi_partition_.uniq_guid, ZBI_PARTITION_GUID_LEN);
      return ZX_OK;
    default:
      return ZX_ERR_INVALID_ARGS;
  }
}

static_assert(ZBI_PARTITION_NAME_LEN <= MAX_PARTITION_NAME_LENGTH, "Name length mismatch");

zx_status_t BootPartition::BlockPartitionGetName(char* out_name, size_t capacity) {
  if (capacity < ZBI_PARTITION_NAME_LEN) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }

  strlcpy(out_name, zbi_partition_.name, ZBI_PARTITION_NAME_LEN);
  return ZX_OK;
}

zx_status_t BootPartition::DdkGetProtocol(uint32_t proto_id, void* out_protocol) {
  auto* proto = static_cast<ddk::AnyProtocol*>(out_protocol);
  proto->ctx = this;
  switch (proto_id) {
    case ZX_PROTOCOL_BLOCK_IMPL: {
      proto->ops = &block_impl_protocol_ops_;
      return ZX_OK;
    }
    case ZX_PROTOCOL_BLOCK_PARTITION: {
      proto->ops = &block_partition_protocol_ops_;
      return ZX_OK;
    }
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
}

zx_status_t BootPartition::Bind(void* ctx, zx_device_t* parent) {
  ddk::BlockImplProtocolClient block_impl_client(parent);
  if (!block_impl_client.is_valid()) {
    zxlogf(ERROR, "Device does not support block impl protocol.");
    return ZX_ERR_NOT_SUPPORTED;
  }

  uint8_t buffer[METADATA_PARTITION_MAP_MAX];
  size_t actual;
  zx_status_t status =
      device_get_metadata(parent, DEVICE_METADATA_PARTITION_MAP, buffer, sizeof(buffer), &actual);
  if (status != ZX_OK) {
    return status;
  }

  zbi_partition_map_t* map = reinterpret_cast<zbi_partition_map_t*>(buffer);
  static_assert(alignof(zbi_partition_map_t) >= alignof(zbi_partition_t));
  cpp20::span<const zbi_partition_t> partitions(reinterpret_cast<const zbi_partition_t*>(map + 1),
                                                map->partition_count);
  if (partitions.empty()) {
    zxlogf(ERROR, "Partition count is zero.");
    return ZX_ERR_INTERNAL;
  }

  block_info_t block_info;
  size_t block_op_size;
  block_impl_client.Query(&block_info, &block_op_size);

  for (size_t i = 0; i < partitions.size(); ++i) {
    fbl::AllocChecker ac;
    auto bootpart = fbl::make_unique_checked<BootPartition>(
        &ac, parent, i, block_impl_client, partitions[i], block_info, block_op_size);
    if (!ac.check()) {
      zxlogf(ERROR, "Failed to allocate memory for boot partition driver.");
      return ZX_ERR_NO_MEMORY;
    }

    status = bootpart->AddBootPartition();
    if (status != ZX_OK) {
      return status;
    }

    // The DriverFramework now owns driver.
    [[maybe_unused]] auto placeholder = bootpart.release();
  }
  return ZX_OK;
}

zx_status_t BootPartition::AddBootPartition() {
  fbl::String type_guid = ToGuidString(zbi_partition_.type_guid);
  fbl::String uniq_guid = ToGuidString(zbi_partition_.uniq_guid);
  zxlogf(TRACE,
         "Partition %lu (%s) type=%s guid=%s name=%s first=0x%" PRIx64 " last=0x%" PRIx64 "\n",
         partition_index_, PartitionName().c_str(), type_guid.c_str(), uniq_guid.c_str(),
         zbi_partition_.name, zbi_partition_.first_block, zbi_partition_.last_block);

  zx_status_t status = DdkAdd(PartitionName().c_str());
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed DdkAdd: %s", zx_status_get_string(status));
  }
  return status;
}

static zx_driver_ops_t driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = BootPartition::Bind,
};

}  // namespace bootpart

ZIRCON_DRIVER(bootpart, bootpart::driver_ops, "zircon", "0.1");
