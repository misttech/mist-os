// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_NVME_NAMESPACE_H_
#define SRC_DEVICES_BLOCK_DRIVERS_NVME_NAMESPACE_H_

#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <lib/driver/compat/cpp/compat.h>

#include <fbl/string_printf.h>
#include <sdk/lib/driver/logging/cpp/logger.h>

namespace nvme {

class Nvme;

class Namespace : public ddk::BlockImplProtocol<Namespace> {
 public:
  explicit Namespace(Nvme* controller, uint32_t namespace_id)
      : controller_(controller), namespace_id_(namespace_id) {}

  // Create a namespace on |controller| with |namespace_id|.
  static zx::result<std::unique_ptr<Namespace>> Bind(Nvme* controller, uint32_t namespace_id);
  fbl::String NamespaceName() const { return fbl::StringPrintf("namespace-%u", namespace_id_); }

  // ddk::BlockImplProtocol implementations.
  void BlockImplQuery(block_info_t* out_info, uint64_t* out_block_op_size);
  void BlockImplQueue(block_op_t* op, block_impl_queue_callback callback, void* cookie);

 private:
  // Invokes AddChild().
  zx_status_t AddNamespace();

  // Main driver initialization.
  zx_status_t Init();

  fdf::Logger& logger();

  Nvme* const controller_;
  const uint32_t namespace_id_;

  block_info_t block_info_ = {};
  uint32_t max_transfer_blocks_;

  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> node_controller_;

  compat::BanjoServer block_impl_server_{ZX_PROTOCOL_BLOCK_IMPL, this, &block_impl_protocol_ops_};
  compat::SyncInitializedDeviceServer compat_server_;
};

}  // namespace nvme

#endif  // SRC_DEVICES_BLOCK_DRIVERS_NVME_NAMESPACE_H_
