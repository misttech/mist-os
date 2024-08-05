// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "usb-dci-interface-server.h"

#include <fidl/fuchsia.hardware.usb.dci/cpp/wire.h>

#include "src/devices/usb/drivers/usb-peripheral/usb-peripheral.h"

// This header appears unused, but is required to appease the forward declaration of UsbFunction
// brought in by way of usb-peripheral.h (which is required).
#include "src/devices/usb/drivers/usb-peripheral/usb-function.h"

namespace usb_peripheral {

void UsbDciInterfaceServer::Control(ControlRequestView req, ControlCompleter::Sync& completer) {
  fidl::Arena arena;
  fidl::VectorView<uint8_t> read_data(arena, req->setup.w_length);  // May be 0.

  // convert the FIDL ::UsbSetup struct into a banjo usb_setup_t.
  // clang-format off
  usb_setup_t bsetup{
      req->setup.bm_request_type,
      req->setup.b_request,
      req->setup.w_value,
      req->setup.w_index,
      req->setup.w_length,
  };
  // clang-format on

  // Some lightweight spans for convenience.
  cpp20::span<uint8_t> span_write = req->write.get();
  cpp20::span<uint8_t> span_read = read_data.get();

  size_t actual;  // Unused by FIDL, size encoded in returned read_data vector.
  zx_status_t status = drv_->CommonControl(&bsetup, span_write.data(), span_write.size_bytes(),
                                           span_read.data(), span_read.size_bytes(), &actual);

  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.buffer(arena).ReplySuccess(read_data);
  }
}

void UsbDciInterfaceServer::SetConnected(SetConnectedRequestView req,
                                         SetConnectedCompleter::Sync& completer) {
  drv_->CommonSetConnected(req->is_connected);
  completer.ReplySuccess();
}

void UsbDciInterfaceServer::SetSpeed(SetSpeedRequestView req, SetSpeedCompleter::Sync& completer) {
  drv_->speed_ = static_cast<usb_speed_t>(req->speed);
  completer.ReplySuccess();
}

}  // namespace usb_peripheral
