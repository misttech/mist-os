// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_ABR_SHIM_ABR_SHIM_H_
#define SRC_DEVICES_BLOCK_DRIVERS_ABR_SHIM_ABR_SHIM_H_

#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <fuchsia/hardware/block/partition/cpp/banjo.h>
#include <lib/abr/abr.h>
#include <lib/operation/block.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/vmo.h>

#include <ddktl/device.h>
#include <fbl/mutex.h>

namespace block {

class AbrShim;
using AbrShimType = ddk::Device<AbrShim, ddk::GetProtocolable, ddk::Suspendable>;

class AbrShim : public AbrShimType,
                public ddk::BlockImplProtocol<AbrShim, ddk::base_protocol>,
                public ddk::BlockPartitionProtocol<AbrShim> {
 public:
  static zx_status_t Bind(void* ctx, zx_device_t* dev);

  AbrShim(zx_device_t* parent, const ddk::BlockImplProtocolClient& block_impl_client,
          const ddk::BlockPartitionProtocolClient& block_partition_client, zx::vmo block_data,
          uint32_t block_size, uint32_t block_op_size)
      : AbrShimType(parent),
        block_impl_client_(block_impl_client),
        block_partition_client_(block_partition_client),
        block_data_(std::move(block_data)),
        block_size_(block_size),
        block_op_size_(block_op_size) {}

  virtual ~AbrShim() = default;

  void DdkRelease() { delete this; }

  void DdkSuspend(ddk::SuspendTxn txn);
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out);

  void BlockImplQuery(block_info_t* out_info, uint64_t* out_block_op_size);
  void BlockImplQueue(block_op_t* txn, block_impl_queue_callback callback, void* cookie);

  zx_status_t BlockPartitionGetGuid(guidtype_t guid_type, guid_t* out_guid);
  zx_status_t BlockPartitionGetName(char* out_name, size_t name_capacity);

 private:
  // Provided as function pointers to libabr.
  static bool ReadAbrMetadata(void* context, size_t size, uint8_t* buffer) {
    return reinterpret_cast<AbrShim*>(context)->ReadAbrMetadata(size, buffer);
  }

  static bool WriteAbrMetadata(void* context, const uint8_t* buffer, size_t size) {
    return reinterpret_cast<AbrShim*>(context)->WriteAbrMetadata(buffer, size);
  }

  static void BlockOpCallback(void* ctx, zx_status_t status, block_op_t* op);
  zx_status_t DoBlockOp(uint32_t command) const TA_REQ(io_lock_);

  bool ReadAbrMetadata(size_t size, uint8_t* buffer);
  bool WriteAbrMetadata(const uint8_t* buffer, size_t size);

  fbl::Mutex io_lock_;
  bool rebooting_to_recovery_or_bl_ TA_GUARDED(io_lock_) = false;
  const ddk::BlockImplProtocolClient block_impl_client_ TA_GUARDED(io_lock_);
  const ddk::BlockPartitionProtocolClient block_partition_client_;
  const zx::vmo block_data_ TA_GUARDED(io_lock_);
  const uint32_t block_size_;
  const uint32_t block_op_size_;
};

}  // namespace block

#endif  // SRC_DEVICES_BLOCK_DRIVERS_ABR_SHIM_ABR_SHIM_H_
