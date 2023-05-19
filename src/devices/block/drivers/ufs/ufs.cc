// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ufs.h"

#include <lib/ddk/binding_driver.h>
#include <lib/fit/defer.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>
#include <zircon/threads.h>

#include "safemath/safe_conversions.h"

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
      return zx::ok();
    // If these get called we're probably in trouble.
    case NotifyEvent::kPrePowerModeChange:
    case NotifyEvent::kPostPowerModeChange:
      return zx::error(ZX_ERR_NOT_SUPPORTED);
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
                                 const fbl::String &timeout_message) {
  uint32_t time_left = timeout_us;
  while (true) {
    if (wait_for()) {
      return ZX_OK;
    }
    if (time_left == 0) {
      zxlogf(ERROR, "%s  after %u usecs", timeout_message.begin(), timeout_us);
      return ZX_ERR_TIMED_OUT;
    }
    usleep(1);
    time_left--;
  }
}

zx::result<> Ufs::Isr() {
  auto interrupt_status = InterruptStatusReg::Get().ReadFrom(&mmio_);
  interrupt_status.WriteTo(&mmio_);

  if (interrupt_status.utp_transfer_request_completion_status()) {
    transfer_request_processor_->RequestCompletion();
  }
  // TODO(fxbug.dev/124835): Handle UTMR completion

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

int Ufs::ScsiLoop() {
  while (true) {
    if (IsDriverShutdown()) {
      zxlogf(DEBUG, "IO thread exiting.");
      break;
    }

    if (zx_status_t status = sync_completion_wait(&scsi_event_, ZX_TIME_INFINITE);
        status != ZX_OK) {
      zxlogf(ERROR, "Waiting scsi_event_ is Failed: %s", zx_status_get_string(status));
      break;
    }
    sync_completion_reset(&scsi_event_);

    {
      std::lock_guard lock(xfer_list_lock_);
      if (scsi_xfer_list_.is_empty()) {
        continue;
      }
    }

    zx::result<uint8_t> slot = transfer_request_processor_->ReserveSlot();
    if (slot.is_error()) {
      continue;  // If there is no free slot, continue.
    }

    std::unique_ptr<scsi_xfer> xfer;
    {
      // TODO(fxbug.dev/124835): One performance optimization possible here is to maintain a bitmap
      // of free slots, enabling a quick check of whether we have any free slots. This prevents us
      // from having to acquire this same lock a second time (pop the list when the lock is acquired
      // above if we know there is a free slot).
      std::lock_guard lock(xfer_list_lock_);
      xfer = scsi_xfer_list_.pop_front();
    }
    ZX_ASSERT(xfer != nullptr);

    zx::result<ResponseUpiu> response =
        transfer_request_processor_->SendScsiUpiu(std::move(xfer), slot.value());
    if (response.is_error()) {
      zxlogf(ERROR, "ScsiThread SendScsiUpiu() is Failed: %s", response.status_string());
    }
  }
  return thrd_success;
}

void Ufs::QueueScsiCommand(std::unique_ptr<scsi_xfer> xfer) {
  {
    std::lock_guard lock(xfer_list_lock_);
    scsi_xfer_list_.push_back(std::move(xfer));
  }

  // Kick scsi thread.
  sync_completion_signal(&scsi_event_);
}

zx_status_t Ufs::Init() {
  if (int thrd_status = thrd_create_with_name(
          &irq_thread_, [](void *ctx) { return static_cast<Ufs *>(ctx)->IrqLoop(); }, this,
          "ufs-irq-thread");
      thrd_status) {
    zx_status_t status = thrd_status_to_zx_status(thrd_status);
    zxlogf(ERROR, " Failed to create IRQ thread: %s", zx_status_get_string(status));
    return status;
  }
  irq_thread_started_ = true;

  if (zx::result<> result = InitController(); result.is_error()) {
    zxlogf(ERROR, "Failed to init UFS controller: %s", result.status_string());
    return result.error_value();
  }

  if (int thrd_status = thrd_create_with_name(
          &scsi_thread_, [](void *ctx) { return static_cast<Ufs *>(ctx)->ScsiLoop(); }, this,
          "ufs-scsi-thread");
      thrd_status) {
    zx_status_t status = thrd_status_to_zx_status(thrd_status);
    zxlogf(ERROR, " Failed to create SCSI thread: %s", zx_status_get_string(status));
    return status;
  }
  scsi_thread_started_ = true;

  // TODO(fxbug.dev/124835): Get controller descriptor
  // TODO(fxbug.dev/124835): Scan logical units

  zxlogf(INFO, "Bind Success");
  return ZX_OK;
}

zx::result<> Ufs::InitController() {
  // Notify platform UFS that we are going to init the UFS host controller.
  if (zx::result<> result = Notify(NotifyEvent::kInit, 0); result.is_error()) {
    return result.take_error();
  }

  zxlogf(INFO, "Controller version %u.%u found",
         VersionReg::Get().ReadFrom(&mmio_).major_version_number(),
         VersionReg::Get().ReadFrom(&mmio_).minor_version_number());
  zxlogf(DEBUG, "capabilities 0x%x", CapabilityReg::Get().ReadFrom(&mmio_).reg_value());

  uint8_t number_of_task_management_request_slots = safemath::checked_cast<uint8_t>(
      CapabilityReg::Get().ReadFrom(&mmio_).number_of_utp_task_management_request_slots() + 1);
  zxlogf(DEBUG, "number_of_task_management_request_slots=%d",
         number_of_task_management_request_slots);
  // TODO(fxbug.dev/124835): Create TaskManagementRequestProcessor

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

  // TODO(fxbug.dev/124835): We need to check if retry is needed in the real HW and remove it if
  // not.
  // Initialise the device interface. If it fails, retry twice.
  zx::result<> result;
  for (uint32_t retry = 3; retry > 0; --retry) {
    if (result = InitDeviceInterface(); result.is_error()) {
      zxlogf(WARNING, "Device init failed: %s, retrying", result.status_string());
    } else {
      break;
    }
  }
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to initialize device interface: %s", result.status_string());
    return result.take_error();
  }

  return zx::ok();
}

