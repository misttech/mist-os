// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ufs.h"

#include <lib/ddk/binding_driver.h>
#include <lib/fit/defer.h>
#include <lib/trace/event.h>
#include <zircon/errors.h>
#include <zircon/threads.h>

#include <array>
#include <vector>

#include <fbl/auto_lock.h>
#include <safemath/safe_conversions.h>

namespace ufs {

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
      zxlogf(ERROR, "%s after %u usecs", timeout_message.begin(), timeout_us);
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
    zxlogf(ERROR, "Failed to map IO buffer: %s", zx_status_get_string(status));
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
    zxlogf(ERROR, "Invalid scsi lun: 0x%x", scsi_lun);
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
      fbl::AutoLock lock(&commands_lock_);
      io_cmd = list_remove_head_type(&pending_commands_, IoCommand, node);
    }

    if (io_cmd == nullptr) {
      return;
    }

    DataDirection data_direction = DataDirection::kNone;
    if (io_cmd->is_write) {
      data_direction = DataDirection::kHostToDevice;
    } else if (io_cmd->disk_op.op.command.opcode == BLOCK_OPCODE_READ) {
      data_direction = DataDirection::kDeviceToHost;
    }

    std::optional<zx::unowned_vmo> vmo_optional = std::nullopt;
    uint32_t transfer_bytes = 0;
    if (data_direction != DataDirection::kNone) {
      if (io_cmd->disk_op.op.command.opcode == BLOCK_OPCODE_TRIM) {
        // For the UNMAP command, a data buffer is required for the parameter list.
        zx::vmo data_vmo;
        fzl::VmoMapper mapper;
        if (zx::result<> result = AllocatePages(data_vmo, mapper, io_cmd->data_length);
            result.is_error()) {
          zxlogf(ERROR, "Failed to allocate data buffer (command %p): %s", io_cmd,
                 result.status_string());
          return;
        }
        memcpy(mapper.start(), io_cmd->data_buffer, io_cmd->data_length);
        vmo_optional = zx::unowned_vmo(data_vmo);
        io_cmd->data_vmo = std::move(data_vmo);

        transfer_bytes = io_cmd->data_length;
      } else {
        vmo_optional = zx::unowned_vmo(io_cmd->disk_op.op.rw.vmo);

        transfer_bytes = io_cmd->disk_op.op.rw.length * io_cmd->block_size_bytes;
      }
    }

    if (transfer_bytes > max_transfer_bytes_) {
      zxlogf(ERROR,
             "Request exceeding max transfer size. transfer_bytes=%d, max_transfer_bytes_=%d",
             transfer_bytes, max_transfer_bytes_);
      io_cmd->disk_op.Complete(ZX_ERR_INVALID_ARGS);
      continue;
    }

    ScsiCommandUpiu upiu(io_cmd->cdb_buffer, io_cmd->cdb_length, data_direction, transfer_bytes);
    auto response =
        transfer_request_processor_->SendScsiUpiu(upiu, io_cmd->lun, vmo_optional, io_cmd);
    if (response.is_error()) {
      if (response.error_value() == ZX_ERR_NO_RESOURCES) {
        fbl::AutoLock lock(&commands_lock_);
        list_add_head(&pending_commands_, &io_cmd->node);
        return;
      }
      zxlogf(ERROR, "Failed to submit SCSI command (command %p): %s", io_cmd,
             response.status_string());
      io_cmd->disk_op.Complete(response.error_value());
      io_cmd->data_vmo.reset();
    }
  }
}

void Ufs::ProcessCompletions() { transfer_request_processor_->RequestCompletion(); }

