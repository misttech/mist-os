// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ufs.h"

#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <lib/driver/power/cpp/element-description-builder.h>
#include <lib/driver/power/cpp/power-support.h>
#include <lib/fit/defer.h>
#include <lib/trace/event.h>
#include <zircon/errors.h>

#include <array>
#include <mutex>
#include <vector>

#include <safemath/safe_conversions.h>

namespace ufs {
namespace {

// TODO(b/329588116): Relocate this power config.
// This power element represents the SDMMC controller hardware. Its opportunistic dependency on
// SAG's (Execution State, wake handling) allows for orderly power down of the hardware before the
// CPU suspends scheduling.
fuchsia_hardware_power::PowerElementConfiguration GetHardwarePowerConfig() {
  auto transitions_from_off =
      std::vector<fuchsia_hardware_power::Transition>{fuchsia_hardware_power::Transition{{
          .target_level = Ufs::kPowerLevelOn,
          // TODO(b/42075643): Fill it in later with appropriate numbers.
          .latency_us = 0,
      }}};
  auto transitions_from_on =
      std::vector<fuchsia_hardware_power::Transition>{fuchsia_hardware_power::Transition{{
          .target_level = Ufs::kPowerLevelOff,
          // TODO(b/42075643): Fill it in later with appropriate numbers.
          .latency_us = 0,
      }}};
  fuchsia_hardware_power::PowerLevel off = {
      {.level = Ufs::kPowerLevelOff, .name = "off", .transitions = transitions_from_off}};
  fuchsia_hardware_power::PowerLevel on = {
      {.level = Ufs::kPowerLevelOn, .name = "on", .transitions = transitions_from_on}};
  fuchsia_hardware_power::PowerElement hardware_power = {{
      .name = Ufs::kHardwarePowerElementName,
      .levels = {{off, on}},
  }};

  fuchsia_hardware_power::LevelTuple on_to_wake_handling = {{
      .child_level = Ufs::kPowerLevelOn,
      .parent_level = static_cast<uint8_t>(fuchsia_power_system::ExecutionStateLevel::kSuspending),
  }};
  fuchsia_hardware_power::PowerDependency opportunistic_on_exec_state_wake_handling = {{
      .child = Ufs::kHardwarePowerElementName,
      .parent = fuchsia_hardware_power::ParentElement::WithSag(
          fuchsia_hardware_power::SagElement::kExecutionState),
      .level_deps = {{on_to_wake_handling}},
      .strength = fuchsia_hardware_power::RequirementType::kOpportunistic,
  }};

  fuchsia_hardware_power::PowerElementConfiguration hardware_power_config = {
      {.element = hardware_power, .dependencies = {{opportunistic_on_exec_state_wake_handling}}}};
  return hardware_power_config;
}

// TODO(b/329588116): Relocate this power config.
// This power element does not represent real hardware. Its active dependency on SAG's
// (Wake Handling, active) is used to secure the (Execution State, wake handling) necessary to
// satisfy the hardware power element's dependency, thus allowing the hardware to wake up and serve
// incoming requests.
fuchsia_hardware_power::PowerElementConfiguration GetSystemWakeOnRequestPowerConfig() {
  auto transitions_from_off =
      std::vector<fuchsia_hardware_power::Transition>{fuchsia_hardware_power::Transition{{
          .target_level = Ufs::kPowerLevelOn,
          .latency_us = 0,
      }}};
  auto transitions_from_on =
      std::vector<fuchsia_hardware_power::Transition>{fuchsia_hardware_power::Transition{{
          .target_level = Ufs::kPowerLevelOff,
          .latency_us = 0,
      }}};
  fuchsia_hardware_power::PowerLevel off = {
      {.level = Ufs::kPowerLevelOff, .name = "off", .transitions = transitions_from_off}};
  fuchsia_hardware_power::PowerLevel on = {
      {.level = Ufs::kPowerLevelOn, .name = "on", .transitions = transitions_from_on}};
  fuchsia_hardware_power::PowerElement wake_on_request = {{
      .name = Ufs::kSystemWakeOnRequestPowerElementName,
      .levels = {{off, on}},
  }};

  fuchsia_hardware_power::LevelTuple on_to_active = {{
      .child_level = Ufs::kPowerLevelOn,
      .parent_level = static_cast<uint8_t>(fuchsia_power_system::ExecutionStateLevel::kSuspending),
  }};
  fuchsia_hardware_power::PowerDependency assertive_on_wake_handling_active = {{
      .child = Ufs::kSystemWakeOnRequestPowerElementName,
      .parent = fuchsia_hardware_power::ParentElement::WithSag(
          fuchsia_hardware_power::SagElement::kExecutionState),
      .level_deps = {{on_to_active}},
      .strength = fuchsia_hardware_power::RequirementType::kAssertive,
  }};

  fuchsia_hardware_power::PowerElementConfiguration wake_on_request_config = {
      {.element = wake_on_request, .dependencies = {{assertive_on_wake_handling_active}}}};
  return wake_on_request_config;
}

// TODO(b/329588116): Relocate this power config.
std::vector<fuchsia_hardware_power::PowerElementConfiguration> GetAllPowerConfigs() {
  return std::vector<fuchsia_hardware_power::PowerElementConfiguration>{
      GetHardwarePowerConfig(), GetSystemWakeOnRequestPowerConfig()};
}

}  // namespace

zx::result<fidl::ClientEnd<fuchsia_power_broker::LeaseControl>> Ufs::AcquireLease(
    const fidl::WireSyncClient<fuchsia_power_broker::Lessor>& lessor_client) {
  const fidl::WireResult result = lessor_client->Lease(kPowerLevelOn);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Call to Lease failed: %s", result.status_string());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    switch (result->error_value()) {
      case fuchsia_power_broker::LeaseError::kInternal:
        FDF_LOG(ERROR, "Lease returned internal error.");
        break;
      case fuchsia_power_broker::LeaseError::kNotAuthorized:
        FDF_LOG(ERROR, "Lease returned not authorized error.");
        break;
      default:
        FDF_LOG(ERROR, "Lease returned unknown error.");
        break;
    }
    return zx::error(ZX_ERR_INTERNAL);
  }
  if (!result->value()->lease_control.is_valid()) {
    FDF_LOG(ERROR, "Lease returned invalid lease control client end.");
    return zx::error(ZX_ERR_BAD_STATE);
  }
  return zx::ok(std::move(result->value()->lease_control));
}

void Ufs::UpdatePowerLevel(
    const fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel>& current_level_client,
    fuchsia_power_broker::PowerLevel power_level) {
  const fidl::WireResult result = current_level_client->Update(power_level);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Call to Update failed: %s", result.status_string());
  } else if (result->is_error()) {
    FDF_LOG(ERROR, "Update returned failure.");
  }
}

zx::result<> Ufs::NotifyEventCallback(NotifyEvent event, uint64_t data) {
  switch (event) {
    // This should all be done by the bootloader at start up and not reperformed.
    case NotifyEvent::kInit:
    // This is normally done at init, but isn't necessary.
    case NotifyEvent::kReset:
    case NotifyEvent::kPreLinkStartup:
    case NotifyEvent::kPostLinkStartup:
    case NotifyEvent::kDeviceInitDone:
    case NotifyEvent::kSetupTransferRequestList:
    case NotifyEvent::kSetupTaskManagementRequestList:
    case NotifyEvent::kPrePowerModeChange:
    case NotifyEvent::kPostPowerModeChange:
      return zx::ok();
    default:
      return zx::error(ZX_ERR_INVALID_ARGS);
  };
}

zx::result<> Ufs::Notify(NotifyEvent event, uint64_t data) {
  if (!host_controller_callback_) {
    return zx::error(ZX_ERR_BAD_HANDLE);
  }
  return host_controller_callback_(event, data);
}

zx_status_t Ufs::WaitWithTimeout(fit::function<zx_status_t()> wait_for, uint32_t timeout_us,
                                 const fbl::String& timeout_message) {
  uint32_t time_left = timeout_us;
  while (true) {
    if (wait_for()) {
      return ZX_OK;
    }
    if (time_left == 0) {
      FDF_LOG(ERROR, "%s after %u usecs", timeout_message.begin(), timeout_us);
      return ZX_ERR_TIMED_OUT;
    }
    usleep(1);
    time_left--;
  }
}

