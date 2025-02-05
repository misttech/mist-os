// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_UFS_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_UFS_H_

#include <fidl/fuchsia.hardware.pci/cpp/wire.h>
#include <fidl/fuchsia.hardware.ufs/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/scsi/block-device.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zircon-internal/thread_annotations.h>

#include <unordered_set>

#include <fbl/array.h>
#include <fbl/condition_variable.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/string_printf.h>

#include "src/devices/block/drivers/ufs/device_manager.h"
#include "src/devices/block/drivers/ufs/registers.h"
#include "src/devices/block/drivers/ufs/request_processor.h"
#include "src/devices/block/drivers/ufs/task_management_request_processor.h"
#include "src/devices/block/drivers/ufs/transfer_request_processor.h"
#include "src/devices/block/drivers/ufs/ufs_config.h"

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
  kSetupTaskManagementRequestList,
  kDeviceInitDone,
  kPrePowerModeChange,
  kPostPowerModeChange,
};

struct IoCommand {
  scsi::DeviceOp device_op;

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

struct InspectProperties {
  inspect::BoolProperty power_suspended;              // Updated whenever power state changes.
  inspect::UintProperty wake_on_request_count;        // Updated whenever wake-on-request occurs.
  inspect::ExponentialUintHistogram wake_latency_us;  // Updated whenever wake-on-request occurs.
  // Controller
  inspect::UintProperty max_transfer_bytes;  // Set once by the init thread.
  inspect::UintProperty logical_unit_count;  // Set once by the init thread.
  inspect::StringProperty reference_clock;   // Set once by the init thread.
  inspect::UintProperty power_condition;     // Updated whenever power state changes.
  inspect::UintProperty link_state;          // Updated whenever power state changes.
  // Version
  inspect::UintProperty major_version_number;  // Set once by the init thread.
  inspect::UintProperty minor_version_number;  // Set once by the init thread.
  inspect::UintProperty version_suffix;        // Set once by the init thread.
  // Capabilities
  inspect::BoolProperty crypto_support;                        // Set once by the init thread.
  inspect::BoolProperty uic_dme_test_mode_command_supported;   // Set once by the init thread.
  inspect::BoolProperty out_of_order_data_delivery_supported;  // Set once by the init thread.
  inspect::BoolProperty _64_bit_addressing_supported;          // Set once by the init thread.
  inspect::BoolProperty auto_hibernation_support;              // Set once by the init thread.
  inspect::UintProperty
      number_of_utp_task_management_request_slots;  // Set once by the init thread.
  inspect::UintProperty
      number_of_outstanding_rtt_requests_supported;            // Set once by the init thread.
  inspect::UintProperty number_of_utp_transfer_request_slots;  // Set once by the init thread.
  // Attribute
  inspect::UintProperty b_boot_lun_en;         // Set once by the init thread.
  inspect::UintProperty b_current_power_mode;  // Updated whenever power state changes.
  inspect::UintProperty b_active_icc_level;    // Updated whenever power state changes.
  // Unipro
  inspect::UintProperty remote_version;           // Set once by the init thread.
  inspect::UintProperty local_version;            // Set once by the init thread.
  inspect::UintProperty host_t_activate;          // Set once by the init thread.
  inspect::UintProperty device_t_activate;        // Set once by the init thread.
  inspect::UintProperty host_granularity;         // Set once by the init thread.
  inspect::UintProperty device_granularity;       // Set once by the init thread.
  inspect::UintProperty pa_active_tx_data_lanes;  // Set once by the init thread.
  inspect::UintProperty pa_active_rx_data_lanes;  // Set once by the init thread.
  inspect::UintProperty pa_max_rx_hs_gear;        // Set once by the init thread.
  inspect::UintProperty pa_tx_gear;               // Updated whenever gear changes.
  inspect::UintProperty pa_rx_gear;               // Updated whenever gear changes.
  inspect::BoolProperty tx_termination;           // Set once by the init thread.
  inspect::BoolProperty rx_termination;           // Set once by the init thread.
  inspect::UintProperty pa_hs_series;             // Set once by the init thread.
  inspect::UintProperty power_mode;               // Updated whenever power state changes.
  // Write Protect
  inspect::BoolProperty is_power_on_write_protect_enabled;   // Set once by the init thread.
  inspect::BoolProperty logical_lun_power_on_write_protect;  // Set once by the init thread.
  // Background Operations
  inspect::BoolProperty is_background_op_enabled;  // Updated whenever the power state changes or an
                                                   // exception event occurs.
  // WriteBooster
  inspect::BoolProperty is_write_booster_enabled;                    // Set once by the init thread.
  inspect::BoolProperty writebooster_buffer_flush_during_hibernate;  // Set once by the init thread.
  inspect::BoolProperty writebooster_buffer_flush_enabled;           // Set once by the init thread.
  inspect::UintProperty write_booster_buffer_type;                   // Set once by the init thread.
  inspect::UintProperty user_space_configuration_option;             // Set once by the init thread.
  inspect::UintProperty write_booster_dedicated_lu;                  // Set once by the init thread.
  inspect::UintProperty write_booster_buffer_size_in_bytes;          // Set once by the init thread.
};

using HostControllerCallback = fit::function<zx::result<>(NotifyEvent, uint64_t data)>;

class Ufs : public fdf::DriverBase,
            public scsi::Controller,
            public fidl::WireServer<fuchsia_hardware_ufs::Ufs> {
 public:
  static constexpr char kDriverName[] = "ufs";
  static constexpr char kHardwarePowerElementName[] = "ufs-hardware";
  static constexpr char kSystemWakeOnRequestPowerElementName[] = "ufs-system-wake-on-request";
  // Common to hardware power and wake-on-request power elements.
  // TODO(https://fxbug.dev/42075643): We need to add sleep(low-power) level
  static constexpr fuchsia_power_broker::PowerLevel kPowerLevelOff = 0;
  static constexpr fuchsia_power_broker::PowerLevel kPowerLevelOn = 1;

  Ufs(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase(kDriverName, std::move(start_args), std::move(dispatcher)),
        config_(take_config<ufs_config::Config>()) {}
  ~Ufs() override = default;

  zx::result<> Start() override;

  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // scsi::Controller
  fidl::WireSyncClient<fuchsia_driver_framework::Node> &root_node() override { return root_node_; }
  std::string_view driver_name() const override { return name(); }
  const std::shared_ptr<fdf::Namespace> &driver_incoming() const override { return incoming(); }
  std::shared_ptr<fdf::OutgoingDirectory> &driver_outgoing() override { return outgoing(); }
  async_dispatcher_t *driver_async_dispatcher() const { return dispatcher(); }
  const std::optional<std::string> &driver_node_name() const override { return node_name(); }
  fdf::Logger &driver_logger() override { return logger(); }
  const ufs_config::Config &config() const { return config_; }

  size_t BlockOpSize() override { return sizeof(IoCommand); }
  zx_status_t ExecuteCommandSync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                                 iovec data) override;
  void ExecuteCommandAsync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                           uint32_t block_size_bytes, scsi::DeviceOp *device_op,
                           iovec data) override;

