// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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

class TestDriver {
 public:
  explicit TestDriver(fdf_dispatcher_t* dispatcher)
      : dispatcher_(dispatcher), outgoing_(fdf_dispatcher_get_async_dispatcher(dispatcher)) {}

  zx::result<> Init(fdf::wire::DriverStartArgs& start_args) {
    auto error = fdf_internal::SymbolValue<zx_status_t*>(start_args, "error");
    if (error.is_ok()) {
      return zx::error(**error);
    }

    // Call the "func" driver symbol.
    auto func = fdf_internal::SymbolValue<void (*)()>(start_args, "func");
    if (func.is_ok()) {
      (*func)();
    }

    // Set the "dispatcher" driver symbol.
    auto dispatcher = fdf_internal::SymbolValue<fdf_dispatcher_t**>(start_args, "dispatcher");
    if (dispatcher.is_ok()) {
      **dispatcher = dispatcher_;
    }

    // Connect to the incoming service.
    auto svc_dir = fdf_internal::NsValue(start_args.incoming(), "/svc");
    if (svc_dir.is_error()) {
      return svc_dir.take_error();
    }
    auto client_end = component::ConnectAt<ftest::Incoming>(svc_dir.value());
    if (!client_end.is_ok()) {
      return client_end.take_error();
    }

    // Setup the outgoing service.
    auto status = outgoing_.AddUnmanagedProtocol<ftest::Outgoing>(
        [](fidl::ServerEnd<ftest::Outgoing> request) {
          fidl_epitaph_write(request.channel().get(), ZX_ERR_STOP);
        });
    if (status.is_error()) {
      return status;
    }

    return outgoing_.Serve(std::move(start_args.outgoing_dir()));
  }

 private:
  fdf_dispatcher_t* dispatcher_;
  component::OutgoingDirectory outgoing_;
};

void test_driver_start(EncodedDriverStartArgs encoded_start_args, fdf_dispatcher_t* dispatcher,
                       StartCompleteCallback* complete, void* complete_cookie) {
  auto wire_format_metadata =
      fidl::WireFormatMetadata::FromOpaque(encoded_start_args.wire_format_metadata);
  fdf_internal::AdoptEncodedFidlMessage encoded{encoded_start_args.msg};
  fit::result decoded = fidl::StandaloneInplaceDecode<fdf::wire::DriverStartArgs>(
      encoded.TakeMessage(), wire_format_metadata);
  if (!decoded.is_ok()) {
    complete(complete_cookie, decoded.error_value().status(), nullptr);
    return;
  }

  auto test_driver = std::make_unique<TestDriver>(dispatcher);
  auto init = test_driver->Init(*decoded.value());
  if (init.is_error()) {
    complete(complete_cookie, init.error_value(), nullptr);
    return;
  }

  complete(complete_cookie, ZX_OK, test_driver.release());
}

void test_driver_prepare_stop(void* driver, PrepareStopCompleteCallback* complete,
                              void* complete_cookie) {
  complete(complete_cookie, ZX_OK);
}

zx_status_t test_driver_stop(void* driver) {
  if (driver != nullptr) {
    delete static_cast<TestDriver*>(driver);
  }
  return ZX_OK;
}

FUCHSIA_DRIVER_LIFECYCLE_V3(.start = test_driver_start, .prepare_stop = test_driver_prepare_stop,
                            .stop = test_driver_stop);