zx::result<> Ufs::AllocatePages(zx::vmo& vmo, fzl::VmoMapper& mapper, size_t size) {
  const uint32_t data_size =
      fbl::round_up(safemath::checked_cast<uint32_t>(size), zx_system_get_page_size());
  if (zx_status_t status = zx::vmo::create(data_size, 0, &vmo); status != ZX_OK) {
    return zx::error(status);
  }

  if (zx_status_t status = mapper.Map(vmo, 0, data_size); status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to map IO buffer: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok();
}

zx::result<uint16_t> Ufs::TranslateUfsLunToScsiLun(uint8_t ufs_lun) {
  // Logical unit
  if (!(ufs_lun & kUfsWellKnownlunId)) {
    if (ufs_lun > kMaxLunIndex) {
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }
    return zx::ok(ufs_lun);
  }

  // Well known logical unit
  return zx::ok(static_cast<uint16_t>((static_cast<uint16_t>(ufs_lun) & ~kUfsWellKnownlunId) |
                                      kScsiWellKnownLunId));
}

zx::result<uint8_t> Ufs::TranslateScsiLunToUfsLun(uint16_t scsi_lun) {
  constexpr uint16_t kScsiWellKownLunIndicatorField = 0xff00;
  // Well known logical unit
  if ((scsi_lun & kScsiWellKownLunIndicatorField) == kScsiWellKnownLunId) {
    return zx::ok(static_cast<uint8_t>((scsi_lun & kMaxLunId) | kUfsWellKnownlunId));
  }

  // Logical unit
  if ((scsi_lun & kScsiWellKownLunIndicatorField) != 0) {
    FDF_LOG(ERROR, "Invalid scsi lun: 0x%x", scsi_lun);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if ((scsi_lun & kMaxLunId) > kMaxLunIndex) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }
  return zx::ok(static_cast<uint8_t>(scsi_lun & kMaxLunId));
}

void Ufs::ProcessIoSubmissions() {
  while (true) {
    IoCommand* io_cmd;
    {
      std::lock_guard<std::mutex> lock(commands_lock_);
      io_cmd = list_remove_head_type(&pending_commands_, IoCommand, node);
    }

    if (io_cmd == nullptr) {
      return;
    }

    DataDirection data_direction = DataDirection::kNone;
    if (io_cmd->is_write) {
      data_direction = DataDirection::kHostToDevice;
    } else if (io_cmd->device_op.op.command.opcode == BLOCK_OPCODE_READ) {
      data_direction = DataDirection::kDeviceToHost;
    }

    std::optional<zx::unowned_vmo> vmo_optional = std::nullopt;
    uint32_t transfer_bytes = 0;
    if (data_direction != DataDirection::kNone) {
      if (io_cmd->device_op.op.command.opcode == BLOCK_OPCODE_TRIM) {
        // For the UNMAP command, a data buffer is required for the parameter list.
        zx::vmo data_vmo;
        fzl::VmoMapper mapper;
        if (zx::result<> result = AllocatePages(data_vmo, mapper, io_cmd->data_length);
            result.is_error()) {
          FDF_LOG(ERROR, "Failed to allocate data buffer (command %p): %s", io_cmd,
                  result.status_string());
          return;
        }
        memcpy(mapper.start(), io_cmd->data_buffer, io_cmd->data_length);
        vmo_optional = zx::unowned_vmo(data_vmo);
        io_cmd->data_vmo = std::move(data_vmo);

        transfer_bytes = io_cmd->data_length;
      } else {
        vmo_optional = zx::unowned_vmo(io_cmd->device_op.op.rw.vmo);

        transfer_bytes = io_cmd->device_op.op.rw.length * io_cmd->block_size_bytes;
      }
    }

    if (transfer_bytes > max_transfer_bytes_) {
      FDF_LOG(ERROR,
              "Request exceeding max transfer size. transfer_bytes=%d, max_transfer_bytes_=%d",
              transfer_bytes, max_transfer_bytes_);
      io_cmd->device_op.Complete(ZX_ERR_INVALID_ARGS);
      continue;
    }

    ScsiCommandUpiu upiu(io_cmd->cdb_buffer, io_cmd->cdb_length, data_direction, transfer_bytes);
    auto response =
        transfer_request_processor_->SendScsiUpiu(upiu, io_cmd->lun, vmo_optional, io_cmd);
    if (response.is_error()) {
      if (response.error_value() == ZX_ERR_NO_RESOURCES) {
        std::lock_guard<std::mutex> lock(commands_lock_);
        list_add_head(&pending_commands_, &io_cmd->node);
        return;
      }
      FDF_LOG(ERROR, "Failed to submit SCSI command (command %p): %s", io_cmd,
              response.status_string());
      io_cmd->device_op.Complete(response.error_value());
      io_cmd->data_vmo.reset();
    }
  }
}

void Ufs::ProcessAdminCompletions() { transfer_request_processor_->AdminRequestCompletion(); }

void Ufs::ProcessIoCompletions() { transfer_request_processor_->IoRequestCompletion(); }

zx::result<> Ufs::Isr() {
  const fdf::MmioBuffer& mmio = mmio_.value();
  auto interrupt_status = InterruptStatusReg::Get().ReadFrom(&mmio);

  // TODO(https://fxbug.dev/42075643): implement error handlers
  if (interrupt_status.uic_error()) {
    FDF_LOG(ERROR, "UFS: UIC error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_uic_error(true).WriteTo(&mmio);

    // UECPA for Host UIC Error Code within PHY Adapter Layer.
    if (HostUicErrorCodePhyAdapterLayerReg::Get().ReadFrom(&mmio).uic_phy_adapter_layer_error()) {
      FDF_LOG(ERROR, "UECPA error code: 0x%x",
              HostUicErrorCodePhyAdapterLayerReg::Get()
                  .ReadFrom(&mmio)
                  .uic_phy_adapter_layer_error_code());
    }
    // UECDL for Host UIC Error Code within Data Link Layer.
    if (HostUicErrorCodeDataLinkLayerReg::Get().ReadFrom(&mmio).uic_data_link_layer_error()) {
      FDF_LOG(
          ERROR, "UECDL error code: 0x%x",
          HostUicErrorCodeDataLinkLayerReg::Get().ReadFrom(&mmio).uic_data_link_layer_error_code());
    }
    // UECN for Host UIC Error Code within Network Layer.
    if (HostUicErrorCodeNetworkLayerReg::Get().ReadFrom(&mmio).uic_network_layer_error()) {
      FDF_LOG(
          ERROR, "UECN error code: 0x%x",
          HostUicErrorCodeNetworkLayerReg::Get().ReadFrom(&mmio).uic_network_layer_error_code());
    }
    // UECT for Host UIC Error Code within Transport Layer.
    if (HostUicErrorCodeTransportLayerReg::Get().ReadFrom(&mmio).uic_transport_layer_error()) {
      FDF_LOG(ERROR, "UECT error code: 0x%x",
              HostUicErrorCodeTransportLayerReg::Get()
                  .ReadFrom(&mmio)
                  .uic_transport_layer_error_code());
    }
    // UECDME for Host UIC Error Code within DME subcomponent.
    if (HostUicErrorCodeReg::Get().ReadFrom(&mmio).uic_dme_error()) {
      FDF_LOG(ERROR, "UECDME error code: 0x%x",
              HostUicErrorCodeReg::Get().ReadFrom(&mmio).uic_dme_error_code());
    }
  }
  if (interrupt_status.device_fatal_error_status()) {
    FDF_LOG(ERROR, "UFS: Device fatal error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_device_fatal_error_status(true).WriteTo(&mmio);
  }
  if (interrupt_status.host_controller_fatal_error_status()) {
    FDF_LOG(ERROR, "UFS: Host controller fatal error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_host_controller_fatal_error_status(true).WriteTo(
        &mmio);
  }
  if (interrupt_status.system_bus_fatal_error_status()) {
    FDF_LOG(ERROR, "UFS: System bus fatal error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_system_bus_fatal_error_status(true).WriteTo(&mmio);
  }
  if (interrupt_status.crypto_engine_fatal_error_status()) {
    FDF_LOG(ERROR, "UFS: Crypto engine fatal error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_crypto_engine_fatal_error_status(true).WriteTo(
        &mmio);
  }
  // Handle command completion interrupts.
  if (interrupt_status.utp_transfer_request_completion_status()) {
    InterruptStatusReg::Get().FromValue(0).set_utp_transfer_request_completion_status(true).WriteTo(
        &mmio);
    auto& request_list = transfer_request_processor_->GetRequestList();
    SlotState admin_slot_state = request_list.GetSlot(kAdminCommandSlotNumber).state;
    uint32_t door_bell = UtrListDoorBellReg::Get().ReadFrom(&mmio).door_bell();
    bool admin_door_bell = door_bell & (1 << kAdminCommandSlotNumber);

    if (admin_slot_state == SlotState::kScheduled && !admin_door_bell) {
      sync_completion_signal(&admin_signal_);

      // TODO(b/42075643) Check that the interrupt also has I/O completion.
    } else {
      sync_completion_signal(&io_signal_);
    }
  }
  if (interrupt_status.utp_task_management_request_completion_status()) {
    InterruptStatusReg::Get()
        .FromValue(0)
        .set_utp_task_management_request_completion_status(true)
        .WriteTo(&mmio);
    task_management_request_processor_->IoRequestCompletion();
  }
  if (interrupt_status.uic_command_completion_status()) {
    // TODO(https://fxbug.dev/42075643): Handle UIC completion
    FDF_LOG(ERROR, "UFS: UIC completion not yet implemented");
  }

  return zx::ok();
}

int Ufs::IrqLoop() {
  while (true) {
    if (zx_status_t status = irq_.wait(nullptr); status != ZX_OK) {
      if (status == ZX_ERR_CANCELED) {
        FDF_LOG(DEBUG, "Interrupt cancelled. Exiting IRQ loop.");
      } else {
        FDF_LOG(ERROR, "Failed to wait for interrupt: %s", zx_status_get_string(status));
      }
      break;
    }

    if (zx::result<> result = Isr(); result.is_error()) {
      FDF_LOG(ERROR, "Failed to run interrupt service routine: %s", result.status_string());
    }

    if (irq_mode_ == fuchsia_hardware_pci::InterruptMode::kLegacy) {
      const fidl::WireResult result = pci_->AckInterrupt();
      if (!result.ok()) {
        FDF_LOG(ERROR, "Call to AckInterrupt failed: %s", result.status_string());
        break;
      }
      if (result->is_error()) {
        FDF_LOG(ERROR, "AckInterrupt failed: %s", zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    }
  }
  return thrd_success;
}

int Ufs::AdminLoop() {
  while (true) {
    if (zx_status_t status = sync_completion_wait(&admin_signal_, ZX_TIME_INFINITE);
        status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to wait for sync completion: %s", zx_status_get_string(status));
      break;
    }
    sync_completion_reset(&admin_signal_);

    {
      std::lock_guard<std::mutex> lock(lock_);
      if (driver_shutdown_) {
        FDF_LOG(DEBUG, "Admin thread exiting.");
        break;
      }
    }

    if (!disable_completion_) {
      ProcessAdminCompletions();
    }
  }
  return thrd_success;
}

int Ufs::IoLoop() {
  while (true) {
    if (zx_status_t status = sync_completion_wait(&io_signal_, ZX_TIME_INFINITE); status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to wait for sync completion: %s", zx_status_get_string(status));
      break;
    }
    sync_completion_reset(&io_signal_);

    bool wake_on_request = false;
    fidl::ClientEnd<fuchsia_power_broker::LeaseControl> wake_on_request_lease_control_client_end;

    {
      std::lock_guard<std::mutex> lock(lock_);
      if (driver_shutdown_) {
        FDF_LOG(DEBUG, "IO thread exiting.");
        break;
      }

      if (!device_manager_->IsResumed()) {
        wake_on_request = true;
        wait_for_power_resumed_.Reset();

        // Acquire lease on wake-on-request power element. This indirectly raises SAG's Execution
        // State, satisfying the hardware power element's lease status (which is opportunistically
        // dependent on SAG's Execution State), and thus resuming power.
        ZX_ASSERT(!wake_on_request_lease_control_client_end.is_valid());
        zx::result lease_control_client_end = AcquireLease(wake_on_request_lessor_client_);
        if (!lease_control_client_end.is_ok()) {
          FDF_LOG(ERROR, "Failed to acquire lease during wake-on-request: %s",
                  zx_status_get_string(lease_control_client_end.status_value()));
          return thrd_error;
        };
        wake_on_request_lease_control_client_end = std::move(lease_control_client_end.value());
        ZX_ASSERT(wake_on_request_lease_control_client_end.is_valid());

        properties_.wake_on_request_count.Add(1);

        lock_.unlock();
        wait_for_power_resumed_.Wait();
        lock_.lock();
      }
    }

    // TODO(https://fxbug.dev/42075643): We need to perform a I/O completion on the in-flight I/O
    // before the device is suspended.
    if (!disable_completion_) {
      ProcessIoCompletions();
    }

    // Unable to send I/O when in suspend.
    if (device_manager_->IsResumed()) {
      ProcessIoSubmissions();
    }

    if (wake_on_request) {
      // Drop lease on wake-on-request power element. This lets the hardware power element's lease
      // status revert to pending, unless there are other entities that are raising SAG's
      // Execution State.
      ZX_ASSERT(wake_on_request_lease_control_client_end.is_valid());
      wake_on_request_lease_control_client_end.channel().reset();
      ZX_ASSERT(!wake_on_request_lease_control_client_end.is_valid());
    }
  }
  return thrd_success;
}

void Ufs::ExecuteCommandAsync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                              uint32_t block_size_bytes, scsi::DeviceOp* device_op, iovec data) {
  IoCommand* io_cmd = containerof(device_op, IoCommand, device_op);
  if (cdb.iov_len > sizeof(io_cmd->cdb_buffer)) {
    device_op->Complete(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  auto lun_id = TranslateScsiLunToUfsLun(lun);
  if (lun_id.is_error()) {
    device_op->Complete(lun_id.status_value());
    return;
  }

  memcpy(io_cmd->cdb_buffer, cdb.iov_base, cdb.iov_len);
  io_cmd->cdb_length = safemath::checked_cast<uint8_t>(cdb.iov_len);
  io_cmd->lun = lun_id.value();
  io_cmd->block_size_bytes = block_size_bytes;
  io_cmd->is_write = is_write;

  // Currently, data is only used in the UNMAP command.
  if (device_op->op.command.opcode == BLOCK_OPCODE_TRIM && data.iov_len != 0) {
    if (sizeof(io_cmd->data_buffer) != data.iov_len) {
      FDF_LOG(ERROR,
              "The size of the requested data buffer(%zu) and data_buffer(%lu) are different.",
              data.iov_len, sizeof(io_cmd->data_buffer));
      device_op->Complete(ZX_ERR_INVALID_ARGS);
      return;
    }
    memcpy(io_cmd->data_buffer, data.iov_base, data.iov_len);
    io_cmd->data_length = static_cast<uint8_t>(data.iov_len);
  }

  // Queue transaction.
  {
    std::lock_guard<std::mutex> lock(commands_lock_);
    list_add_tail(&pending_commands_, &io_cmd->node);
  }
  sync_completion_signal(&io_signal_);
}

zx_status_t Ufs::ExecuteCommandSync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                                    iovec data) {
  auto lun_id = TranslateScsiLunToUfsLun(lun);
  if (lun_id.is_error()) {
    return lun_id.status_value();
  }

  if (data.iov_len > max_transfer_bytes_) {
    FDF_LOG(ERROR,
            "Request exceeding max transfer size. transfer_bytes=%zu, max_transfer_bytes_=%d",
            data.iov_len, max_transfer_bytes_);
    return ZX_ERR_INVALID_ARGS;
  }

  DataDirection data_direction = DataDirection::kNone;
  if (is_write) {
    data_direction = DataDirection::kHostToDevice;
  } else if (data.iov_base != nullptr) {
    data_direction = DataDirection::kDeviceToHost;
  }

  std::optional<zx::unowned_vmo> vmo_optional = std::nullopt;
  zx::vmo data_vmo;
  fzl::VmoMapper mapper;
  if (data_direction != DataDirection::kNone) {
    // Allocate a response data buffer.
    // TODO(https://fxbug.dev/42075643): We need to pre-allocate a data buffer that will be used in
    // the Sync command.
    if (zx::result<> result = AllocatePages(data_vmo, mapper, data.iov_len); result.is_error()) {
      return result.error_value();
    }
    vmo_optional = zx::unowned_vmo(data_vmo);
  }

  if (data_direction == DataDirection::kHostToDevice) {
    memcpy(mapper.start(), data.iov_base, data.iov_len);
  }

  ScsiCommandUpiu upiu(static_cast<uint8_t*>(cdb.iov_base),
                       safemath::checked_cast<uint8_t>(cdb.iov_len), data_direction,
                       safemath::checked_cast<uint32_t>(data.iov_len));
  if (auto response = transfer_request_processor_->SendScsiUpiu(upiu, lun_id.value(), vmo_optional);
      response.is_error()) {
    // Get the previous response from the admin slot.
    auto response_upiu = std::make_unique<ResponseUpiu>(
        transfer_request_processor_->GetRequestList().GetDescriptorBuffer(
            kAdminCommandSlotNumber, ScsiCommandUpiu::GetResponseOffset()));
    auto* response_data =
        reinterpret_cast<scsi::FixedFormatSenseDataHeader*>(response_upiu->GetSenseData());
    if (response_data->sense_key() != scsi::SenseKey::UNIT_ATTENTION) {
      FDF_LOG(ERROR, "Failed to send SCSI command: %s", response.status_string());
      return response.error_value();
    }
    // Returns ZX_ERR_UNAVAILABLE if a unit attention error.
    return ZX_ERR_UNAVAILABLE;
  }

  if (data_direction == DataDirection::kDeviceToHost) {
    memcpy(data.iov_base, mapper.start(), data.iov_len);
  }
  return ZX_OK;
}

// Record the constant inspects.
void Ufs::PopulateVersionInspect(inspect::Node* inspect_node) {
  const fdf::MmioBuffer& mmio = mmio_.value();
  VersionReg version_reg = VersionReg::Get().ReadFrom(&mmio);

  auto version = inspect_node->CreateChild("version");
  properties_.major_version_number =
      version.CreateUint("major_version_number", version_reg.major_version_number());
  properties_.minor_version_number =
      version.CreateUint("minor_version_number", version_reg.minor_version_number());
  properties_.version_suffix = version.CreateUint("version_suffix", version_reg.version_suffix());
  inspector().inspector().emplace(std::move(version));

  FDF_LOG(INFO, "Controller version %u.%u found", version_reg.major_version_number(),
          version_reg.minor_version_number());
}

// Record the constant inspects.
void Ufs::PopulateCapabilitiesInspect(inspect::Node* inspect_node) {
  const fdf::MmioBuffer& mmio = mmio_.value();
  CapabilityReg caps_reg = CapabilityReg::Get().ReadFrom(&mmio);

  auto caps = inspect_node->CreateChild("capabilities");
  properties_.crypto_support = caps.CreateBool("crypto_support", caps_reg.crypto_support());
  properties_.uic_dme_test_mode_command_supported = caps.CreateBool(
      "uic_dme_test_mode_command_supported", caps_reg.uic_dme_test_mode_command_supported());
  properties_.out_of_order_data_delivery_supported = caps.CreateBool(
      "out_of_order_data_delivery_supported", caps_reg.out_of_order_data_delivery_supported());
  properties_._64_bit_addressing_supported =
      caps.CreateBool("64_bit_addressing_supported", caps_reg._64_bit_addressing_supported());
  properties_.auto_hibernation_support =
      caps.CreateBool("auto_hibernation_support", caps_reg.auto_hibernation_support());
  properties_.number_of_utp_task_management_request_slots =
      caps.CreateUint("number_of_utp_task_management_request_slots",
                      caps_reg.number_of_utp_task_management_request_slots());
  properties_.number_of_outstanding_rtt_requests_supported =
      caps.CreateUint("number_of_outstanding_rtt_requests_supported",
                      caps_reg.number_of_outstanding_rtt_requests_supported());
  properties_.number_of_utp_transfer_request_slots = caps.CreateUint(
      "number_of_utp_transfer_request_slots", caps_reg.number_of_utp_transfer_request_slots());
  inspector().inspector().emplace(std::move(caps));
}

zx::result<> Ufs::InitMmioBuffer() {
  auto mmio = CreateMmioBuffer(0, mmio_buffer_size_, std::move(mmio_buffer_vmo_));
  if (mmio.is_error()) {
    return zx::error(mmio.status_value());
  }
  mmio_ = std::move(mmio.value());
  return zx::ok();
}

zx_status_t Ufs::Init() {
  list_initialize(&pending_commands_);

  if (zx::result<> result = InitMmioBuffer(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize MMIO buffer: %s", result.status_string());
    return result.error_value();
  }

  inspect_node_ = inspector().root().CreateChild("ufs");
  PopulateVersionInspect(&inspect_node_);
  PopulateCapabilitiesInspect(&inspect_node_);

  auto controller_node = inspect_node_.CreateChild("controller");
  auto wp_node = controller_node.CreateChild("write_protect");
  auto wb_node = controller_node.CreateChild("writebooster");
  auto bkop_node = controller_node.CreateChild("background_operations");

  if (zx::result<> result = InitQuirk(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize quirk: %s", result.status_string());
    return result.error_value();
  }
  if (zx::result<> result = InitController(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize UFS controller: %s", result.status_string());
    return result.error_value();
  }

  if (zx::result<> result = InitDeviceInterface(controller_node); result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize device interface: %s", result.status_string());
    return result.error_value();
  }

  if (zx::result<> result = device_manager_->GetControllerDescriptor(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to get controller descriptor: %s", result.status_string());
    return result.error_value();
  }

  if (zx::result<> result = device_manager_->ConfigureWriteProtect(wp_node); result.is_error()) {
    FDF_LOG(ERROR, "Failed to configure Write Protect %s", result.status_string());
    return result.error_value();
  }

  if (zx::result<> result = device_manager_->ConfigureBackgroundOp(bkop_node); result.is_error()) {
    FDF_LOG(ERROR, "Failed to configure Background Operations %s", result.status_string());
    return result.error_value();
  }

  if (zx::result<> result = device_manager_->ConfigureWriteBooster(wb_node); result.is_error()) {
    if (result.status_value() == ZX_ERR_NOT_SUPPORTED) {
      FDF_LOG(WARNING, "This device does not support WriteBooster");
    } else {
      FDF_LOG(ERROR, "Failed to configure WriteBooster %s", result.status_string());
      return result.error_value();
    }
  }

  // The maximum transfer size supported by UFSHCI spec is 65535 * 256 KiB. However, we limit the
  // maximum transfer size to 1MiB for performance reason.
  max_transfer_bytes_ = kMaxTransferSize1MiB;
  properties_.max_transfer_bytes =
      controller_node.CreateUint("max_transfer_bytes", max_transfer_bytes_);

  zx::result<uint32_t> lun_count;
  if (lun_count = AddLogicalUnits(); lun_count.is_error()) {
    FDF_LOG(ERROR, "Failed to scan logical units: %s", lun_count.status_string());
    return lun_count.error_value();
  }

  if (lun_count.value() == 0) {
    FDF_LOG(ERROR, "Bind Error. There is no available LUN(lun_count = 0).");
    return ZX_ERR_BAD_STATE;
  }
  logical_unit_count_ = lun_count.value();
  properties_.logical_unit_count =
      controller_node.CreateUint("logical_unit_count", logical_unit_count_);
  properties_.wake_on_request_count = inspect_node_.CreateUint("wake_on_request_count", 0);
  // 14 buckets spanning from 1us to ~8ms.
  properties_.wake_latency_us = inspect_node_.CreateExponentialUintHistogram(
      "wake_latency_us", /*floor=*/1, /*initial_step=*/1, /*step_multiplier=*/2,
      /*buckets=*/14);

  inspector().inspector().emplace(std::move(controller_node));
  inspector().inspector().emplace(std::move(wp_node));
  inspector().inspector().emplace(std::move(wb_node));
  inspector().inspector().emplace(std::move(bkop_node));
  FDF_LOG(INFO, "Bind Success");

  return ZX_OK;
}

zx::result<> Ufs::InitQuirk() {
  // Check PCI device quirk.
  if (pci_.is_valid()) {
    fuchsia_hardware_pci::wire::DeviceInfo info;
    const auto result = pci_->GetDeviceInfo();
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to get PCI device info: %s", result.status_string());
      return zx::error(result.status());
    }
    info = result->info;

    // Check that the current environment is QEMU.
    // Vendor ID = 0x1b36: Red Hat, Inc
    // Device ID = 0x0013: QEMU UFS Host Controller
    constexpr uint16_t kRedHatVendorId = 0x1b36;
    constexpr uint16_t kQemuUfsHostController = 0x0013;
    if ((info.vendor_id == kRedHatVendorId) && (info.device_id == kQemuUfsHostController)) {
      qemu_quirk_ = true;
    }
    FDF_LOG(INFO, "PCI device info: Vendor ID = 0x%x, Device ID = 0x%x", info.vendor_id,
            info.device_id);
  }

  return zx::ok();
}

zx::result<> Ufs::InitController() {
  const fdf::MmioBuffer& mmio = mmio_.value();
  // Disable all interrupts.
  InterruptEnableReg::Get().FromValue(0).WriteTo(&mmio);

  if (zx::result<> result = Notify(NotifyEvent::kReset, 0); result.is_error()) {
    return result.take_error();
  }
  // If UFS host controller is already enabled, disable it.
  if (HostControllerEnableReg::Get().ReadFrom(&mmio).host_controller_enable()) {
    DisableHostController();
  }
  if (zx_status_t status = EnableHostController(); status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to enable host controller %d", status);
    return zx::error(status);
  }

  // Create and post IRQ worker
  {
    auto irq_dispatcher = fdf::SynchronizedDispatcher::Create(
        fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "ufs-irq-worker",
        [&](fdf_dispatcher_t*) { irq_worker_shutdown_completion_.Signal(); });
    if (irq_dispatcher.is_error()) {
      FDF_LOG(ERROR, "Failed to create IRQ dispatcher: %s",
              zx_status_get_string(irq_dispatcher.status_value()));
      return zx::error(irq_dispatcher.status_value());
    }
    irq_worker_dispatcher_ = *std::move(irq_dispatcher);

    zx_status_t status =
        async::PostTask(irq_worker_dispatcher_.async_dispatcher(), [this] { IrqLoop(); });
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to start IRQ worker loop: %s", zx_status_get_string(status));
      return zx::error(status);
    }
  }

  // Notify platform UFS that we are going to init the UFS host controller.
  if (zx::result<> result = Notify(NotifyEvent::kInit, 0); result.is_error()) {
    return result.take_error();
  }

  // Create Task Management Request Processor
  uint8_t number_of_task_management_request_slots = safemath::checked_cast<uint8_t>(
      CapabilityReg::Get().ReadFrom(&mmio).number_of_utp_task_management_request_slots() + 1);
  FDF_LOG(DEBUG, "number_of_task_management_request_slots=%d",
          number_of_task_management_request_slots);

  auto task_management_request_processor =
      TaskManagementRequestProcessor::Create(*this, bti_.borrow(), mmio.View(0, mmio_buffer_size_),
                                             number_of_task_management_request_slots);
  if (task_management_request_processor.is_error()) {
    FDF_LOG(ERROR, "Failed to create task management request processor %s",
            task_management_request_processor.status_string());
    return task_management_request_processor.take_error();
  }
  task_management_request_processor_ = std::move(*task_management_request_processor);

  // Create Transfer Request Processor
  uint8_t number_of_transfer_request_slots = safemath::checked_cast<uint8_t>(
      CapabilityReg::Get().ReadFrom(&mmio).number_of_utp_transfer_request_slots() + 1);
  FDF_LOG(DEBUG, "number_of_transfer_request_slots=%d", number_of_transfer_request_slots);

  auto transfer_request_processor = TransferRequestProcessor::Create(
      *this, bti_.borrow(), mmio.View(0, mmio_buffer_size_), number_of_transfer_request_slots);
  if (transfer_request_processor.is_error()) {
    FDF_LOG(ERROR, "Failed to create transfer request processor %s",
            transfer_request_processor.status_string());
    return transfer_request_processor.take_error();
  }
  transfer_request_processor_ = std::move(*transfer_request_processor);

  auto device_manager = DeviceManager::Create(*this, *transfer_request_processor_, properties_);
  if (device_manager.is_error()) {
    FDF_LOG(ERROR, "Failed to create device manager %s", device_manager.status_string());
    return device_manager.take_error();
  }
  device_manager_ = std::move(*device_manager);

  // Create and post Admin worker
  {
    auto admin_dispatcher = fdf::SynchronizedDispatcher::Create(
        fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "ufs-admin-worker",
        [&](fdf_dispatcher_t*) { admin_worker_shutdown_completion_.Signal(); });
    if (admin_dispatcher.is_error()) {
      FDF_LOG(ERROR, "Failed to create Admin dispatcher: %s",
              zx_status_get_string(admin_dispatcher.status_value()));
      return zx::error(admin_dispatcher.status_value());
    }
    admin_worker_dispatcher_ = *std::move(admin_dispatcher);

    zx_status_t status =
        async::PostTask(admin_worker_dispatcher_.async_dispatcher(), [this] { AdminLoop(); });
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to start Admin worker loop: %s", zx_status_get_string(status));
      return zx::error(status);
    }
  }

  // Create and post IO worker
  {
    auto io_dispatcher = fdf::SynchronizedDispatcher::Create(
        fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "ufs-io-worker",
        [&](fdf_dispatcher_t*) { io_worker_shutdown_completion_.Signal(); });
    if (io_dispatcher.is_error()) {
      FDF_LOG(ERROR, "Failed to create IO dispatcher: %s",
              zx_status_get_string(io_dispatcher.status_value()));
      return zx::error(io_dispatcher.status_value());
    }
    io_worker_dispatcher_ = *std::move(io_dispatcher);

    zx_status_t status =
        async::PostTask(io_worker_dispatcher_.async_dispatcher(), [this] { IoLoop(); });
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to start IO worker loop: %s", zx_status_get_string(status));
      return zx::error(status);
    }
  }

  // Create Exception Event worker
  {
    auto ee_dispatcher = fdf::SynchronizedDispatcher::Create(
        fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "ufs-exception-event-worker",
        [&](fdf_dispatcher_t*) { exception_event_completion_.Signal(); });
    if (ee_dispatcher.is_error()) {
      FDF_LOG(ERROR, "Failed to create Exception Event dispatcher: %s",
              zx_status_get_string(ee_dispatcher.status_value()));
      return zx::error(ee_dispatcher.status_value());
    }
    exception_event_dispatcher_ = *std::move(ee_dispatcher);
  }

  return zx::ok();
}

zx::result<> Ufs::InitDeviceInterface(inspect::Node& controller_node) {
  const fdf::MmioBuffer& mmio = mmio_.value();

  // Enable error and UIC/UTP related interrupts.
  InterruptEnableReg::Get()
      .FromValue(0)
      .set_crypto_engine_fatal_error_enable(true)
      .set_system_bus_fatal_error_enable(true)
      .set_host_controller_fatal_error_enable(true)
      .set_utp_error_enable(true)
      .set_device_fatal_error_enable(true)
      .set_uic_command_completion_enable(false)  // The UIC command uses polling mode.
      .set_utp_task_management_request_completion_enable(true)
      .set_uic_link_startup_status_enable(false)  // Ignore link startup interrupt.
      .set_uic_link_lost_status_enable(true)
      .set_uic_hibernate_enter_status_enable(false)  // The hibernate commands use polling mode.
      .set_uic_hibernate_exit_status_enable(false)   // The hibernate commands use polling mode.
      .set_uic_power_mode_status_enable(false)       // The power mode uses polling mode.
      .set_uic_test_mode_status_enable(true)
      .set_uic_error_enable(true)
      .set_uic_dme_endpointreset(true)
      .set_utp_transfer_request_completion_enable(true)
      .WriteTo(&mmio);

  if (!HostControllerStatusReg::Get().ReadFrom(&mmio).uic_command_ready()) {
    FDF_LOG(ERROR, "UIC command is not ready\n");
    return zx::error(ZX_ERR_INTERNAL);
  }

  // Send Link Startup UIC command to start the link startup procedure.
  if (zx::result<> result = device_manager_->SendLinkStartUp(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to send Link Startup UIC command %s", result.status_string());
    return result.take_error();
  }

  // The |device_present| bit becomes true if the host controller has successfully received a Link
  // Startup UIC command response and the UFS device has found a physical link to the controller.
  if (!HostControllerStatusReg::Get().ReadFrom(&mmio).device_present()) {
    FDF_LOG(ERROR, "UFS device not found");
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  FDF_LOG(INFO, "UFS device found");

  if (zx::result<> result = task_management_request_processor_->Init(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize task management request processor %s",
            result.status_string());
    return result.take_error();
  }

  if (zx::result<> result = transfer_request_processor_->Init(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize transfer request processor %s", result.status_string());
    return result.take_error();
  }

  // TODO(https://fxbug.dev/42075643): Configure interrupt aggregation. (default 0)

  NopOutUpiu nop_upiu;
  auto nop_response = transfer_request_processor_->SendRequestUpiu<NopOutUpiu, NopInUpiu>(nop_upiu);
  if (nop_response.is_error()) {
    FDF_LOG(ERROR, "Failed to send NopInUpiu %s", nop_response.status_string());
    return nop_response.take_error();
  }

  if (zx::result<> result = device_manager_->DeviceInit(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize device %s", result.status_string());
    return result.take_error();
  }

  if (zx::result<> result = Notify(NotifyEvent::kDeviceInitDone, 0); result.is_error()) {
    return result.take_error();
  }

  if (zx::result<> result = device_manager_->InitReferenceClock(controller_node);
      result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize reference clock %s", result.status_string());
    return result.take_error();
  }

  auto unipro_node = controller_node.CreateChild("unipro");
  auto attributes_node = controller_node.CreateChild("attributes");
  if (qemu_quirk_) {
    // Currently, QEMU UFS devices do not support unipro and power mode.
    device_manager_->SetCurrentPowerMode(UfsPowerMode::kActive);
  } else {
    if (zx::result<> result = device_manager_->InitUniproAttributes(unipro_node);
        result.is_error()) {
      FDF_LOG(ERROR, "Failed to initialize Unipro attributes %s", result.status_string());
      return result.take_error();
    }

    if (zx::result<> result = device_manager_->InitUicPowerMode(unipro_node); result.is_error()) {
      FDF_LOG(ERROR, "Failed to initialize UIC power mode %s", result.status_string());
      return result.take_error();
    }

    if (zx::result<> result = device_manager_->InitUfsPowerMode(controller_node, attributes_node);
        result.is_error()) {
      FDF_LOG(ERROR, "Failed to initialize UFS power mode %s", result.status_string());
      return result.take_error();
    }
  }
  properties_.power_suspended = inspect_node_.CreateBool("power_suspended", false);

  zx::result<uint32_t> result = device_manager_->GetBootLunEnabled();
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to check Boot LUN enabled %s", result.status_string());
    return result.take_error();
  }
  properties_.b_boot_lun_en = attributes_node.CreateUint("bBootLunEn", result.value());

  // TODO(https://fxbug.dev/42075643): Set bMaxNumOfRTT (Read-to-transfer)

  inspector().inspector().emplace(std::move(unipro_node));
  inspector().inspector().emplace(std::move(attributes_node));

  return zx::ok();
}

zx::result<uint32_t> Ufs::AddLogicalUnits() {
  uint8_t max_luns = device_manager_->GetMaxLunCount();
  ZX_ASSERT(max_luns <= kMaxLunCount);

  auto read_unit_descriptor = [this](uint16_t lun, size_t block_size,
                                     uint64_t block_count) -> zx::result<> {
    if (lun > UINT8_MAX) {
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }

    zx::result<UnitDescriptor> unit_descriptor =
        device_manager_->ReadUnitDescriptor(static_cast<uint8_t>(lun));
    if (unit_descriptor.is_error()) {
      return unit_descriptor.take_error();
    }

    if (unit_descriptor->bLUEnable != 1) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    if (unit_descriptor->bLogicalBlockSize >= sizeof(size_t) * 8) {
      FDF_LOG(ERROR, "Cannot handle the unit descriptor bLogicalBlockSize = %d.",
              unit_descriptor->bLogicalBlockSize);
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }

    size_t desc_block_size = 1 << unit_descriptor->bLogicalBlockSize;
    uint64_t desc_block_count = betoh64(unit_descriptor->qLogicalBlockCount);

    if (desc_block_size < kBlockSize ||
        desc_block_size <
            static_cast<size_t>(device_manager_->GetGeometryDescriptor().bMinAddrBlockSize) *
                kSectorSize ||
        desc_block_size >
            static_cast<size_t>(device_manager_->GetGeometryDescriptor().bMaxInBufferSize) *
                kSectorSize ||
        desc_block_size >
            static_cast<size_t>(device_manager_->GetGeometryDescriptor().bMaxOutBufferSize) *
                kSectorSize) {
      FDF_LOG(ERROR, "Cannot handle logical block size of %zu.", desc_block_size);
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }
    ZX_ASSERT_MSG(desc_block_size == kBlockSize, "Currently, it only supports a 4KB block size.");

    if (desc_block_size != block_size || desc_block_count != block_count) {
      FDF_LOG(INFO,
              "Failed to check for disk consistency. (block_size=%zu/%zu, block_count=%ld/%ld)",
              desc_block_size, block_size, desc_block_count, block_count);
      return zx::error(ZX_ERR_BAD_STATE);
    }
    FDF_LOG(INFO, "LUN-%d block_size=%zu, block_count=%ld", lun, desc_block_size, desc_block_count);

    // Currently, we only support kPowerOnWriteProtect.
    if (device_manager_->IsPowerOnWritePotectEnabled() &&
        unit_descriptor->bLUWriteProtect == LUWriteProtect::kPowerOnWriteProtect &&
        !device_manager_->IsLogicalLunPowerOnWriteProtect()) {
      device_manager_->SetLogicalLunPowerOnWriteProtect(true);
    }

    return zx::ok();
  };

  // UFS does not support the MODE SENSE(6) command. We should use the MODE SENSE(10) command.
  // UFS does not support the READ(12)/WRITE(12) commands.
  scsi::DeviceOptions options(/*check_unmap_support*/ true, /*use_mode_sense_6*/ false,
                              /*use_read_write_12*/ false);

  zx::result<uint32_t> lun_count = ScanAndBindLogicalUnits(kPlaceholderTarget, max_transfer_bytes_,
                                                           max_luns, read_unit_descriptor, options);
  if (lun_count.is_error()) {
    FDF_LOG(ERROR, "Failed to scan logical units: %s", lun_count.status_string());
    return lun_count.take_error();
  }

  // Find well known logical units.
  std::array<WellKnownLuns, static_cast<uint8_t>(WellKnownLuns::kCount)> well_known_luns = {
      WellKnownLuns::kReportLuns, WellKnownLuns::kUfsDevice, WellKnownLuns::kBoot,
      WellKnownLuns::kRpmb};

  for (auto& lun : well_known_luns) {
    auto scsi_lun = TranslateUfsLunToScsiLun(static_cast<uint8_t>(lun));
    if (scsi_lun.is_error()) {
      return scsi_lun.take_error();
    }
    if (zx_status_t status = TestUnitReady(kPlaceholderTarget, scsi_lun.value()); status != ZX_OK) {
      continue;
    }
    well_known_lun_set_.insert(lun);
    FDF_LOG(INFO, "Well known LUN-0x%x", static_cast<uint8_t>(lun));
  }

  return zx::ok(lun_count.value());
}

void Ufs::DumpRegisters() {
  const fdf::MmioBuffer& mmio = mmio_.value();
  CapabilityReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "CapabilityReg::%s", arg); });
  VersionReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "VersionReg::%s", arg); });

  InterruptStatusReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "InterruptStatusReg::%s", arg); });
  InterruptEnableReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "InterruptEnableReg::%s", arg); });

  HostControllerStatusReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "HostControllerStatusReg::%s", arg); });
  HostControllerEnableReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "HostControllerEnableReg::%s", arg); });

  UtrListBaseAddressReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "UtrListBaseAddressReg::%s", arg); });
  UtrListBaseAddressUpperReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "UtrListBaseAddressUpperReg::%s", arg); });
  UtrListDoorBellReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "UtrListDoorBellReg::%s", arg); });
  UtrListClearReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "UtrListClearReg::%s", arg); });
  UtrListRunStopReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "UtrListRunStopReg::%s", arg); });
  UtrListCompletionNotificationReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "UtrListCompletionNotificationReg::%s", arg); });

  UtmrListBaseAddressReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "UtmrListBaseAddressReg::%s", arg); });
  UtmrListBaseAddressUpperReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "UtmrListBaseAddressUpperReg::%s", arg); });
  UtmrListDoorBellReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "UtmrListDoorBellReg::%s", arg); });
  UtmrListRunStopReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "UtmrListRunStopReg::%s", arg); });

  UicCommandReg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "UicCommandReg::%s", arg); });
  UicCommandArgument1Reg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "UicCommandArgument1Reg::%s", arg); });
  UicCommandArgument2Reg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "UicCommandArgument2Reg::%s", arg); });
  UicCommandArgument3Reg::Get().ReadFrom(&mmio).Print(
      [](const char* arg) { FDF_LOG(DEBUG, "UicCommandArgument3Reg::%s", arg); });
}

