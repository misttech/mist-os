// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.framework/cpp/driver/fidl.h>
#include <fidl/fuchsia.driverhost.test/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/component/cpp/internal/start_args.h>
#include <lib/driver/component/cpp/internal/symbols.h>
#include <lib/driver/symbols/symbols.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/epitaph.h>

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf
namespace ftest = fuchsia_driverhost_test;

class TestDriver : public fdf::Server<fuchsia_driver_framework::Driver> {
 public:
  explicit TestDriver(fdf_dispatcher_t* dispatcher, fdf_handle_t server_handle)
      : dispatcher_(dispatcher), outgoing_(fdf_dispatcher_get_async_dispatcher(dispatcher)) {
    driver_binding_.emplace(
        dispatcher, fdf::ServerEnd<fuchsia_driver_framework::Driver>(fdf::Channel(server_handle)),
        this, fidl::kIgnoreBindingClosure);
  }

  void Start(StartRequest& request, StartCompleter::Sync& completer) override {
    auto& start_args = request.start_args();
    auto error = fdf_internal::SymbolValue<zx_status_t*>(start_args.symbols(), "error");
    if (error.is_ok()) {
      completer.Reply(zx::error(**error));
      return;
    }

    // Call the "func" driver symbol.
    auto func = fdf_internal::SymbolValue<void (*)()>(start_args.symbols(), "func");
    if (func.is_ok()) {
      (*func)();
    }

    // Set the "dispatcher" driver symbol.
    auto dispatcher =
        fdf_internal::SymbolValue<fdf_dispatcher_t**>(start_args.symbols(), "dispatcher");
    if (dispatcher.is_ok()) {
      **dispatcher = dispatcher_;
    }

    // Connect to the incoming service.
    auto svc_dir = fdf_internal::NsValue(start_args.incoming().value(), "/svc");
    if (svc_dir.is_error()) {
      completer.Reply(svc_dir.take_error());
      return;
    }
    auto client_end = component::ConnectAt<ftest::Incoming>(svc_dir.value());
    if (!client_end.is_ok()) {
      completer.Reply(client_end.take_error());
      return;
    }

    // Setup the outgoing service.
    auto status = outgoing_.AddUnmanagedProtocol<ftest::Outgoing>(
        [](fidl::ServerEnd<ftest::Outgoing> request) {
          fidl_epitaph_write(request.channel().get(), ZX_ERR_STOP);
        });
    if (status.is_error()) {
      completer.Reply(status);
      return;
    }

    completer.Reply(outgoing_.Serve(std::move(start_args.outgoing_dir().value())));
  }

  void Stop(StopCompleter::Sync& completer) override { driver_binding_.reset(); }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_driver_framework::Driver> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  fdf_dispatcher_t* dispatcher_;
  component::OutgoingDirectory outgoing_;
  std::optional<fdf::ServerBinding<fuchsia_driver_framework::Driver>> driver_binding_;
};

void* init(fdf_handle_t server_handle) {
  fdf_dispatcher_t* dispatcher = fdf_dispatcher_get_current_dispatcher();
  auto test_driver = std::make_unique<TestDriver>(dispatcher, server_handle);
  return test_driver.release();
}

void destroy(void* token) {
  if (token != nullptr) {
    delete static_cast<TestDriver*>(token);
  }
}

EXPORT_FUCHSIA_DRIVER_REGISTRATION_V1(.initialize = init, .destroy = destroy);
