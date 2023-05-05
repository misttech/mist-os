// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ufs.h"

#include <lib/ddk/binding_driver.h>
#include <lib/fit/defer.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>

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
    case NotifyEvent::kSetupTransferUtrl:
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

zx_status_t Ufs::Init() {
  // TODO(fxbug.dev/124835): Initialize IRQ, I/O threads

  if (zx::result<> result = InitController(); result.is_error()) {
    zxlogf(ERROR, "Failed to init UFS controller: %s", result.status_string());
    return result.error_value();
  }

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

  number_of_utmr_slots_ =
      CapabilityReg::Get().ReadFrom(&mmio_).number_of_utp_task_management_request_slots() + 1;
  zxlogf(DEBUG, "number_of_utmr_slots_=%d", number_of_utmr_slots_);
  // TODO(fxbug.dev/124835): Create TaskManagementRequestQueue

  number_of_utr_slots_ =
      CapabilityReg::Get().ReadFrom(&mmio_).number_of_utp_transfer_request_slots() + 1;
  zxlogf(DEBUG, "number_of_utr_slots_=%d", number_of_utr_slots_);
  // TODO(fxbug.dev/124835): Create TransferRequestQueue

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
    zxlogf(ERROR, "UFS device not found\n");
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  zxlogf(INFO, "UFS device found\n");

  // TODO(fxbug.dev/124835): Send NopUPIU command
  // TODO(fxbug.dev/124835): Send Set Flags::fDeviceInit UPIU command

  if (zx::result<> result = Notify(NotifyEvent::kDeviceInitDone, 0); result.is_error()) {
    return result.take_error();
  }

  // TODO(fxbug.dev/124835): Send read Attributes::bBootLunEn UPIU command
  // TODO(fxbug.dev/124835): Get connected lanes using UIC command
  // TODO(fxbug.dev/124835): Update lanes with available TX/RX lanes using UIC command

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

  delete this;
}

static zx_driver_ops_t driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = Ufs::Bind,
};

}  // namespace ufs

ZIRCON_DRIVER(Ufs, ufs::driver_ops, "zircon", "0.1");