zx_status_t Ufs::EnableHostController() {
  const fdf::MmioBuffer& mmio = mmio_.value();
  HostControllerEnableReg::Get().FromValue(0).set_host_controller_enable(true).WriteTo(&mmio);

  auto wait_for = [&]() -> bool {
    return HostControllerEnableReg::Get().ReadFrom(&mmio).host_controller_enable();
  };
  fbl::String timeout_message = "Timeout waiting for EnableHostController";
  return WaitWithTimeout(wait_for, kHostControllerTimeoutUs, timeout_message);
}

zx_status_t Ufs::DisableHostController() {
  const fdf::MmioBuffer& mmio = mmio_.value();
  HostControllerEnableReg::Get().FromValue(0).set_host_controller_enable(false).WriteTo(&mmio);

  auto wait_for = [&]() -> bool {
    return !HostControllerEnableReg::Get().ReadFrom(&mmio).host_controller_enable();
  };
  fbl::String timeout_message = "Timeout waiting for DisableHostController";
  return WaitWithTimeout(wait_for, kHostControllerTimeoutUs, timeout_message);
}

zx::result<> Ufs::ConnectToPciService() {
  auto pci_client_end = incoming()->Connect<fuchsia_hardware_pci::Service::Device>("pci");
  if (!pci_client_end.is_ok()) {
    FDF_LOG(ERROR, "Failed to connect to PCI device service: %s", pci_client_end.status_string());
    return pci_client_end.take_error();
  }
  pci_ = fidl::WireSyncClient<fuchsia_hardware_pci::Device>(*std::move(pci_client_end));

  return zx::ok();
}

