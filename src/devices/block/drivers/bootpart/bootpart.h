// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_BOOTPART_BOOTPART_H_
#define SRC_DEVICES_BLOCK_DRIVERS_BOOTPART_BOOTPART_H_

#include <fuchsia/hardware/block/cpp/banjo.h>
#include <fuchsia/hardware/block/partition/cpp/banjo.h>
#include <lib/ddk/metadata.h>

#include <ddktl/device.h>
#include <fbl/string_printf.h>

namespace bootpart {

class BootPartition;
using DeviceType = ddk::Device<BootPartition, ddk::Initializable, ddk::GetProtocolable>;
class BootPartition : public DeviceType,
                      public ddk::BlockImplProtocol<BootPartition, ddk::base_protocol>,
                      public ddk::BlockPartitionProtocol<BootPartition> {
 public:
  BootPartition(zx_device_t* parent, uint64_t partition_index,
                const ddk::BlockImplProtocolClient& block_impl_client,
                const zbi_partition_t& zbi_partition, const block_info_t& block_info,
                size_t block_op_size)
      : DeviceType(parent),
        partition_index_(partition_index),
        block_impl_client_(block_impl_client),
        zbi_partition_(zbi_partition),
        block_info_(block_info),
        block_op_size_(block_op_size) {
    // The last LBA is inclusive.
    block_info_.block_count = zbi_partition.last_block - zbi_partition.first_block + 1;
  }
  ~BootPartition() = default;

  static zx_status_t Bind(void* ctx, zx_device_t* parent);
  zx_status_t AddBootPartition();
  fbl::String PartitionName() const { return fbl::StringPrintf("part-%03lu", partition_index_); }

  void DdkInit(ddk::InitTxn txn);
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out_protocol);
  void DdkRelease();

  // BlockImplProtocol implementation.
  void BlockImplQuery(block_info_t* out_info, uint64_t* out_block_op_size);
  void BlockImplQueue(block_op_t* bop, block_impl_queue_callback completion_cb, void* cookie);

  // BlockPartitionProtocol implementation.
  zx_status_t BlockPartitionGetGuid(guidtype_t guid_type, guid_t* out_guid);
  zx_status_t BlockPartitionGetName(char* out_name, size_t capacity);

 private:
  const uint64_t partition_index_;

  const ddk::BlockImplProtocolClient block_impl_client_;
  const zbi_partition_t zbi_partition_;

  block_info_t block_info_;
  const size_t block_op_size_;
};

}  // namespace bootpart

#endif  // SRC_DEVICES_BLOCK_DRIVERS_BOOTPART_BOOTPART_H_