zx::result<> Ufs::InitDeviceInterface() {
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

  // Enable UIC related interrupts.
  InterruptEnableReg::Get()
      .FromValue(0)
      .set_utp_transfer_request_completion_enable(true)
      .set_utp_task_management_request_completion_enable(true)
      .WriteTo(&mmio_);

  // Send Link Startup UIC command to start the link startup procedure.
  DmeLinkStartUpUicCommand link_startup_command(*this);
  if (zx::result<std::optional<uint32_t>> result = link_startup_command.SendCommandWithNotify();
      result.is_error()) {
    zxlogf(ERROR, "Failed to startup UFS link: %s", result.status_string());
    return result.take_error();
  }

  // TODO(fxbug.dev/124835): Get the max gear level using DME_GET command.

  // The |device_present| bit becomes true if the host controller has successfully received a Link
  // Startup UIC command response and the UFS device has found a physical link to the controller.
  if (!(HostControllerStatusReg::Get().ReadFrom(&mmio_).device_present())) {
    zxlogf(ERROR, "UFS device not found");
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  zxlogf(INFO, "UFS device found");

  // TODO(fxbug.dev/124835): Init task management request processor

  if (zx::result<> result = transfer_request_processor_->Init(); result.is_error()) {
    zxlogf(ERROR, "Failed to initialize transfer request processor %s", result.status_string());
    return result.take_error();
  }

  NopOutUpiu nop_upiu;
  auto nop_response = transfer_request_processor_->SendUpiu<NopInUpiu>(nop_upiu);
  if (nop_response.is_error()) {
    zxlogf(ERROR, "Send NopInUpiu failed: %s", nop_response.status_string());
    return nop_response.take_error();
  }

  zx::time device_init_start_time = zx::clock::get_monotonic();
  SetFlagUpiu set_flag_upiu(Flags::fDeviceInit);
  auto query_response = transfer_request_processor_->SendUpiu<QueryResponseUpiu>(set_flag_upiu);
  if (query_response.is_error()) {
    zxlogf(ERROR, "Failed to set fDeviceInit flag: %s", query_response.status_string());
    return query_response.take_error();
  }

  zx::time device_init_time_out = device_init_start_time + zx::msec(kDeviceInitTimeoutMs);
  while (true) {
    ReadFlagUpiu read_flag_upiu(Flags::fDeviceInit);
    auto response = transfer_request_processor_->SendUpiu<QueryResponseUpiu>(read_flag_upiu);
    if (response.is_error()) {
      zxlogf(ERROR, "Failed to read fDeviceInit flag: %s", response.status_string());
      return response.take_error();
    }
    uint8_t flag = response->GetResponse<FlagResponseUpiu>().GetFlag();

    if (!flag)
      break;

    if (zx::clock::get_monotonic() > device_init_time_out) {
      zxlogf(ERROR, "Wait for fDeviceInit timed out (%u ms)", kDeviceInitTimeoutMs);
      return zx::error(ZX_ERR_TIMED_OUT);
    }
    usleep(10000);
  }

  if (zx::result<> result = Notify(NotifyEvent::kDeviceInitDone, 0); result.is_error()) {
    return result.take_error();
  }

  // 26MHz is a default value written in spec.
  // UFS Specification Version 3.1, section 6.4 "Reference Clock".
  WriteAttributeUpiu write_attribute_upiu(Attributes::bRefClkFreq, AttributeReferenceClock::k26MHz);
  query_response = transfer_request_processor_->SendUpiu<QueryResponseUpiu>(write_attribute_upiu);
  if (query_response.is_error()) {
    zxlogf(ERROR, "Failed to write bRefClkFreq attribute: %s", query_response.status_string());
  }

  // Get connected lanes.
  DmeGetUicCommand dme_get_connected_tx_lanes_command(*this, PA_ConnectedTxDataLanes, 0);
  zx::result<std::optional<uint32_t>> value = dme_get_connected_tx_lanes_command.SendCommand();
  if (value.is_error()) {
    return value.take_error();
  }
  [[maybe_unused]] uint32_t connected_tx_lanes = value.value().value();

  DmeGetUicCommand dme_get_connected_rx_lanes_command(*this, PA_ConnectedRxDataLanes, 0);
  value = dme_get_connected_rx_lanes_command.SendCommand();
  if (value.is_error()) {
    return value.take_error();
  }
  [[maybe_unused]] uint32_t connected_rx_lanes = value.value().value();

  // Update lanes with available TX/RX lanes.
  DmeGetUicCommand dme_get_avail_tx_lanes_command(*this, PA_AvailTxDataLanes, 0);
  value = dme_get_avail_tx_lanes_command.SendCommand();
  if (value.is_error()) {
    return value.take_error();
  }
  uint32_t tx_lanes = value.value().value();

  DmeGetUicCommand dme_get_avail_rx_lanes_command(*this, PA_AvailRxDataLanes, 0);
  value = dme_get_avail_rx_lanes_command.SendCommand();
  if (value.is_error()) {
    return value.take_error();
  }
  uint32_t rx_lanes = value.value().value();
  zxlogf(DEBUG, "tx_lanes_=%d, rx_lanes_=%d", tx_lanes, rx_lanes);

  // Read bBootLunEn to confirm device interface is ok.
  ReadAttributeUpiu read_attribute_upiu(Attributes::bBootLunEn);
  query_response = transfer_request_processor_->SendUpiu<QueryResponseUpiu>(read_attribute_upiu);
  if (query_response.is_error()) {
    zxlogf(ERROR, "Failed to read bBootLunEn attribute: %s", query_response.status_string());
    return query_response.take_error();
  }
  auto attribute = query_response->GetResponse<AttributeResponseUpiu>().GetAttribute();
  zxlogf(DEBUG, "bBootLunEn 0x%0x", attribute);

  // TODO(fxbug.dev/124835): Set bMaxNumOfRTT (Read-to-transfer)

  return zx::ok();
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
  if (zx_status_t status =
          DdkAdd(ddk::DeviceAddArgs(kDriverName).set_inspect_vmo(inspector_.DuplicateVmo()));
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to run DdkAdd: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t Ufs::Bind(void *ctx, zx_device_t *parent) {
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
  }
  txn.Reply(status);
}

void Ufs::DdkRelease() {
  zxlogf(DEBUG, "Releasing driver.");
  driver_shutdown_ = true;
  if (mmio_.get_vmo() != ZX_HANDLE_INVALID) {
    pci_.SetBusMastering(false);
  }
  irq_.destroy();  // Make irq_.wait() in IrqLoop() return ZX_ERR_CANCELED.
  if (irq_thread_started_) {
    thrd_join(irq_thread_, nullptr);
  }
  if (scsi_thread_started_) {
    sync_completion_signal(&scsi_event_);
    thrd_join(scsi_thread_, nullptr);
  }

  delete this;
}

static zx_driver_ops_t driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = Ufs::Bind,
};

}  // namespace ufs

ZIRCON_DRIVER(Ufs, ufs::driver_ops, "zircon", "0.1");