zx::result<> Ufs::ConfigResources() {
  // Map register window.
  {
    const auto result = pci_->GetBar(0);
    if (!result.ok()) {
      FDF_LOG(ERROR, "Call to GetBar failed: %s", result.status_string());
      return zx::error(result.status());
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "GetBar failed: %s", zx_status_get_string(result->error_value()));
      return zx::error(result->error_value());
    }

    if (!result->value()->result.result.is_vmo()) {
      FDF_LOG(ERROR, "PCI BAR is not an MMIO BAR.");
      return zx::error(ZX_ERR_WRONG_TYPE);
    }
    mmio_buffer_vmo_ = std::move(result->value()->result.result.vmo());
    mmio_buffer_size_ = result->value()->result.size;
  }

  // UFS host controller is bus master
  {
    const auto result = pci_->SetBusMastering(true);
    if (!result.ok()) {
      FDF_LOG(ERROR, "Call to SetBusMastering failed: %s", result.status_string());
      return zx::error(result.status());
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "SetBusMastering failed: %s", zx_status_get_string(result->error_value()));
      return zx::error(result->error_value());
    }
  }

  // Request 1 interrupt of any mode.
  {
    const auto result = pci_->GetInterruptModes();
    if (!result.ok()) {
      FDF_LOG(ERROR, "Call to GetInterruptModes failed: %s", result.status_string());
      return zx::error(result.status());
    }
    if (result->modes.msix_count > 0) {
      irq_mode_ = fuchsia_hardware_pci::InterruptMode::kMsiX;
    } else if (result->modes.msi_count > 0) {
      irq_mode_ = fuchsia_hardware_pci::InterruptMode::kMsi;
    } else if (result->modes.has_legacy) {
      irq_mode_ = fuchsia_hardware_pci::InterruptMode::kLegacy;
    } else {
      FDF_LOG(ERROR, "No interrupt modes are supported.");
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }
    FDF_LOG(DEBUG, "Interrupt mode: %u", static_cast<uint8_t>(irq_mode_));
  }
  {
    const auto result = pci_->SetInterruptMode(irq_mode_, 1);
    if (!result.ok()) {
      FDF_LOG(ERROR, "Call to SetInterruptMode failed: %s", result.status_string());
      return zx::error(result.status());
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "SetInterruptMode failed: %s", zx_status_get_string(result->error_value()));
      return zx::error(result->error_value());
    }
  }

  // Get irq handle.
  {
    const auto result = pci_->MapInterrupt(0);
    if (!result.ok()) {
      FDF_LOG(ERROR, "Call to MapInterrupt failed: %s", result.status_string());
      return zx::error(result.status());
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "MapInterrupt failed: %s", zx_status_get_string(result->error_value()));
      return zx::error(result->error_value());
    }
    irq_ = std::move(result->value()->interrupt);
  }

  // Get bti handle.
  {
    const auto result = pci_->GetBti(0);
    if (!result.ok()) {
      FDF_LOG(ERROR, "Call to GetBti failed: %s", result.status_string());
      return zx::error(result.status());
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "GetBti failed: %s", zx_status_get_string(result->error_value()));
      return zx::error(result->error_value());
    }
    bti_ = std::move(result->value()->bti);
  }

  return zx::ok();
}

