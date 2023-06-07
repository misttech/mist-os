// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "abr-shim.h"

#include <lib/cksum.h>
#include <lib/ddk/binding_driver.h>

#include <memory>

#include <fbl/auto_lock.h>

#include "src/storage/lib/paver/pinecrest_abr_avbab_conversion.h"

// CRC32 implementation required for libabr.
extern "C" uint32_t AbrCrc32(const void* buf, size_t buf_size) {
  return crc32(0UL, reinterpret_cast<const uint8_t*>(buf), buf_size);
}

namespace {

constexpr zx_signals_t kBlockOpCompleteSignal = ZX_USER_SIGNAL_0;

struct AbrBlockOp {
  zx_status_t* status;
  zx::unowned_event complete;
};

}  // namespace

namespace block {

zx_status_t AbrShim::Bind(void* ctx, zx_device_t* dev) {
  ddk::BlockImplProtocolClient block_impl_client(dev);
  if (!block_impl_client.is_valid()) {
    zxlogf(ERROR, "Failed to get block impl protocol");
    return ZX_ERR_NO_RESOURCES;
  }

  ddk::BlockPartitionProtocolClient block_partition_client(dev);
  if (!block_partition_client.is_valid()) {
    zxlogf(ERROR, "Failed to get block partition protocol");
    return ZX_ERR_NO_RESOURCES;
  }

  block_info_t block_info{};
  uint64_t block_op_size{};
  block_impl_client.Query(&block_info, &block_op_size);

  zx::vmo block_data;
  if (zx_status_t status = zx::vmo::create(block_info.block_size, 0, &block_data);
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to create VMO for block IO: %s", zx_status_get_string(status));
    return status;
  }

  auto device =
      std::make_unique<AbrShim>(dev, block_impl_client, block_partition_client,
                                std::move(block_data), block_info.block_size, block_op_size);
  if (zx_status_t status = device->DdkAdd("abr-shim"); status != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed: %s", zx_status_get_string(status));
    return status;
  }

  [[maybe_unused]] auto* unowned_device = device.release();
  return ZX_OK;
}

void AbrShim::DdkSuspend(ddk::SuspendTxn txn) {
  if (txn.suspend_reason() == DEVICE_SUSPEND_REASON_REBOOT_RECOVERY ||
      txn.suspend_reason() == DEVICE_SUSPEND_REASON_REBOOT_BOOTLOADER) {
    {
      fbl::AutoLock lock(&io_lock_);
      rebooting_to_recovery_or_bl_ = true;
    }

    // No more client txns can be queued after this point, so we should have the final say on the
    // contents of the partition.
    const AbrOps ops{
        .context = this,
        .read_abr_metadata = ReadAbrMetadata,
        .write_abr_metadata = WriteAbrMetadata,
    };
    if (txn.suspend_reason() == DEVICE_SUSPEND_REASON_REBOOT_RECOVERY) {
      if (auto result = AbrSetOneShotRecovery(&ops, /*enable=*/true); result == kAbrResultOk) {
        zxlogf(INFO, "Set ABR one-shot recovery flag");
      } else {
        zxlogf(ERROR, "Failed to set ABR one-shot recovery flag: %d", result);
        txn.Reply(ZX_ERR_INTERNAL, txn.requested_state());
        return;
      }
    } else if (txn.suspend_reason() == DEVICE_SUSPEND_REASON_REBOOT_BOOTLOADER) {
      if (auto result = AbrSetOneShotBootloader(&ops, /*enable=*/true); result == kAbrResultOk) {
        zxlogf(INFO, "Set ABR one-shot bootloader flag");
      } else {
        zxlogf(ERROR, "Failed to set ABR one-shot bootloader flag: %d", result);
        txn.Reply(ZX_ERR_INTERNAL, txn.requested_state());
        return;
      }
    }
  }

  txn.Reply(ZX_OK, txn.requested_state());
}

zx_status_t AbrShim::DdkGetProtocol(uint32_t proto_id, void* out) {
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

void AbrShim::BlockImplQuery(block_info_t* out_info, uint64_t* out_block_op_size) {
  fbl::AutoLock lock(&io_lock_);
  block_impl_client_.Query(out_info, out_block_op_size);
}

void AbrShim::BlockImplQueue(block_op_t* txn, block_impl_queue_callback callback, void* cookie) {
  {
    fbl::AutoLock lock(&io_lock_);
    if (!rebooting_to_recovery_or_bl_) {
      block_impl_client_.Queue(txn, callback, cookie);
      return;
    }
  }

  // Reboot to recovery is in progress -- cancel out the txn to prevent client accesses from
  // interfering with libabr updating the metadata.
  callback(cookie, ZX_ERR_CANCELED, txn);
}

zx_status_t AbrShim::BlockPartitionGetGuid(guidtype_t guid_type, guid_t* out_guid) {
  return block_partition_client_.GetGuid(guid_type, out_guid);
}

zx_status_t AbrShim::BlockPartitionGetName(char* out_name, size_t name_capacity) {
  return block_partition_client_.GetName(out_name, name_capacity);
}

void AbrShim::BlockOpCallback(void* ctx, zx_status_t status, block_op_t* op) {
  const size_t block_op_size = *reinterpret_cast<size_t*>(ctx);
  block::Operation<AbrBlockOp> block_op(op, block_op_size);
  *block_op.private_storage()->status = status;
  block_op.private_storage()->complete->signal(0, kBlockOpCompleteSignal);
}

zx_status_t AbrShim::DoBlockOp(uint8_t opcode) const {
  zx::event complete;
  if (zx_status_t status = zx::event::create(0, &complete); status != ZX_OK) {
    zxlogf(ERROR, "Failed to create event: %s", zx_status_get_string(status));
    return status;
  }

  std::optional op = block::Operation<AbrBlockOp>::Alloc(block_op_size_);
  if (!op) {
    zxlogf(ERROR, "Failed to create block operation");
    return ZX_ERR_NO_MEMORY;
  }

  zx_status_t block_status = ZX_ERR_IO;
  *op->private_storage() = {
      .status = &block_status,
      .complete = complete.borrow(),
  };

  if (opcode == BLOCK_OPCODE_READ || opcode == BLOCK_OPCODE_WRITE) {
    op->operation()->rw = {
        .command = {.opcode = opcode, .flags = 0},
        .extra = 0,
        .vmo = block_data_.get(),
        .length = 1,
        .offset_dev = 0,
        .offset_vmo = 0,
    };
  } else {
    op->operation()->command = {.opcode = opcode, .flags = 0};
  }

  size_t block_op_size = block_op_size_;
  block_impl_client_.Queue(op->take(), BlockOpCallback, &block_op_size);

  zx_status_t status = complete.wait_one(kBlockOpCompleteSignal, zx::time::infinite(), nullptr);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to wait for block operation: %s", zx_status_get_string(status));
    return status;
  }

  return block_status;
}

bool AbrShim::ReadAbrMetadata(size_t size, uint8_t* buffer) {
  // These assumption simplifies the block read/write logic and VMO creation.
  ZX_DEBUG_ASSERT(size == sizeof(AbrData));
  ZX_DEBUG_ASSERT(size <= block_size_);

  AbrData data;

  {
    fbl::AutoLock lock(&io_lock_);

    if (zx_status_t status = DoBlockOp(BLOCK_OPCODE_READ); status != ZX_OK) {
      zxlogf(ERROR, "Failed to read from block device: %s", zx_status_get_string(status));
      return false;
    }

    if (zx_status_t status = block_data_.read(&data, 0, sizeof(data)); status != ZX_OK) {
      zxlogf(ERROR, "Failed to read from VMO: %s", zx_status_get_string(status));
      return false;
    }
  }

  memcpy(buffer, &data, sizeof(data));
  return true;
}

bool AbrShim::WriteAbrMetadata(const uint8_t* buffer, size_t size) {
  ZX_DEBUG_ASSERT(size == sizeof(AbrData));
  ZX_DEBUG_ASSERT(size <= block_size_);

  AbrData data;
  memcpy(&data, buffer, sizeof(data));

  fbl::AutoLock lock(&io_lock_);

  if (size < block_size_) {
    // Preserve the data at the end of the block.
    if (zx_status_t status = DoBlockOp(BLOCK_OPCODE_READ); status != ZX_OK) {
      zxlogf(ERROR, "Failed to read from block device: %s", zx_status_get_string(status));
      return false;
    }
  }

  if (zx_status_t status = block_data_.write(&data, 0, sizeof(data))) {
    zxlogf(ERROR, "Failed to write to VMO: %s", zx_status_get_string(status));
    return false;
  }

  if (zx_status_t status = DoBlockOp(BLOCK_OPCODE_WRITE); status != ZX_OK) {
    zxlogf(ERROR, "Failed to write to block device: %s", zx_status_get_string(status));
    return false;
  }

  // Issue a final flush just in case.
  if (zx_status_t status = DoBlockOp(BLOCK_OPCODE_FLUSH); status != ZX_OK) {
    zxlogf(ERROR, "Failed to flush block device: %s", zx_status_get_string(status));
    return false;
  }

  return true;
}

constexpr zx_driver_ops_t abr_shim_driver_ops = []() {
  zx_driver_ops_t driver_ops = {};
  driver_ops.version = DRIVER_OPS_VERSION;
  driver_ops.bind = AbrShim::Bind;
  return driver_ops;
}();

}  // namespace block

ZIRCON_DRIVER(abr_shim, block::abr_shim_driver_ops, "zircon", "0.1");
