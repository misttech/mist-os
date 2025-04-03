// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/rtc/drivers/pl031-rtc/pl031-rtc.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/platform-device/cpp/pdev.h>
#include <lib/zx/clock.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <ddktl/fidl.h>

#include "src/devices/rtc/lib/rtc/include/librtc_llcpp.h"

namespace rtc {

zx_status_t Pl031::Bind(void* /*unused*/, zx_device_t* dev) {
  zx::result pdev_client_end =
      DdkConnectFidlProtocol<fuchsia_hardware_platform_device::Service::Device>(dev);
  if (pdev_client_end.is_error()) {
    zxlogf(ERROR, "Failed to connect to platform device: %s", pdev_client_end.status_string());
    return pdev_client_end.status_value();
  }
  fdf::PDev pdev{std::move(pdev_client_end.value())};

  // Carve out some address space for this device.
  zx::result mmio = pdev.MapMmio(0);
  if (mmio.is_error()) {
    zxlogf(ERROR, "Failed to map mmio: %s", mmio.status_string());
    return mmio.status_value();
  }

  auto pl031_device = std::make_unique<Pl031>(dev, *std::move(mmio));

  zx_status_t status =
      pl031_device->DdkAdd(ddk::DeviceAddArgs("rtc").set_proto_id(ZX_PROTOCOL_RTC));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s error adding device: %s", __func__, zx_status_get_string(status));
    return status;
  }

  // Retrieve and sanitize the RTC value. Set the RTC to the value.
  FidlRtc::wire::Time rtc = SecondsToRtc(MmioRead32(&pl031_device->regs_->dr));
  rtc = SanitizeRtc(dev, rtc);
  status = pl031_device->SetRtc(rtc);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s failed to set rtc: %s", __func__, zx_status_get_string(status));
  }

  // The object is owned by the DDK, now that it has been added. It will be deleted
  // when the device is released.
  [[maybe_unused]] auto ptr = pl031_device.release();

  return status;
}

Pl031::Pl031(zx_device_t* parent, fdf::MmioBuffer mmio)
    : RtcDeviceType(parent),
      mmio_(std::move(mmio)),
      regs_(reinterpret_cast<MMIO_PTR Pl031Regs*>(mmio_.get())) {}

void Pl031::Get(GetCompleter::Sync& completer) {
  FidlRtc::wire::Time rtc = SecondsToRtc(MmioRead32(&regs_->dr));
  // TODO(https://fxbug.dev/42074113): Reply with error if RTC time is known to be invalid.
  completer.ReplySuccess(rtc);
}

void Pl031::Set(SetRequestView request, SetCompleter::Sync& completer) {
  completer.Reply(SetRtc(request->rtc));
}

void Pl031::Set2(Set2RequestView request, Set2Completer::Sync& completer) {
  zx_status_t status{SetRtc(request->rtc)};
  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.ReplySuccess();
  }
}

void Pl031::DdkRelease() { delete this; }

zx_status_t Pl031::SetRtc(FidlRtc::wire::Time rtc) {
  if (!IsRtcValid(rtc)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  MmioWrite32(static_cast<uint32_t>(SecondsSinceEpoch(rtc)), &regs_->lr);

  return ZX_OK;
}

zx_driver_ops_t pl031_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = Pl031::Bind,
};

}  // namespace rtc

ZIRCON_DRIVER(pl031, rtc::pl031_driver_ops, "zircon", "0.1");