zx::result<> Ufs::ConfigurePowerManagement() {
  fidl::Arena<> arena;
  const auto power_configs = fidl::ToWire(arena, GetAllPowerConfigs());
  if (power_configs.count() == 0) {
    FDF_LOG(INFO, "No power configs found.");
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  auto power_broker = driver_incoming()->Connect<fuchsia_power_broker::Topology>();
  if (power_broker.is_error() || !power_broker->is_valid()) {
    FDF_LOG(ERROR, "Failed to connect to power broker: %s", power_broker.status_string());
    return power_broker.take_error();
  }

  // Register power configs with the Power Broker.
  for (const auto& wire_config : power_configs) {
    fdf_power::PowerElementConfiguration config;
    {
      fuchsia_hardware_power::PowerElementConfiguration natural_config =
          fidl::ToNatural(wire_config);
      zx::result result = fdf_power::PowerElementConfiguration::FromFidl(natural_config);
      if (result.is_error()) {
        FDF_SLOG(ERROR, "Failed to convert power element config from fidl.",
                 KV("status", result.status_string()));
        return result.take_error();
      }
      config = std::move(result.value());
    }

    auto tokens = fdf_power::GetDependencyTokens(*driver_incoming(), config);
    if (tokens.is_error()) {
      FDF_LOG(ERROR, "Failed to get power dependency tokens: %u.",
              static_cast<uint8_t>(tokens.error_value()));
      return zx::error(ZX_ERR_INTERNAL);
    }

    fdf_power::ElementDesc description =
        fdf_power::ElementDescBuilder(config, std::move(tokens.value())).Build();
    auto result = fdf_power::AddElement(power_broker.value(), description);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to add power element: %u", static_cast<uint8_t>(result.error_value()));
      return zx::error(ZX_ERR_INTERNAL);
    }

    assertive_power_dep_tokens_.push_back(std::move(description.assertive_token));
    opportunistic_power_dep_tokens_.push_back(std::move(description.opportunistic_token));

    if (config.element.name == kHardwarePowerElementName) {
      hardware_power_element_control_client_ =
          fidl::WireSyncClient<fuchsia_power_broker::ElementControl>(
              std::move(description.element_control_client.value()));
      hardware_power_lessor_client_ = fidl::WireSyncClient<fuchsia_power_broker::Lessor>(
          std::move(description.lessor_client.value()));
      hardware_power_current_level_client_ =
          fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel>(
              std::move(description.current_level_client.value()));
      hardware_power_required_level_client_ = fidl::WireClient<fuchsia_power_broker::RequiredLevel>(
          std::move(description.required_level_client.value()), driver_async_dispatcher());
    } else if (config.element.name == kSystemWakeOnRequestPowerElementName) {
      wake_on_request_element_control_client_ =
          fidl::WireSyncClient<fuchsia_power_broker::ElementControl>(
              std::move(description.element_control_client.value()));
      wake_on_request_lessor_client_ = fidl::WireSyncClient<fuchsia_power_broker::Lessor>(
          std::move(description.lessor_client.value()));
      wake_on_request_current_level_client_ =
          fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel>(
              std::move(description.current_level_client.value()));
      wake_on_request_required_level_client_ =
          fidl::WireClient<fuchsia_power_broker::RequiredLevel>(
              std::move(description.required_level_client.value()), driver_async_dispatcher());
    } else {
      FDF_LOG(ERROR, "Unexpected power element: %s", std::string(config.element.name).c_str());
      return zx::error(ZX_ERR_BAD_STATE);
    }
  }

  // The lease request on the hardware power element remains persistent throughout the lifetime
  // of this driver.
  zx::result lease_control_client_end = AcquireLease(hardware_power_lessor_client_);
  if (!lease_control_client_end.is_ok()) {
    FDF_LOG(ERROR, "Failed to acquire lease on hardware power: %s",
            zx_status_get_string(lease_control_client_end.status_value()));
    return lease_control_client_end.take_error();
  }
  hardware_power_lease_control_client_end_ = std::move(lease_control_client_end.value());

  // Start continuous monitoring of the required level and adjusting of the hardware's power level.
  WatchHardwareRequiredLevel();
  WatchWakeOnRequestRequiredLevel();

  return zx::success();
}

