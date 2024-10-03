// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device.h"

#include <fidl/fuchsia.wlan.fullmac/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/fit/function.h>

#include "debug.h"

namespace wlanif {

Device::Device(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("wlanif", std::move(start_args), std::move(driver_dispatcher)),
      rust_mlme_(nullptr, delete_fullmac_mlme_handle) {
  auto logger = fdf::Logger::Create(*incoming(), dispatcher(), "wlanif", FUCHSIA_LOG_INFO);
  logger_ = std::move(logger);
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

  // |rust_mlme_| will join the MLME thread when it is destructed by calling
  // |delete_fullmac_mlme_handle|.
  // |rust_mlme_| will call Device::MlmeShutdownCallback on its thread when it exits.
  // |this| should always outlive |rust_mlme_|, so it's safe to pass |this| to the MLME.
  rust_mlme_ =
      RustFullmacMlme(start_fullmac_mlme(client_end_handle, this, &Device::MlmeShutdownCallback),
                      delete_fullmac_mlme_handle);
  if (!rust_mlme_) {
    FDF_LOGL(ERROR, *logger_, "Rust MLME is not valid");
    return zx::error(ZX_ERR_BAD_HANDLE);
  }

  return zx::ok();
}

void Device::PrepareStop(fdf::PrepareStopCompleter completer) {
  std::lock_guard guard(lock_);
  ZX_DEBUG_ASSERT(rust_mlme_);

  if (mlme_exit_status_.has_value()) {
    // MLME exited before PrepareStop is called.
    // Complete PrepareStop immediately with the status stored in |shutdown_state_|.
    completer(zx::make_result(mlme_exit_status_.value()));

    // Note: we don't call stop_fullmac_mlme in this path because MLME has already exited. This will
    // result in warning or error messages when |rust_mlme_| is dropped. But the warning messages
    // are fine since this is already an unexpected way for wlanif::Device to shut down.
  } else {
    // Expected case: PrepareStop is called before MLME exits. Tell MLME to shutdown.
    stop_fullmac_mlme(rust_mlme_.get());
    prepare_stop_completer_.emplace(std::move(completer));
  }
}

// Note: this function is passed to MLME and will run on the Rust MLME thread.
// Any data shared between this callback and other functions in wlanif::Device should be
// synchronized.
//
// This function assumes that |self_ptr| points to a valid instance of Device.
// Since it's only called by |rust_mlme_| on shutdown, |self_ptr| should always be valid.
void Device::MlmeShutdownCallback(void* self_ptr, zx_status_t status) {
  Device* self = reinterpret_cast<Device*>(self_ptr);

  std::lock_guard guard(self->lock_);
  if (self->prepare_stop_completer_.has_value()) {
    // PrepareStop called before MLME exit.
    // Complete the saved |prepare_stop_completer_|.
    self->prepare_stop_completer_.value()(zx::make_result(status));
    self->prepare_stop_completer_.reset();
  } else {
    // MLME exits before PrepareStop is called.
    ZX_DEBUG_ASSERT(!self->mlme_exit_status_.has_value());
    self->mlme_exit_status_.emplace(status);

    // Request that the Driver Framework begins tearing down wlanif::Device by resetting
    // |node()|.
    self->node().reset();
  }
}

}  // namespace wlanif
FUCHSIA_DRIVER_EXPORT(::wlanif::Device);