  const fdf::MmioBuffer &GetMmio() const {
    ZX_ASSERT(mmio_.has_value());
    return mmio_.value();
  }

  DeviceManager &GetDeviceManager() const {
    ZX_DEBUG_ASSERT(device_manager_ != nullptr);
    return *device_manager_;
  }
  TransferRequestProcessor &GetTransferRequestProcessor() const {
    ZX_DEBUG_ASSERT(transfer_request_processor_ != nullptr);
    return *transfer_request_processor_;
  }
  TaskManagementRequestProcessor &GetTaskManagementRequestProcessor() const {
    ZX_DEBUG_ASSERT(task_management_request_processor_ != nullptr);
    return *task_management_request_processor_;
  }

  // Queue an IO command to be performed asynchronously.
  void QueueIoCommand(IoCommand *io_cmd);

  // Convert block operations to UPIU commands and submit them asynchronously.
  void ProcessIoSubmissions();
  // Find the completed Admin commands in the Request List and handle their completion.
  void ProcessAdminCompletions();
  // Find the completed IO commands in the Request List and handle their completion.
  void ProcessIoCompletions();

  // Used to register a platform-specific NotifyEventCallback, which handles variants and quirks for
  // each host interface platform.
  void SetHostControllerCallback(HostControllerCallback callback) {
    host_controller_callback_ = std::move(callback);
  }

  // Defines a callback function to perform when an |event| occurs.
  static zx::result<> NotifyEventCallback(NotifyEvent event, uint64_t data);
  // The controller notifies the host controller when it takes the action defined in |event|.
  zx::result<> Notify(NotifyEvent event, uint64_t data);

  zx_status_t WaitWithTimeout(fit::function<zx_status_t()> wait_for, uint32_t timeout_us,
                              const fbl::String &timeout_message);