void Ufs::WatchHardwareRequiredLevel() {
  fidl::Arena<> arena;
  hardware_power_required_level_client_.buffer(arena)->Watch().Then(
      [this](fidl::WireUnownedResult<fuchsia_power_broker::RequiredLevel::Watch>& result) {
        bool delay_before_next_watch = true;

        auto defer = fit::defer([&]() {
          if ((result.status() == ZX_ERR_CANCELED) || (result.status() == ZX_ERR_PEER_CLOSED)) {
            FDF_LOG(WARNING, "Watch returned %s. Stop monitoring required power level.",
                    zx_status_get_string(result.status()));
          } else {
            if (delay_before_next_watch) {
              // TODO(b/339826112): Determine how to handle errors when communicating with the Power
              // Broker. For now, avoid overwhelming the Power Broker with calls.
              zx::nanosleep(zx::deadline_after(zx::msec(1)));
            }
            // Recursively call self to watch the required hardware power level again. The Watch()
            // call blocks until the required power level has changed.
            WatchHardwareRequiredLevel();
          }
        });

        if (!result.ok()) {
          FDF_LOG(ERROR, "Call to Watch failed: %s", result.status_string());
          return;
        }
        if (result->is_error()) {
          switch (result->error_value()) {
            case fuchsia_power_broker::RequiredLevelError::kInternal:
              FDF_LOG(ERROR, "Watch returned internal error.");
              break;
            case fuchsia_power_broker::RequiredLevelError::kNotAuthorized:
              FDF_LOG(ERROR, "Watch returned not authorized error.");
              break;
            default:
              FDF_LOG(ERROR, "Watch returned unknown error.");
              break;
          }
          return;
        }

        const fuchsia_power_broker::PowerLevel required_level = result->value()->required_level;
        switch (required_level) {
          case kPowerLevelOn: {
            const zx::time start = zx::clock::get_monotonic();

            // Actually raise the hardware's power level.
            zx::result result = device_manager_->ResumePower();
            if (result.is_error()) {
              const zx::duration duration = zx::clock::get_monotonic() - start;
              FDF_LOGL(ERROR, logger(), "Failed to resume power after %ld us: %s",
                       duration.to_usecs(), result.status_string());
              return;
            }

            // Communicate to Power Broker that the hardware power level has been raised.
            UpdatePowerLevel(hardware_power_current_level_client_, kPowerLevelOn);

            const zx::duration duration = zx::clock::get_monotonic() - start;
            properties_.wake_latency_us.Insert(duration.to_usecs());

            wait_for_power_resumed_.Signal();
            break;
          }
          case kPowerLevelOff: {
            // Actually lower the hardware's power level.
            zx::result result = device_manager_->SuspendPower();
            if (result.is_error()) {
              FDF_LOG(ERROR, "Failed to suspend power: %s", result.status_string());
              return;
            }

            // Communicate to Power Broker that the hardware power level has been lowered.
            UpdatePowerLevel(hardware_power_current_level_client_, kPowerLevelOff);
            break;
          }
          default:
            FDF_LOG(ERROR, "Unexpected power level for hardware power element: %u", required_level);
            return;
        }

        delay_before_next_watch = false;
      });
}

