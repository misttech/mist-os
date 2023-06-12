// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_NVME_NVME_H_
#define SRC_DEVICES_BLOCK_DRIVERS_NVME_NVME_H_

#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <lib/device-protocol/pci.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/sync/completion.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <threads.h>
#include <zircon/listnode.h>
#include <zircon/types.h>

#include <ddktl/device.h>
#include <fbl/mutex.h>

#include "src/devices/block/drivers/nvme/commands.h"
#include "src/devices/block/drivers/nvme/queue-pair.h"
#include "src/devices/block/drivers/nvme/registers.h"

namespace fake_nvme {
class FakeController;
}

namespace nvme {

struct IoCommand {
  void Complete(zx_status_t status) { completion_cb(cookie, status, &op); }

  block_op_t op;
  block_impl_queue_callback completion_cb;
  void* cookie;

  uint32_t namespace_id;
  uint32_t block_size_bytes;

  list_node_t node;
};

class Nvme;
using DeviceType = ddk::Device<Nvme, ddk::Initializable>;
class Nvme : public DeviceType {
 public:
  static constexpr char kDriverName[] = "nvme";

  Nvme(zx_device_t* parent, ddk::Pci pci, fdf::MmioBuffer mmio,
       fuchsia_hardware_pci::InterruptMode irq_mode, zx::interrupt irq, zx::bti bti)
      : DeviceType(parent),
        pci_(std::move(pci)),
        mmio_(std::move(mmio)),
        irq_mode_(irq_mode),
        irq_(std::move(irq)),
        bti_(std::move(bti)) {}
  ~Nvme() = default;

  static zx_status_t Bind(void* ctx, zx_device_t* parent);
  zx_status_t AddDevice();

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();

  // Perform an admin command synchronously (i.e., blocks for the command to complete or timeout).
  // Returns the command completion.
  zx::result<Completion> DoAdminCommandSync(
      Submission& submission, std::optional<zx::unowned_vmo> admin_data = std::nullopt);

  // Queue an IO command to be performed asynchronously.
  void QueueIoCommand(IoCommand* io_cmd);

  inspect::Inspector& inspector() { return inspector_; }
  inspect::Node& inspect_node() { return inspect_node_; }

  QueuePair* io_queue() const { return io_queue_.get(); }
  uint32_t max_data_transfer_bytes() const { return max_data_transfer_bytes_; }
  bool volatile_write_cache_enabled() const { return volatile_write_cache_enabled_; }
  uint16_t atomic_write_unit_normal() const { return atomic_write_unit_normal_; }
  uint16_t atomic_write_unit_power_fail() const { return atomic_write_unit_power_fail_; }

 private:
  friend class fake_nvme::FakeController;

  int IrqLoop();
  int IoLoop();

  // Main driver initialization.
  zx_status_t Init();

  // Process pending IO commands. Called in the IoLoop().
  void ProcessIoSubmissions();
  // Process pending IO completions. Called in the IoLoop().
  void ProcessIoCompletions();

  ddk::Pci pci_;
  fdf::MmioBuffer mmio_;
  fuchsia_hardware_pci::InterruptMode irq_mode_;
  zx::interrupt irq_;
  zx::bti bti_;
  inspect::Inspector inspector_;
  inspect::Node inspect_node_;

  fbl::Mutex commands_lock_;
  // The pending list consists of commands that have been received via QueueIoCommand() and are
  // waiting for IO to start.
  list_node_t pending_commands_ TA_GUARDED(commands_lock_);

  // Admin submission and completion queues.
  std::unique_ptr<QueuePair> admin_queue_;
  fbl::Mutex admin_lock_;  // Used to serialize admin transactions.

  // IO submission and completion queues.
  std::unique_ptr<QueuePair> io_queue_;
  // Notifies IoThread() that it has work to do. Signaled from QueueIoCommand() or the IRQ handler.
  sync_completion_t io_signal_;

  thrd_t irq_thread_;
  thrd_t io_thread_;
  bool irq_thread_started_ = false;
  bool io_thread_started_ = false;

  uint32_t max_data_transfer_bytes_;
  // This flag indicates whether the volatile write cache of the device is enabled. It can only be
  // enabled if the volatile write cache is supported.
  bool volatile_write_cache_enabled_ = false;
  bool driver_shutdown_ = false;

  uint16_t atomic_write_unit_normal_;
  uint16_t atomic_write_unit_power_fail_;
};

}  // namespace nvme

#endif  // SRC_DEVICES_BLOCK_DRIVERS_NVME_NVME_H_
