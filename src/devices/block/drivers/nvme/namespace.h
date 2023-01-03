// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_NVME_NAMESPACE_H_
#define SRC_DEVICES_BLOCK_DRIVERS_NVME_NAMESPACE_H_

#include <fuchsia/hardware/block/cpp/banjo.h>
#include <lib/sync/completion.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <threads.h>
#include <zircon/listnode.h>
#include <zircon/types.h>

#include <ddktl/device.h>
#include <fbl/mutex.h>
#include <fbl/string_printf.h>

namespace nvme {

struct IoCommand;
class Nvme;

class Namespace;
using NamespaceDeviceType = ddk::Device<Namespace, ddk::Initializable>;
class Namespace : public NamespaceDeviceType,
                  public ddk::BlockImplProtocol<Namespace, ddk::base_protocol> {
 public:
  explicit Namespace(zx_device_t* parent, Nvme* controller, uint32_t namespace_id)
      : NamespaceDeviceType(parent), controller_(controller), namespace_id_(namespace_id) {}

  // Create a namespace on |controller| with |namespace_id|.
  static zx::result<Namespace*> Bind(Nvme* controller, uint32_t namespace_id);
  zx_status_t AddNamespace();
  fbl::String NamespaceName() const { return fbl::StringPrintf("namespace-%u", namespace_id_); }

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();

  // BlockImpl implementations
  void BlockImplQuery(block_info_t* out_info, uint64_t* out_block_op_size);
  void BlockImplQueue(block_op_t* op, block_impl_queue_callback callback, void* cookie);

  // Notifies IoThread() that it has work to do. Called by the IRQ handler.
  void SignalIo() { sync_completion_signal(&io_signal_); }

 private:
  static int IoThread(void* arg) { return static_cast<Namespace*>(arg)->IoLoop(); }
  int IoLoop();

  // Main driver initialization.
  zx_status_t Init();

  // Process pending IO commands. Called in the IoLoop().
  void ProcessIoSubmissions();
  // Process pending IO completions. Called in the IoLoop().
  void ProcessIoCompletions();

  Nvme* controller_;
  const uint32_t namespace_id_;

  fbl::Mutex commands_lock_;
  // The pending list consists of commands that have been received via BlockImplQueue() and are
  // waiting for IO to start.
  list_node_t pending_commands_ TA_GUARDED(commands_lock_);

  // Notifies IoThread() that it has work to do. Signaled from BlockImplQueue() or the IRQ handler.
  sync_completion_t io_signal_;

  thrd_t io_thread_;
  bool io_thread_started_ = false;
  bool driver_shutdown_ = false;

  block_info_t block_info_ = {};
  uint32_t max_transfer_blocks_;
};

}  // namespace nvme

#endif  // SRC_DEVICES_BLOCK_DRIVERS_NVME_NAMESPACE_H_