void Ufs::WatchWakeOnRequestRequiredLevel() {
  fidl::Arena<> arena;
  wake_on_request_required_level_client_.buffer(arena)->Watch().Then(
      [this](fidl::WireUnownedResult<fuchsia_power_broker::RequiredLevel::Watch>& result) {
        auto defer = fit::defer([&]() {
          if (result.status() == ZX_ERR_CANCELED) {
            FDF_LOG(WARNING,
                    "Watch returned canceled error. Stop monitoring required power level.");
          } else {
            // Recursively call self to watch the required wake-on-request power level again. The
            // Watch() call blocks until the required power level has changed.
            WatchWakeOnRequestRequiredLevel();
          }
        });

        if (!result.ok()) {
          FDF_LOG(ERROR, "Call to Watch failed: %s", result.status_string());
          return;
        }
        if (result->is_error()) {
          switch (result->error_value()) {
            case fuchsia_power_broker::RequiredLevelError::kInternal:
              FDF_LOG(ERROR, "Watch returned internal error.");
              break;
            case fuchsia_power_broker::RequiredLevelError::kNotAuthorized:
              FDF_LOG(ERROR, "Watch returned not authorized error.");
              break;
            default:
              FDF_LOG(ERROR, "Watch returned unknown error.");
              break;
          }
          return;
        }

        const fuchsia_power_broker::PowerLevel required_level = result->value()->required_level;
        if ((required_level != kPowerLevelOn) && (required_level != kPowerLevelOff)) {
          FDF_LOG(ERROR, "Unexpected power level for wake-on-request power element: %u",
                  required_level);
          return;
        }

        UpdatePowerLevel(wake_on_request_current_level_client_, required_level);
      });
}

