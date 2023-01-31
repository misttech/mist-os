// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_NVME_NVME_H_
#define SRC_DEVICES_BLOCK_DRIVERS_NVME_NVME_H_

#include <lib/device-protocol/pci.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/sync/completion.h>
#include <threads.h>
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

class Namespace;

class Nvme;
using DeviceType = ddk::Device<Nvme, ddk::Initializable>;
class Nvme : public DeviceType {
 public:
  explicit Nvme(zx_device_t* parent, pci_protocol_t pci, fdf::MmioBuffer mmio,
                pci_interrupt_mode_t irq_mode, zx::interrupt irq, zx::bti bti)
      : DeviceType(parent),
        pci_(pci),
        mmio_(std::move(mmio)),
        irq_mode_(irq_mode),
        irq_(std::move(irq)),
        bti_(std::move(bti)) {}
  ~Nvme() = default;

  static zx_status_t Bind(void* ctx, zx_device_t* dev);
  zx_status_t AddDevice();

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();

  // Perform an admin command synchronously (i.e., blocks for the command to complete or timeout).
  zx_status_t DoAdminCommandSync(Submission& submission,
                                 std::optional<zx::unowned_vmo> admin_data = std::nullopt);

  inspect::Inspector& inspector() { return inspector_; }
  inspect::Node& inspect_node() { return inspect_node_; }

  QueuePair* io_queue() const { return io_queue_.get(); }
  uint32_t max_data_transfer_bytes() const { return max_data_transfer_bytes_; }
  bool volatile_write_cache_enabled() const { return volatile_write_cache_enabled_; }
  uint16_t atomic_write_unit_normal() const { return atomic_write_unit_normal_; }
  uint16_t atomic_write_unit_power_fail() const { return atomic_write_unit_power_fail_; }

 private:
  friend class fake_nvme::FakeController;

  static int IrqThread(void* arg) { return static_cast<Nvme*>(arg)->IrqLoop(); }
  int IrqLoop();

  // Main driver initialization.
  zx_status_t Init();

  pci_protocol_t pci_;
  fdf::MmioBuffer mmio_;
  pci_interrupt_mode_t irq_mode_;
  zx::interrupt irq_;
  zx::bti bti_;
  inspect::Inspector inspector_;
  inspect::Node inspect_node_;

  // Admin submission and completion queues.
  std::unique_ptr<QueuePair> admin_queue_;
  fbl::Mutex admin_lock_;  // Used to serialize admin transactions.
  sync_completion_t admin_signal_;
  Completion admin_result_;

  // IO submission and completion queues.
  std::unique_ptr<QueuePair> io_queue_;

  thrd_t irq_thread_;
  bool irq_thread_started_ = false;

  uint32_t max_data_transfer_bytes_;
  // This flag indicates whether the volatile write cache of the device is enabled. It can only be
  // enabled if the volatile write cache is supported.
  bool volatile_write_cache_enabled_ = false;

  uint16_t atomic_write_unit_normal_;
  uint16_t atomic_write_unit_power_fail_;

  std::vector<Namespace*> namespaces_;
};

}  // namespace nvme

#endif  // SRC_DEVICES_BLOCK_DRIVERS_NVME_NVME_H_
