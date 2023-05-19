// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_UFS_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_UFS_H_

#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <lib/device-protocol/pci.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/inspect/cpp/inspect.h>

#include <ddktl/device.h>
#include <fbl/condition_variable.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/string_printf.h>

#include "registers.h"
#include "transfer_request_processor.h"
#include "upiu/attributes.h"
#include "upiu/descriptors.h"
#include "upiu/flags.h"

namespace ufs {

constexpr uint32_t kMaxLun = 8;
constexpr uint32_t kDeviceInitTimeoutMs = 2000;
constexpr uint32_t kHostControllerTimeoutUs = 1000;

enum NotifyEvent {
  kInit = 0,
  kReset,
  kPreLinkStartup,
  kPostLinkStartup,
  kSetupTransferRequestList,
  kDeviceInitDone,
  kPrePowerModeChange,
  kPostPowerModeChange,
};

struct BlockDevice {
  bool is_scanned = false;
  std::string name;
  uint8_t lun = 0;
  off_t total_size = 0;
  size_t block_size = 0;
  uint32_t block_count = 0;
};

class Ufs;

using HostControllerCallback = fit::function<zx::result<>(NotifyEvent, uint64_t data)>;

using UfsDeviceType = ddk::Device<Ufs, ddk::Initializable>;
class Ufs : public UfsDeviceType {
 public:
  static constexpr char kDriverName[] = "ufs";

  explicit Ufs(zx_device_t *parent, ddk::Pci pci, fdf::MmioBuffer mmio,
               fuchsia_hardware_pci::InterruptMode irq_mode, zx::interrupt irq, zx::bti bti)
      : UfsDeviceType(parent),
        pci_(std::move(pci)),
        mmio_(std::move(mmio)),
        irq_mode_(irq_mode),
        irq_(std::move(irq)),
        bti_(std::move(bti)) {}
  ~Ufs() = default;

  static zx_status_t Bind(void *ctx, zx_device_t *parent);
  zx_status_t AddDevice();

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();

  inspect::Inspector &inspector() { return inspector_; }
  inspect::Node &inspect_node() { return inspect_node_; }

  fdf::MmioBuffer &GetMmio() { return mmio_; }
  BlockDevice &GetBlockDevice(uint8_t lun) { return block_devices_[lun]; }

  TransferRequestProcessor &GetTransferRequestProcessor() const {
    ZX_DEBUG_ASSERT(transfer_request_processor_ != nullptr);
    return *transfer_request_processor_;
  }

  // Used to register a platform-specific NotifyEventCallback, which handles variants and quirks for
  // each host interface platform.
  void SetHostControllerCallback(HostControllerCallback callback) {
    host_controller_callback_ = std::move(callback);
  }

  bool IsDriverShutdown() const { return driver_shutdown_; }

  // Defines a callback function to perform when an |event| occurs.
  static zx::result<> NotifyEventCallback(NotifyEvent event, uint64_t data);
  // The controller notifies the host controller when it takes the action defined in |event|.
  zx::result<> Notify(NotifyEvent event, uint64_t data);

  zx_status_t WaitWithTimeout(fit::function<zx_status_t()> wait_for, uint32_t timeout_us,
                              const fbl::String &timeout_message);

  sync_completion_t &GetScsiEvent() { return scsi_event_; }

  void QueueScsiCommand(std::unique_ptr<scsi_xfer> xfer);

 private:
  friend class UfsTest;
  // TODO(fxbug.dev/124835): Irq threads and scsi threads will be refactored.
  int IrqLoop();
  int ScsiLoop();

  // Interrupt service routine. Check that the request is complete.
  zx::result<> Isr();

  // Initialize the UFS controller and bind the logical units.
  zx_status_t Init();
  zx::result<> InitController();
  zx::result<> InitDeviceInterface();

  zx_status_t EnableHostController();
  zx_status_t DisableHostController();

  ddk::Pci pci_;
  fdf::MmioBuffer mmio_;
  fuchsia_hardware_pci::InterruptMode irq_mode_;
  zx::interrupt irq_;
  zx::bti bti_;
  inspect::Inspector inspector_;
  inspect::Node inspect_node_;

  // TODO(fxbug.dev/124835): Remove mock information when logical unit is implemented.
  static constexpr size_t kMockBlockSize = 4096;
  static constexpr uint32_t kMockTotalDeviceCapacity = (1 << 24);  // 16MB

  BlockDevice block_devices_[kMaxLun]{
      // TODO(fxbug.dev/124835): Temporarily add a single block device since block device scan is
      // not yet implemented.
      {
          true,
          "mock-device",
          0,
          kMockTotalDeviceCapacity,
          kMockBlockSize,
          static_cast<uint32_t>(kMockTotalDeviceCapacity / kMockBlockSize),
      }};

  // TODO(fxbug.dev/124835): Replace SCSI thread to I/O thread
  thrd_t scsi_thread_ = 0;
  thrd_t irq_thread_ = 0;
  bool scsi_thread_started_ = false;
  bool irq_thread_started_ = false;

  sync_completion_t scsi_event_;
  std::mutex xfer_list_lock_;
  fbl::DoublyLinkedList<std::unique_ptr<scsi_xfer>> scsi_xfer_list_ TA_GUARDED(xfer_list_lock_);

  std::unique_ptr<TransferRequestProcessor> transfer_request_processor_;

  // Callback function to perform when the host controller is notified.
  HostControllerCallback host_controller_callback_;

  bool driver_shutdown_ = false;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_UFS_H_