zx::result<> Ufs::Isr() {
  auto interrupt_status = InterruptStatusReg::Get().ReadFrom(&mmio_);

  // TODO(https://fxbug.dev/42075643): implement error handlers
  if (interrupt_status.uic_error()) {
    zxlogf(ERROR, "UFS: UIC error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_uic_error(true).WriteTo(&mmio_);

    // UECPA for Host UIC Error Code within PHY Adapter Layer.
    if (HostUicErrorCodePhyAdapterLayerReg::Get().ReadFrom(&mmio_).uic_phy_adapter_layer_error()) {
      zxlogf(ERROR, "UECPA error code: 0x%x",
             HostUicErrorCodePhyAdapterLayerReg::Get()
                 .ReadFrom(&mmio_)
                 .uic_phy_adapter_layer_error_code());
    }
    // UECDL for Host UIC Error Code within Data Link Layer.
    if (HostUicErrorCodeDataLinkLayerReg::Get().ReadFrom(&mmio_).uic_data_link_layer_error()) {
      zxlogf(ERROR, "UECDL error code: 0x%x",
             HostUicErrorCodeDataLinkLayerReg::Get()
                 .ReadFrom(&mmio_)
                 .uic_data_link_layer_error_code());
    }
    // UECN for Host UIC Error Code within Network Layer.
    if (HostUicErrorCodeNetworkLayerReg::Get().ReadFrom(&mmio_).uic_network_layer_error()) {
      zxlogf(
          ERROR, "UECN error code: 0x%x",
          HostUicErrorCodeNetworkLayerReg::Get().ReadFrom(&mmio_).uic_network_layer_error_code());
    }
    // UECT for Host UIC Error Code within Transport Layer.
    if (HostUicErrorCodeTransportLayerReg::Get().ReadFrom(&mmio_).uic_transport_layer_error()) {
      zxlogf(ERROR, "UECT error code: 0x%x",
             HostUicErrorCodeTransportLayerReg::Get()
                 .ReadFrom(&mmio_)
                 .uic_transport_layer_error_code());
    }
    // UECDME for Host UIC Error Code within DME subcomponent.
    if (HostUicErrorCodeReg::Get().ReadFrom(&mmio_).uic_dme_error()) {
      zxlogf(ERROR, "UECDME error code: 0x%x",
             HostUicErrorCodeReg::Get().ReadFrom(&mmio_).uic_dme_error_code());
    }
  }
  if (interrupt_status.device_fatal_error_status()) {
    zxlogf(ERROR, "UFS: Device fatal error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_device_fatal_error_status(true).WriteTo(&mmio_);
  }
  if (interrupt_status.host_controller_fatal_error_status()) {
    zxlogf(ERROR, "UFS: Host controller fatal error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_host_controller_fatal_error_status(true).WriteTo(
        &mmio_);
  }
  if (interrupt_status.system_bus_fatal_error_status()) {
    zxlogf(ERROR, "UFS: System bus fatal error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_system_bus_fatal_error_status(true).WriteTo(&mmio_);
  }
  if (interrupt_status.crypto_engine_fatal_error_status()) {
    zxlogf(ERROR, "UFS: Crypto engine fatal error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_crypto_engine_fatal_error_status(true).WriteTo(
        &mmio_);
  }

  // Handle command completion interrupts.
  if (interrupt_status.utp_transfer_request_completion_status()) {
    InterruptStatusReg::Get().FromValue(0).set_utp_transfer_request_completion_status(true).WriteTo(
        &mmio_);
    sync_completion_signal(&io_signal_);
  }
  if (interrupt_status.utp_task_management_request_completion_status()) {
    // TODO(https://fxbug.dev/42075643): Handle UTMR completion
    zxlogf(ERROR, "UFS: UTMR completion not yet implemented");
    InterruptStatusReg::Get()
        .FromValue(0)
        .set_utp_task_management_request_completion_status(true)
        .WriteTo(&mmio_);
  }
  if (interrupt_status.uic_command_completion_status()) {
    // TODO(https://fxbug.dev/42075643): Handle UIC completion
    zxlogf(ERROR, "UFS: UIC completion not yet implemented");
  }

  return zx::ok();
}

int Ufs::IrqLoop() {
  while (true) {
    if (zx_status_t status = irq_.wait(nullptr); status != ZX_OK) {
      if (status == ZX_ERR_CANCELED) {
        zxlogf(DEBUG, "Interrupt cancelled. Exiting IRQ loop.");
      } else {
        zxlogf(ERROR, "Failed to wait for interrupt: %s", zx_status_get_string(status));
      }
      break;
    }

    if (zx::result<> result = Isr(); result.is_error()) {
      zxlogf(ERROR, "Failed to run interrupt service routine: %s", result.status_string());
    }

    if (irq_mode_ == fuchsia_hardware_pci::InterruptMode::kLegacy) {
      if (zx_status_t status = pci_.AckInterrupt(); status != ZX_OK) {
        zxlogf(ERROR, "Failed to ack interrupt: %s", zx_status_get_string(status));
        break;
      }
    }
  }
  return thrd_success;
}

int Ufs::IoLoop() {
  while (true) {
    if (IsDriverShutdown()) {
      zxlogf(DEBUG, "IO thread exiting.");
      break;
    }

    if (zx_status_t status = sync_completion_wait(&io_signal_, ZX_TIME_INFINITE); status != ZX_OK) {
      zxlogf(ERROR, "Failed to wait for sync completion: %s", zx_status_get_string(status));
      break;
    }
    sync_completion_reset(&io_signal_);

    // TODO(https://fxbug.dev/42075643): Process async completions

    if (!disable_completion_) {
      ProcessCompletions();
    }
    ProcessIoSubmissions();
  }
  return thrd_success;
}

void Ufs::ExecuteCommandAsync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                              uint32_t block_size_bytes, scsi::DiskOp* disk_op, iovec data) {
  IoCommand* io_cmd = containerof(disk_op, IoCommand, disk_op);
  if (cdb.iov_len > sizeof(io_cmd->cdb_buffer)) {
    disk_op->Complete(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  auto lun_id = TranslateScsiLunToUfsLun(lun);
  if (lun_id.is_error()) {
    disk_op->Complete(lun_id.status_value());
    return;
  }

  memcpy(io_cmd->cdb_buffer, cdb.iov_base, cdb.iov_len);
  io_cmd->cdb_length = safemath::checked_cast<uint8_t>(cdb.iov_len);
  io_cmd->lun = lun_id.value();
  io_cmd->block_size_bytes = block_size_bytes;
  io_cmd->is_write = is_write;

  // Currently, data is only used in the UNMAP command.
  if (disk_op->op.command.opcode == BLOCK_OPCODE_TRIM && data.iov_len != 0) {
    if (sizeof(io_cmd->data_buffer) != data.iov_len) {
      zxlogf(ERROR,
             "The size of the requested data buffer(%zu) and data_buffer(%lu) are different.",
             data.iov_len, sizeof(io_cmd->data_buffer));
      disk_op->Complete(ZX_ERR_INVALID_ARGS);
      return;
    }
    memcpy(io_cmd->data_buffer, data.iov_base, data.iov_len);
    io_cmd->data_length = static_cast<uint8_t>(data.iov_len);
  }

  // Queue transaction.
  {
    fbl::AutoLock lock(&commands_lock_);
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
    zxlogf(ERROR, "Request exceeding max transfer size. transfer_bytes=%zu, max_transfer_bytes_=%d",
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
      zxlogf(ERROR, "Failed to send SCSI command: %s", response.status_string());
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

zx_status_t Ufs::Init() {
  list_initialize(&pending_commands_);

  if (zx::result<> result = InitController(); result.is_error()) {
    zxlogf(ERROR, "Failed to initialize UFS controller: %s", result.status_string());
    return result.error_value();
  }

  if (zx::result<> result = InitDeviceInterface(); result.is_error()) {
    zxlogf(ERROR, "Failed to initialize device interface: %s", result.status_string());
    return result.error_value();
  }

  if (zx::result<> result = device_manager_->GetControllerDescriptor(); result.is_error()) {
    zxlogf(ERROR, "Failed to get controller descriptor: %s", result.status_string());
    return result.error_value();
  }

  // The maximum transfer size supported by UFSHCI spec is 65535 * 256 KiB. However, we limit the
  // maximum transfer size to 1MiB for performance reason.
  max_transfer_bytes_ = kMaxTransferSize1MiB;

  zx::result<uint32_t> lun_count;
  if (lun_count = AddLogicalUnits(); lun_count.is_error()) {
    zxlogf(ERROR, "Failed to scan logical units: %s", lun_count.status_string());
    return lun_count.error_value();
  }

  if (lun_count.value() == 0) {
    zxlogf(ERROR, "Bind Error. There is no available LUN(lun_count = 0).");
    return ZX_ERR_BAD_STATE;
  }
  logical_unit_count_ = lun_count.value();
  zxlogf(INFO, "Bind Success");

  return ZX_OK;
}

zx::result<> Ufs::InitController() {
  // Disable all interrupts.
  InterruptEnableReg::Get().FromValue(0).WriteTo(&mmio_);

  if (zx::result<> result = Notify(NotifyEvent::kReset, 0); result.is_error()) {
    return result.take_error();
  }
  // If UFS host controller is already enabled, disable it.
  if (HostControllerEnableReg::Get().ReadFrom(&mmio_).host_controller_enable()) {
    DisableHostController();
  }
  if (zx_status_t status = EnableHostController(); status != ZX_OK) {
    zxlogf(ERROR, "Failed to enable host controller %d", status);
    return zx::error(status);
  }

  if (int thrd_status = thrd_create_with_name(
          &irq_thread_, [](void* ctx) { return static_cast<Ufs*>(ctx)->IrqLoop(); }, this,
          "ufs-irq-thread");
      thrd_status) {
    zx_status_t status = thrd_status_to_zx_status(thrd_status);
    zxlogf(ERROR, " Failed to create IRQ thread: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  irq_thread_started_ = true;

  // Notify platform UFS that we are going to init the UFS host controller.
  if (zx::result<> result = Notify(NotifyEvent::kInit, 0); result.is_error()) {
    return result.take_error();
  }

  zxlogf(INFO, "Controller version %u.%u found",
         VersionReg::Get().ReadFrom(&mmio_).major_version_number(),
         VersionReg::Get().ReadFrom(&mmio_).minor_version_number());
  zxlogf(DEBUG, "capabilities 0x%x", CapabilityReg::Get().ReadFrom(&mmio_).reg_value());

  auto device_manager = DeviceManager::Create(*this);
  if (device_manager.is_error()) {
    zxlogf(ERROR, "Failed to create device manager %s", device_manager.status_string());
    return device_manager.take_error();
  }
  device_manager_ = std::move(*device_manager);

  uint8_t number_of_task_management_request_slots = safemath::checked_cast<uint8_t>(
      CapabilityReg::Get().ReadFrom(&mmio_).number_of_utp_task_management_request_slots() + 1);
  zxlogf(DEBUG, "number_of_task_management_request_slots=%d",
         number_of_task_management_request_slots);
  // TODO(https://fxbug.dev/42075643): Create TaskManagementRequestProcessor

  uint8_t number_of_transfer_request_slots = safemath::checked_cast<uint8_t>(
      CapabilityReg::Get().ReadFrom(&mmio_).number_of_utp_transfer_request_slots() + 1);
  zxlogf(DEBUG, "number_of_transfer_request_slots=%d", number_of_transfer_request_slots);

  auto transfer_request_processor = TransferRequestProcessor::Create(
      *this, bti_.borrow(), mmio_,
      safemath::checked_cast<uint8_t>(number_of_transfer_request_slots));
  if (transfer_request_processor.is_error()) {
    zxlogf(ERROR, "Failed to create transfer request processor %s",
           transfer_request_processor.status_string());
    return transfer_request_processor.take_error();
  }
  transfer_request_processor_ = std::move(*transfer_request_processor);

  if (int thrd_status = thrd_create_with_name(
          &io_thread_, [](void* ctx) { return static_cast<Ufs*>(ctx)->IoLoop(); }, this,
          "ufs-io-thread");
      thrd_status) {
    zx_status_t status = thrd_status_to_zx_status(thrd_status);
    zxlogf(ERROR, " Failed to create IO thread: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  io_thread_started_ = true;

  return zx::ok();
}

zx::result<> Ufs::InitDeviceInterface() {
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
      .WriteTo(&mmio_);

  if (!HostControllerStatusReg::Get().ReadFrom(&mmio_).uic_command_ready()) {
    zxlogf(ERROR, "UIC command is not ready\n");
    return zx::error(ZX_ERR_INTERNAL);
  }

  // Send Link Startup UIC command to start the link startup procedure.
  if (zx::result<> result = device_manager_->SendLinkStartUp(); result.is_error()) {
    zxlogf(ERROR, "Failed to send Link Startup UIC command %s", result.status_string());
    return result.take_error();
  }

  // The |device_present| bit becomes true if the host controller has successfully received a Link
  // Startup UIC command response and the UFS device has found a physical link to the controller.
  if (!HostControllerStatusReg::Get().ReadFrom(&mmio_).device_present()) {
    zxlogf(ERROR, "UFS device not found");
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  zxlogf(INFO, "UFS device found");

  // TODO(https://fxbug.dev/42075643): Init task management request processor

  if (zx::result<> result = transfer_request_processor_->Init(); result.is_error()) {
    zxlogf(ERROR, "Failed to initialize transfer request processor %s", result.status_string());
    return result.take_error();
  }

  // TODO(https://fxbug.dev/42075643): Configure interrupt aggregation. (default 0)

  NopOutUpiu nop_upiu;
  auto nop_response = transfer_request_processor_->SendRequestUpiu<NopOutUpiu, NopInUpiu>(nop_upiu);
  if (nop_response.is_error()) {
    zxlogf(ERROR, "Failed to send NopInUpiu %s", nop_response.status_string());
    return nop_response.take_error();
  }

  if (zx::result<> result = device_manager_->DeviceInit(); result.is_error()) {
    zxlogf(ERROR, "Failed to initialize device %s", result.status_string());
    return result.take_error();
  }

  if (zx::result<> result = Notify(NotifyEvent::kDeviceInitDone, 0); result.is_error()) {
    return result.take_error();
  }

  if (zx::result<> result = device_manager_->InitReferenceClock(); result.is_error()) {
    zxlogf(ERROR, "Failed to initialize reference clock %s", result.status_string());
    return result.take_error();
  }

  if (zx::result<> result = device_manager_->InitUniproAttributes(); result.is_error()) {
    zxlogf(ERROR, "Failed to initialize Unipro attributes %s", result.status_string());
    return result.take_error();
  }

  if (zx::result<> result = device_manager_->InitUicPowerMode(); result.is_error()) {
    zxlogf(ERROR, "Failed to initialize UIC power mode %s", result.status_string());
    return result.take_error();
  }

  if (zx::result<> result = device_manager_->InitUfsPowerMode(); result.is_error()) {
    zxlogf(ERROR, "Failed to initialize UFS power mode %s", result.status_string());
    return result.take_error();
  }

  if (zx::result<> result = device_manager_->CheckBootLunEnabled(); result.is_error()) {
    zxlogf(ERROR, "Failed to check Boot LUN enabled %s", result.status_string());
    return result.take_error();
  }

  // TODO(https://fxbug.dev/42075643): Set bMaxNumOfRTT (Read-to-transfer)

  return zx::ok();
}

zx::result<uint32_t> Ufs::AddLogicalUnits() {
  uint8_t max_luns = 0;
  if (device_manager_->GetGeometryDescriptor().bMaxNumberLU == 0) {
    max_luns = 8;
  } else if (device_manager_->GetGeometryDescriptor().bMaxNumberLU == 1) {
    max_luns = 32;
  } else {
    zxlogf(ERROR, "Invalid Geometry Descriptor bMaxNumberLU value=%d",
           device_manager_->GetGeometryDescriptor().bMaxNumberLU);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
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
      zxlogf(ERROR, "Cannot handle the unit descriptor bLogicalBlockSize = %d.",
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
      zxlogf(ERROR, "Cannot handle logical block size of %zu.", desc_block_size);
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }
    ZX_ASSERT_MSG(desc_block_size == kBlockSize, "Currently, it only supports a 4KB block size.");

    if (desc_block_size != block_size || desc_block_count != block_count) {
      zxlogf(INFO,
             "Failed to check for disk consistency. (block_size=%zu/%zu, block_count=%ld/%ld)",
             desc_block_size, block_size, desc_block_count, block_count);
      return zx::error(ZX_ERR_BAD_STATE);
    }
    zxlogf(INFO, "LUN-%d block_size=%zu, block_count=%ld", lun, desc_block_size, desc_block_count);

    return zx::ok();
  };

  // UFS does not support the MODE SENSE(6) command. We should use the MODE SENSE(10) command.
  // UFS does not support the READ(12)/WRITE(12) commands.
  scsi::DiskOptions options(/*check_unmap_support*/ true, /*use_mode_sense_6*/ false,
                            /*use_read_write_12*/ false);

  zx::result<uint32_t> lun_count = ScanAndBindLogicalUnits(
      zxdev(), kPlaceholderTarget, max_transfer_bytes_, max_luns, read_unit_descriptor, options);
  if (lun_count.is_error()) {
    zxlogf(ERROR, "Failed to scan logical units: %s", lun_count.status_string());
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
    zxlogf(INFO, "Well known LUN-0x%x", static_cast<uint8_t>(lun));
  }

  return zx::ok(lun_count.value());
}

void Ufs::DumpRegisters() {
  CapabilityReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "CapabilityReg::%s", arg); });
  VersionReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "VersionReg::%s", arg); });

  InterruptStatusReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "InterruptStatusReg::%s", arg); });
  InterruptEnableReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "InterruptEnableReg::%s", arg); });

  HostControllerStatusReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "HostControllerStatusReg::%s", arg); });
  HostControllerEnableReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "HostControllerEnableReg::%s", arg); });

  UtrListBaseAddressReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtrListBaseAddressReg::%s", arg); });
  UtrListBaseAddressUpperReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtrListBaseAddressUpperReg::%s", arg); });
  UtrListDoorBellReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtrListDoorBellReg::%s", arg); });
  UtrListClearReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtrListClearReg::%s", arg); });
  UtrListRunStopReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtrListRunStopReg::%s", arg); });
  UtrListCompletionNotificationReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtrListCompletionNotificationReg::%s", arg); });

  UtmrListBaseAddressReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtmrListBaseAddressReg::%s", arg); });
  UtmrListBaseAddressUpperReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtmrListBaseAddressUpperReg::%s", arg); });
  UtmrListDoorBellReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtmrListDoorBellReg::%s", arg); });
  UtmrListRunStopReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtmrListRunStopReg::%s", arg); });

  UicCommandReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UicCommandReg::%s", arg); });
  UicCommandArgument1Reg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UicCommandArgument1Reg::%s", arg); });
  UicCommandArgument2Reg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UicCommandArgument2Reg::%s", arg); });
  UicCommandArgument3Reg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UicCommandArgument3Reg::%s", arg); });
}

