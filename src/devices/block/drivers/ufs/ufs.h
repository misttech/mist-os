// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_UFS_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_UFS_H_

#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <lib/device-protocol/pci.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/scsi/disk-dfv1.h>

#include <unordered_set>

#include <ddktl/device.h>
#include <fbl/array.h>
#include <fbl/condition_variable.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/string_printf.h>

#include "registers.h"
#include "src/devices/block/drivers/ufs/device_manager.h"
#include "transfer_request_processor.h"

namespace ufs {

constexpr uint32_t kMaxLunCount = 32;
constexpr uint32_t kMaxLunIndex = kMaxLunCount - 1;
constexpr uint32_t kDeviceInitTimeoutUs = 2000000;
constexpr uint32_t kHostControllerTimeoutUs = 1000;
constexpr uint32_t kMaxTransferSize1MiB = 1024 * 1024;
constexpr uint8_t kPlaceholderTarget = 0;

constexpr uint32_t kBlockSize = 4096;
constexpr uint32_t kSectorSize = 512;

constexpr uint32_t kMaxLunId = 0x7f;
constexpr uint16_t kUfsWellKnownlunId = 1 << 7;
constexpr uint16_t kScsiWellKnownLunId = 0xc100;

enum class WellKnownLuns : uint8_t {
  kReportLuns = 0x81,
  kBoot = 0xb0,
  kRpmb = 0xc4,
  kUfsDevice = 0xd0,
  kCount = 4,
};

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

struct IoCommand {
  scsi::DiskOp disk_op;

  // Ufs::ExecuteCommandAsync() checks that the incoming CDB's size does not exceed
  // this buffer's.
  uint8_t cdb_buffer[16];
  uint8_t cdb_length;
  uint8_t lun;
  uint32_t block_size_bytes;
  bool is_write;

  // Currently, data_buffer is only used by the UNMAP command and has a maximum size of 24 byte.
  uint8_t data_buffer[24];
  uint8_t data_length;
  zx::vmo data_vmo;

  list_node_t node;
};

class Ufs;

using HostControllerCallback = fit::function<zx::result<>(NotifyEvent, uint64_t data)>;

using UfsDeviceType = ddk::Device<Ufs, ddk::Suspendable, ddk::Resumable, ddk::Initializable>;
class Ufs : public scsi::Controller, public UfsDeviceType {
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
  ~Ufs() override = default;

  static zx_status_t Bind(void *ctx, zx_device_t *parent);
  zx_status_t AddDevice();

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();
  void DdkSuspend(ddk::SuspendTxn txn);
  void DdkResume(ddk::ResumeTxn txn);

  // scsi::Controller
  size_t BlockOpSize() override { return sizeof(IoCommand); }
  zx_status_t ExecuteCommandSync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                                 iovec data) override;
  void ExecuteCommandAsync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                           uint32_t block_size_bytes, scsi::DiskOp *disk_op, iovec data) override;

  inspect::Inspector &inspector() { return inspector_; }
  inspect::Node &inspect_node() { return inspect_node_; }

  fdf::MmioBuffer &GetMmio() { return mmio_; }

  DeviceManager &GetDeviceManager() const {
    ZX_DEBUG_ASSERT(device_manager_ != nullptr);
    return *device_manager_;
  }
  TransferRequestProcessor &GetTransferRequestProcessor() const {
    ZX_DEBUG_ASSERT(transfer_request_processor_ != nullptr);
    return *transfer_request_processor_;
  }

  // Queue an IO command to be performed asynchronously.
  void QueueIoCommand(IoCommand *io_cmd);

  // Convert block operations to UPIU commands and submit them asynchronously.
  void ProcessIoSubmissions();
  // Find the completed commands in the Request List and handle their completion.
  void ProcessCompletions();

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

  static zx::result<uint16_t> TranslateUfsLunToScsiLun(uint8_t ufs_lun);
  static zx::result<uint8_t> TranslateScsiLunToUfsLun(uint16_t scsi_lun);

  // for test
  uint32_t GetLogicalUnitCount() const { return logical_unit_count_; }
  thrd_t &GetIoThread() { return io_thread_; }

  void DisableCompletion() { disable_completion_ = true; }
  void DumpRegisters();

  bool HasWellKnownLun(WellKnownLuns lun) {
    return well_known_lun_set_.find(lun) != well_known_lun_set_.end();
  }

  bool IsSuspended() const { return device_manager_->IsSuspended(); }

 private:
  friend class UfsTest;
  int IrqLoop();
  int IoLoop();

  // Interrupt service routine. Check that the request is complete.
  zx::result<> Isr();

  // Initialize the UFS controller and bind the logical units.
  zx_status_t Init();
  zx::result<> InitQuirk();
  zx::result<> InitController();
  zx::result<> InitDeviceInterface(inspect::Node &controller_node);
  zx::result<> GetControllerDescriptor();
  zx::result<uint32_t> AddLogicalUnits();

  zx_status_t EnableHostController();
  zx_status_t DisableHostController();

  zx::result<> AllocatePages(zx::vmo &vmo, fzl::VmoMapper &mapper, size_t size);

  ddk::Pci pci_;
  fdf::MmioBuffer mmio_;
  fuchsia_hardware_pci::InterruptMode irq_mode_;
  zx::interrupt irq_;
  zx::bti bti_;
  inspect::Inspector inspector_;
  inspect::Node inspect_node_;

  std::mutex commands_lock_;
  // The pending list consists of commands that have been received via QueueIoCommand() and are
  // waiting for IO to start.
  list_node_t pending_commands_ TA_GUARDED(commands_lock_);

  // Notifies IoThread() that it has work to do. Signaled from QueueIoCommand() or the IRQ handler.
  sync_completion_t io_signal_;

  thrd_t irq_thread_ = 0;
  thrd_t io_thread_ = 0;
  bool irq_thread_started_ = false;

  bool io_thread_started_ = false;

  std::unique_ptr<DeviceManager> device_manager_;
  std::unique_ptr<TransferRequestProcessor> transfer_request_processor_;

  // Controller internal information.
  uint32_t logical_unit_count_ = 0;

  // The luns of the well-known logical units that exist on the UFS device.
  std::unordered_set<WellKnownLuns> well_known_lun_set_;

  // Callback function to perform when the host controller is notified.
  HostControllerCallback host_controller_callback_;

  bool driver_shutdown_ = false;
  bool disable_completion_ = false;

  // The maximum transfer size supported by UFSHCI spec is 65535 * 256 KiB. However, we limit the
  // maximum transfer size to 1MiB for performance reason.
  uint32_t max_transfer_bytes_ = kMaxTransferSize1MiB;

  bool qemu_quirk_ = false;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_UFS_H_