zx::result<> Ufs::Start() {
  parent_node_.Bind(std::move(node()));

  if (zx::result status = ConnectToPciService(); status.is_error()) {
    return status.take_error();
  }

  if (zx::result status = ConfigResources(); status.is_error()) {
    return status.take_error();
  }

  auto [controller_client_end, controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();
  auto [node_client_end, node_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::Node>::Create();

  node_controller_.Bind(std::move(controller_client_end));
  root_node_.Bind(std::move(node_client_end));

  fidl::Arena arena;

  const auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena).name(arena, name()).Build();

  // Add root device, which will contain block devices for logical units
  auto result =
      parent_node_->AddChild(args, std::move(controller_server_end), std::move(node_server_end));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child: %s", result.status_string());
    return zx::error(result.status());
  }

  SetHostControllerCallback(NotifyEventCallback);

  if (zx_status_t status = Init(); status != ZX_OK) {
    return zx::error(status);
  }

  if (config().enable_suspend()) {
    return ConfigurePowerManagement();
  }

  return zx::ok();
}

void Ufs::PrepareStop(fdf::PrepareStopCompleter completer) {
  {
    std::lock_guard<std::mutex> lock(lock_);
    driver_shutdown_ = true;
  }

  if (pci_.is_valid()) {
    const auto result = pci_->SetBusMastering(false);
    if (!result.ok()) {
      FDF_LOG(ERROR, "Call to SetBusMastering failed: %s", result.status_string());
      completer(zx::error(result.status()));
      return;
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "SetBusMastering failed: %s", zx_status_get_string(result->error_value()));
      completer(zx::error(result->error_value()));
      return;
    }
  }

  // TODO(https://fxbug.dev/42075643): We should flush pending_commands_.

  irq_.destroy();  // Make irq_.wait() in IrqLoop() return ZX_ERR_CANCELED.
  // wait for worker loop to finish before removing devices
  if (exception_event_dispatcher_.get()) {
    exception_event_dispatcher_.ShutdownAsync();
    exception_event_completion_.Wait();
  }

  if (irq_worker_dispatcher_.get()) {
    irq_worker_dispatcher_.ShutdownAsync();
    irq_worker_shutdown_completion_.Wait();
  }

  if (io_worker_dispatcher_.get()) {
    sync_completion_signal(&io_signal_);
    wait_for_power_resumed_.Signal();
    io_worker_dispatcher_.ShutdownAsync();
    io_worker_shutdown_completion_.Wait();
  }

  if (admin_worker_dispatcher_.get()) {
    sync_completion_signal(&admin_signal_);
    admin_worker_dispatcher_.ShutdownAsync();
    admin_worker_shutdown_completion_.Wait();
  }

  completer(zx::ok());
}

}  // namespace ufs
