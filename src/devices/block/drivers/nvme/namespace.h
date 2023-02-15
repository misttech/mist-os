// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_NVME_NAMESPACE_H_
#define SRC_DEVICES_BLOCK_DRIVERS_NVME_NAMESPACE_H_

#include <fuchsia/hardware/block/cpp/banjo.h>

#include <ddktl/device.h>
#include <fbl/string_printf.h>

namespace nvme {

class Nvme;

class Namespace;
using NamespaceDeviceType = ddk::Device<Namespace, ddk::Initializable>;
class Namespace : public NamespaceDeviceType,
                  public ddk::BlockImplProtocol<Namespace, ddk::base_protocol> {
 public:
  explicit Namespace(zx_device_t* parent, Nvme* controller, uint32_t namespace_id)
      : NamespaceDeviceType(parent), controller_(controller), namespace_id_(namespace_id) {}

  // Create a namespace on |controller| with |namespace_id|.
  static zx_status_t Bind(Nvme* controller, uint32_t namespace_id);
  zx_status_t AddNamespace();
  fbl::String NamespaceName() const { return fbl::StringPrintf("namespace-%u", namespace_id_); }

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();

  // BlockImpl implementations
  void BlockImplQuery(block_info_t* out_info, uint64_t* out_block_op_size);
  void BlockImplQueue(block_op_t* op, block_impl_queue_callback callback, void* cookie);

 private:
  // Main driver initialization.
  zx_status_t Init();

  // Check that the range of Read/Write IO command is valid.
  zx_status_t IsValidIoRwCommand(const block_op_t& op) const;

  Nvme* const controller_;
  const uint32_t namespace_id_;

  block_info_t block_info_ = {};
  uint32_t max_transfer_blocks_;
};

}  // namespace nvme

#endif  // SRC_DEVICES_BLOCK_DRIVERS_NVME_NAMESPACE_H_