zx_status_t Ufs::EnableHostController() {
  HostControllerEnableReg::Get().FromValue(0).set_host_controller_enable(true).WriteTo(&mmio_);

  auto wait_for = [&]() -> bool {
    return HostControllerEnableReg::Get().ReadFrom(&mmio_).host_controller_enable();
  };
  fbl::String timeout_message = "Timeout waiting for EnableHostController";
  return WaitWithTimeout(wait_for, kHostControllerTimeoutUs, timeout_message);
}

zx_status_t Ufs::DisableHostController() {
  HostControllerEnableReg::Get().FromValue(0).set_host_controller_enable(false).WriteTo(&mmio_);

  auto wait_for = [&]() -> bool {
    return !HostControllerEnableReg::Get().ReadFrom(&mmio_).host_controller_enable();
  };
  fbl::String timeout_message = "Timeout waiting for DisableHostController";
  return WaitWithTimeout(wait_for, kHostControllerTimeoutUs, timeout_message);
}

zx_status_t Ufs::AddDevice() {
  if (zx_status_t status = DdkAdd(ddk::DeviceAddArgs(kDriverName)
                                      .set_flags(DEVICE_ADD_NON_BINDABLE)
                                      .set_inspect_vmo(inspector_.DuplicateVmo()));
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to run DdkAdd: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t Ufs::Bind(void* ctx, zx_device_t* parent) {
  ddk::Pci pci(parent, "pci");
  if (!pci.is_valid()) {
    zxlogf(ERROR, "Failed to find PCI fragment");
    return ZX_ERR_NOT_SUPPORTED;
  }

  std::optional<fdf::MmioBuffer> mmio;
  if (zx_status_t status = pci.MapMmio(0u, ZX_CACHE_POLICY_UNCACHED_DEVICE, &mmio);
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to map registers: %s", zx_status_get_string(status));
    return status;
  }

  fuchsia_hardware_pci::InterruptMode irq_mode;
  if (zx_status_t status = pci.ConfigureInterruptMode(1, &irq_mode); status != ZX_OK) {
    zxlogf(ERROR, "Failed to configure interrupt: %s", zx_status_get_string(status));
    return status;
  }
  zxlogf(DEBUG, "Interrupt mode: %u", static_cast<uint8_t>(irq_mode));

  zx::interrupt irq;
  if (zx_status_t status = pci.MapInterrupt(0, &irq); status != ZX_OK) {
    zxlogf(ERROR, "Failed to map interrupt: %s", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = pci.SetBusMastering(true); status != ZX_OK) {
    zxlogf(ERROR, "Failed to enable bus mastering: %s", zx_status_get_string(status));
    return status;
  }
  auto cleanup = fit::defer([&] { pci.SetBusMastering(false); });

  zx::bti bti;
  if (zx_status_t status = pci.GetBti(0, &bti); status != ZX_OK) {
    zxlogf(ERROR, "Failed to get BTI handle: %s", zx_status_get_string(status));
    return status;
  }

  fbl::AllocChecker ac;
  auto driver = fbl::make_unique_checked<Ufs>(&ac, parent, std::move(pci), std::move(*mmio),
                                              irq_mode, std::move(irq), std::move(bti));
  if (!ac.check()) {
    zxlogf(ERROR, "Failed to allocate memory for UFS driver.");
    return ZX_ERR_NO_MEMORY;
  }
  driver->SetHostControllerCallback(NotifyEventCallback);

  if (zx_status_t status = driver->AddDevice(); status != ZX_OK) {
    return status;
  }

  // The DriverFramework now owns driver.
  [[maybe_unused]] auto placeholder = driver.release();
  cleanup.cancel();
  return ZX_OK;
}

void Ufs::DdkInit(ddk::InitTxn txn) {
  zx_status_t status = Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Driver initialization failed: %s", zx_status_get_string(status));
    DumpRegisters();
  }
  txn.Reply(status);
}

void Ufs::DdkRelease() {
  zxlogf(DEBUG, "Releasing driver.");
  driver_shutdown_ = true;
  if (pci_.is_valid()) {
    pci_.SetBusMastering(false);
  }
  irq_.destroy();  // Make irq_.wait() in IrqLoop() return ZX_ERR_CANCELED.
  if (irq_thread_started_) {
    thrd_join(irq_thread_, nullptr);
  }
  if (io_thread_started_) {
    sync_completion_signal(&io_signal_);
    thrd_join(io_thread_, nullptr);
  }

  delete this;
}

void Ufs::DdkSuspend(ddk::SuspendTxn txn) {
  zxlogf(INFO, "Ufs DdkSuspend, suspend_reason = %d, requested_state = %d", txn.suspend_reason(),
         txn.requested_state());

  if (auto result = device_manager_->Suspend(); result.is_error()) {
    txn.Reply(result.error_value(), txn.requested_state());
    return;
  }
  txn.Reply(ZX_OK, txn.requested_state());
}

void Ufs::DdkResume(ddk::ResumeTxn txn) {
  zxlogf(INFO, "Ufs DdkResume, requested_state = %d", txn.requested_state());

  if (auto result = device_manager_->Resume(); result.is_error()) {
    txn.Reply(result.error_value(), DEV_POWER_STATE_D1, txn.requested_state());
    return;
  }
  txn.Reply(ZX_OK, DEV_POWER_STATE_D0, txn.requested_state());
}

static zx_driver_ops_t driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = Ufs::Bind,
};

}  // namespace ufs

ZIRCON_DRIVER(Ufs, ufs::driver_ops, "zircon", "0.1");
