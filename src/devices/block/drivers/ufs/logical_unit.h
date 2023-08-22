// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_LOGICAL_UNIT_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_LOGICAL_UNIT_H_

#include "ufs.h"

namespace ufs {

constexpr uint32_t kMaxTransferSize1MiB = 1024 * 1024;

class Ufs;
class LogicalUnit;
using LunDeviceType = ddk::Device<LogicalUnit, ddk::Initializable>;
class LogicalUnit : public LunDeviceType,
                    public ddk::BlockImplProtocol<LogicalUnit, ddk::base_protocol> {
 public:
  LogicalUnit(zx_device_t *parent, uint8_t lun_id, BlockDevice *bdev, Ufs &controller)
      : LunDeviceType(parent), controller_(controller), lun_id_(lun_id) {
    block_info_.block_count = bdev->block_count;
    block_info_.block_size = safemath::checked_cast<uint32_t>(bdev->block_size);
  }

  // Create a logical unit on |controller| with |lun_id|.
  static zx_status_t Bind(Ufs &controller, BlockDevice &block_device, uint8_t lun_id);
  fbl::String LunName() const { return fbl::StringPrintf("lun-%u", lun_id_); }

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();

  // BlockImpl implementations
  void BlockImplQuery(block_info_t *info_out, uint64_t *block_op_size_out);
  void BlockImplQueue(block_op_t *op, block_impl_queue_callback callback, void *cookie);

 private:
  zx_status_t AddLogicalUnit();
  zx_status_t Init();

  Ufs &controller_;
  const uint8_t lun_id_;

  block_info_t block_info_ = {};
  uint32_t max_transfer_blocks_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_LOGICAL_UNIT_H_
