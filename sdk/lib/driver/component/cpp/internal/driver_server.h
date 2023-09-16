// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_INTERNAL_DRIVER_SERVER_H_
#define LIB_DRIVER_COMPONENT_CPP_INTERNAL_DRIVER_SERVER_H_

#include <zircon/availability.h>

#if __Fuchsia_API_level__ >= FUCHSIA_HEAD

#include <fidl/fuchsia.driver.framework/cpp/driver/wire.h>
#include <fidl/fuchsia.driver.framework/cpp/type_conversions.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/prepare_stop_completer.h>
#include <lib/driver/component/cpp/start_completer.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/symbols/symbols.h>

namespace fdf_internal {

// This will shim a |DriverBase| based driver with the new FIDL based registration.
template <typename DriverBaseImpl>
class DriverServer : public fdf::WireServer<fuchsia_driver_framework::Driver> {
  static_assert(std::is_base_of_v<fdf::DriverBase, DriverBaseImpl>,
                "The driver type must implement the fdf::DriverBase class.");
  static_assert(std::is_constructible_v<DriverBaseImpl, fuchsia_driver_framework::DriverStartArgs,
                                        fdf::UnownedSynchronizedDispatcher>,
                "The driver must be constructible from "
                "(DriverStartArgs, fdf::UnownedSynchronizedDispatcher)");

 public:
  // Initialize the fuchsia_driver_framework::Driver server.
  static void* initialize(fdf_handle_t server_handle) {
    fdf_dispatcher_t* dispatcher = fdf_dispatcher_get_current_dispatcher();
    DriverServer* driver_server = new DriverServer(dispatcher, server_handle);
    return driver_server;
  }

  // Destroy the fuchsia_driver_framework::Driver server.
  static void destroy(void* token) {
    DriverServer* driver_server = static_cast<DriverServer*>(token);
    delete driver_server;
  }

  DriverServer(fdf_dispatcher_t* dispatcher, fdf_handle_t server_handle) : dispatcher_(dispatcher) {
    binding_.emplace(dispatcher_,
                     fdf::ServerEnd<fuchsia_driver_framework::Driver>(fdf::Channel(server_handle)),
                     this, fidl::kIgnoreBindingClosure);
  }

  virtual ~DriverServer() {
    if (driver_) {
      driver_->Stop();
    }
  }

  void Start(StartRequestView request, fdf::Arena& arena,
             StartCompleter::Sync& completer) override {
    driver_ = std::make_unique<DriverBaseImpl>(fidl::ToNatural(request->start_args),
                                               fdf::UnownedSynchronizedDispatcher(dispatcher_));
    driver_->Start(fdf::StartCompleter(
        [arena = std::move(arena), completer = completer.ToAsync()](zx::result<> result) mutable {
          completer.buffer(arena).Reply(result);
        }));
  }

  void Stop(fdf::Arena& arena, StopCompleter::Sync& completer) override {
    ZX_ASSERT(driver_);
    driver_->PrepareStop(
        fdf::PrepareStopCompleter([this](zx::result<> result) { binding_.reset(); }));
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_driver_framework::Driver> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    if (driver_) {
      FDF_LOGL(INFO, driver_->logger(), "fdf::Driver server received unknown method.");
    }
  }

  void* GetDriverBaseImpl() {
    if (driver_) {
      return driver_.get();
    }

    return nullptr;
  }

 private:
  fdf_dispatcher_t* dispatcher_;
  std::optional<fdf::ServerBinding<fuchsia_driver_framework::Driver>> binding_;
  std::unique_ptr<fdf::DriverBase> driver_;
};
}  // namespace fdf_internal

#endif

#endif  // LIB_DRIVER_COMPONENT_CPP_INTERNAL_DRIVER_SERVER_H_
