// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device.h"

#include <fidl/fuchsia.wlan.fullmac/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_export.h>

#include "debug.h"

namespace wlanif {

Device::Device(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("wlanif", std::move(start_args), std::move(driver_dispatcher)),
      rust_mlme_(nullptr, delete_fullmac_mlme),
      parent_node_(fidl::WireClient(std::move(node()), dispatcher())) {
  zx::result logger = fdf::Logger::Create(*incoming(), dispatcher(), "wlanif", FUCHSIA_LOG_INFO);
  ZX_ASSERT_MSG(logger.is_ok(), "%s", logger.status_string());
  logger_ = std::move(logger.value());
  ltrace_fn(*logger_);
}

Device::~Device() { ltrace_fn(*logger_); }

zx::result<> Device::Start() {
  zx::result<fidl::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImpl>> client_end =
      incoming()->Connect<fuchsia_wlan_fullmac::Service::WlanFullmacImpl>();
  if (client_end.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Connect to FullmacImpl failed: %s", client_end.status_string());
    return client_end.take_error();
  }

  zx_handle_t client_end_handle = client_end.value().TakeHandle().release();
  std::lock_guard lock(rust_mlme_lock_);
  rust_mlme_ = RustFullmacMlme(start_fullmac_mlme(client_end_handle), delete_fullmac_mlme);
  if (!rust_mlme_) {
    FDF_LOGL(ERROR, *logger_, "Rust MLME is not valid");
    return zx::error(ZX_ERR_BAD_HANDLE);
  }

  return zx::ok();
}

void Device::PrepareStop(fdf::PrepareStopCompleter completer) {
  std::lock_guard lock(rust_mlme_lock_);
  if (rust_mlme_) {
    stop_fullmac_mlme(rust_mlme_.get());
  }
  completer(zx::ok());
}

}  // namespace wlanif
FUCHSIA_DRIVER_EXPORT(::wlanif::Device);