  static zx::result<uint16_t> TranslateUfsLunToScsiLun(uint8_t ufs_lun);
  static zx::result<uint8_t> TranslateScsiLunToUfsLun(uint16_t scsi_lun);

  fdf::Dispatcher &exception_event_dispatcher() { return exception_event_dispatcher_; }
  libsync::Completion &exception_event_completion() { return exception_event_completion_; }

  // for test
  uint32_t GetLogicalUnitCount() const { return logical_unit_count_; }

  void DisableCompletion() { disable_completion_ = true; }
  void DumpRegisters();

  bool HasWellKnownLun(WellKnownLuns lun) {
    return well_known_lun_set_.find(lun) != well_known_lun_set_.end();
  }

  bool IsResumed() const { return device_manager_->IsResumed(); }

  const inspect::Inspector &inspect() { return inspector().inspector(); }

 protected:
  // Initialize the UFS controller and bind the logical units.
  // Declare this as virtual to delay driver initialization in tests.
  virtual zx_status_t Init();

  virtual zx::result<fdf::MmioBuffer> CreateMmioBuffer(zx_off_t offset, size_t size, zx::vmo vmo) {
    return fdf::MmioBuffer::Create(offset, size, std::move(vmo), ZX_CACHE_POLICY_UNCACHED_DEVICE);
  }

 private:
  friend class UfsTest;
  int IrqLoop();
  // IoLoop() cannot process SCSI commands when the UFS device is suspended. The SCSI StartStopUnit
  // admin command is required to resume UFS device, so AdminLoop() is required to handle the
  // completion of the admin command even in a suspended state.
  int AdminLoop();
  int IoLoop();

  // Interrupt service routine. Check that the request is complete.
  zx::result<> Isr();

  zx::result<> ConnectToPciService();
  zx::result<> ConfigResources();

  void PopulateVersionInspect(inspect::Node *inspect_node);
  void PopulateCapabilitiesInspect(inspect::Node *inspect_node);

  zx::result<> InitMmioBuffer();
  zx::result<> InitQuirk();
  zx::result<> InitController();
  zx::result<> InitDeviceInterface(inspect::Node &controller_node);
  zx::result<> GetControllerDescriptor();
  zx::result<uint32_t> AddLogicalUnits();

  zx_status_t EnableHostController();
  zx_status_t DisableHostController();

  zx::result<> AllocatePages(zx::vmo &vmo, fzl::VmoMapper &mapper, size_t size);

  // TODO(b/309152899): Once fuchsia.power.SuspendEnabled config cap is available, have this method
  // return failure if power management could not be configured. Use fuchsia.power.SuspendEnabled to
  // ignore this failure when expected.
  // Register power configs from the board driver with Power Broker, and begin the continuous
  // power level adjustment of hardware. For boards/products that don't support the Power Framework,
  // this method simply returns success.
  zx::result<> ConfigurePowerManagement();

  // Acquires a lease on a power element via the supplied |lessor_client|, returning the resulting
  // lease control client end.
  zx::result<fidl::ClientEnd<fuchsia_power_broker::LeaseControl>> AcquireLease(
      const fidl::WireSyncClient<fuchsia_power_broker::Lessor> &lessor_client);

  // Informs Power Broker of the updated |power_level| via the supplied |current_level_client|.
  void UpdatePowerLevel(
      const fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel> &current_level_client,
      fuchsia_power_broker::PowerLevel power_level);

  // Watches the required hardware power level and adjusts it accordingly. Also serves requests that
  // were delayed because they were received during suspended state. Communicates power level
  // transitions to the Power Broker.
  void WatchHardwareRequiredLevel();

  // Watches the required wake-on-request power level and replies to the Power Broker accordingly.
  // Does not directly effect any real power level change of storage hardware. (That happens in
  // WatchHardwareRequiredLevel().)
  void WatchWakeOnRequestRequiredLevel();

  void Serve(fidl::ServerEnd<fuchsia_hardware_ufs::Ufs> server);

  // fidl::WireServer<fuchsia_hardware_ufs::Ufs>
  void ReadDescriptor(ReadDescriptorRequestView request,
                      ReadDescriptorCompleter::Sync &completer) override;
  void WriteDescriptor(WriteDescriptorRequestView request,
                       WriteDescriptorCompleter::Sync &completer) override;
  void ReadFlag(ReadFlagRequestView request, ReadFlagCompleter::Sync &completer) override;
  void SetFlag(SetFlagRequestView request, SetFlagCompleter::Sync &completer) override;
  void ClearFlag(ClearFlagRequestView request, ClearFlagCompleter::Sync &completer) override;
  void ToggleFlag(ToggleFlagRequestView request, ToggleFlagCompleter::Sync &completer) override;
  void ReadAttribute(ReadAttributeRequestView request,
                     ReadAttributeCompleter::Sync &completer) override;
  void WriteAttribute(WriteAttributeRequestView request,
                      WriteAttributeCompleter::Sync &completer) override;
  void SendUicCommand(SendUicCommandRequestView request,
                      SendUicCommandCompleter::Sync &completer) override;
  void Request(RequestRequestView request, RequestCompleter::Sync &completer) override;

  fidl::WireSyncClient<fuchsia_hardware_pci::Device> pci_;
  zx::vmo mmio_buffer_vmo_;
  uint64_t mmio_buffer_size_ = 0;
  std::optional<fdf::MmioBuffer> mmio_;
  fuchsia_hardware_pci::InterruptMode irq_mode_;
  zx::interrupt irq_;
  zx::bti bti_;

  inspect::Node inspect_node_;

  std::mutex commands_lock_;
  // The pending list consists of commands that have been received via QueueIoCommand() and are
  // waiting for IO to start.
  list_node_t pending_commands_ TA_GUARDED(commands_lock_);

  // Notifies AdminThread() that it has work to do. Signaled from QueueIoCommand() or the IRQ
  // handler.
  sync_completion_t admin_signal_;
  // Notifies IoThread() that it has work to do. Signaled from QueueIoCommand() or the IRQ handler.
  sync_completion_t io_signal_;

  // Dispatcher for processing queued block requests.
  fdf::Dispatcher irq_worker_dispatcher_;
  fdf::Dispatcher io_worker_dispatcher_;
  fdf::Dispatcher admin_worker_dispatcher_;
  fdf::Dispatcher exception_event_dispatcher_;
  // Signaled when worker_dispatcher_ is shut down.
  libsync::Completion irq_worker_shutdown_completion_;
  libsync::Completion io_worker_shutdown_completion_;
  libsync::Completion admin_worker_shutdown_completion_;
  libsync::Completion exception_event_completion_;
  // Signaled when power has been resumed.
  libsync::Completion wait_for_power_resumed_;

  std::unique_ptr<DeviceManager> device_manager_;
  std::unique_ptr<TransferRequestProcessor> transfer_request_processor_;
  std::unique_ptr<TaskManagementRequestProcessor> task_management_request_processor_;

  // Controller internal information.
  uint32_t logical_unit_count_ = 0;

  // The luns of the well-known logical units that exist on the UFS device.
  std::unordered_set<WellKnownLuns> well_known_lun_set_;

  // Callback function to perform when the host controller is notified.
  HostControllerCallback host_controller_callback_;

  bool driver_shutdown_ TA_GUARDED(lock_) = false;
  bool disable_completion_ = false;

  // The maximum transfer size supported by UFSHCI spec is 65535 * 256 KiB. However, we limit the
  // maximum transfer size to 1MiB for performance reason.
  uint32_t max_transfer_bytes_ = kMaxTransferSize1MiB;

  bool qemu_quirk_ = false;

  std::mutex lock_;

  ufs_config::Config config_;

  // Record the variable inspects.
  InspectProperties properties_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> root_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> node_controller_;

  std::vector<zx::event> assertive_power_dep_tokens_;
  std::vector<zx::event> opportunistic_power_dep_tokens_;

  fidl::WireSyncClient<fuchsia_power_broker::ElementControl> hardware_power_element_control_client_;
  fidl::WireSyncClient<fuchsia_power_broker::Lessor> hardware_power_lessor_client_;
  fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel> hardware_power_current_level_client_;
  fidl::WireClient<fuchsia_power_broker::RequiredLevel> hardware_power_required_level_client_;
  zx::event hardware_power_assertive_token_;

  fidl::WireSyncClient<fuchsia_power_broker::ElementControl>
      wake_on_request_element_control_client_;
  fidl::WireSyncClient<fuchsia_power_broker::Lessor> wake_on_request_lessor_client_;
  fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel> wake_on_request_current_level_client_;
  fidl::WireClient<fuchsia_power_broker::RequiredLevel> wake_on_request_required_level_client_;

  fidl::ClientEnd<fuchsia_power_broker::LeaseControl> hardware_power_lease_control_client_end_;

  fidl::ServerBindingGroup<fuchsia_hardware_ufs::Ufs> bindings_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_UFS_H_
