// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "simple_driver.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include <bind/fuchsia/test/cpp/bind.h>

namespace simple {

SimpleDriver::SimpleDriver(fdf::DriverStartArgs start_args,
                           fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("simple_driver", std::move(start_args), std::move(driver_dispatcher)) {
  FDF_LOG(
      INFO,
      "SimpleDriver constructor invoked. This constructor is only implemented to"
      "demonstrate the driver lifecycle. Drivers are not expected to add implementation in the constructor");
}

SimpleDriver::~SimpleDriver() {
  FDF_LOG(
      INFO,
      "SimpleDriver destructor invoked after PrepareStop() and Stop() are called. "
      "This is only implemented to demonstrate the driver lifecycle. Drivers should avoid implementing the "
      "destructor and perform clean up in PrepareStop() and Stop() functions");
}

zx::result<> SimpleDriver::Start() {
  FDF_LOG(INFO,
          "SimpleDriver::Start() invoked. In this function, perform the driver "
          "initialization, such as adding children and setting up the compat server.");

  auto child_name = "simple_child";

  // Initialize our compat server.
  {
    zx::result<> result = compat_server_.Initialize(incoming(), outgoing(), node_name(), child_name,
                                                    compat::ForwardMetadata::None());
    if (result.is_error()) {
      return result.take_error();
    }
  }

  // Add a child node.
  fidl::Arena arena;
  auto properties = std::vector{fdf::MakeProperty(arena, bind_fuchsia_test::TEST_CHILD, "simple")};
  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, child_name)
                  .offers2(compat_server_.CreateOffers2(arena))
                  .properties(arena, std::move(properties))
                  .Build();

  auto [client_end, server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();
  child_controller_.Bind(std::move(client_end));

  fidl::WireResult result = fidl::WireCall(node())->AddChild(args, std::move(server_end), {});
  if (!result.ok()) {
    FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
    return zx::error(result.status());
  }

  return zx::ok();
}

void SimpleDriver::PrepareStop(fdf::PrepareStopCompleter completer) {
  FDF_LOG(INFO,
          "SimpleDriver::PrepareStop() invoked. This is called before "
          "the driver dispatchers are shutdown. Only implement this function "
          "if you need to manually clearn up objects (ex/ unique_ptrs) in the driver dispatchers");
  completer(zx::ok());
}

void SimpleDriver::Stop() {
  FDF_LOG(INFO,
          "SimpleDriver::Stop() invoked. This is called after all driver dispatchers are "
          "shutdown. Use this function to perform any remaining teardowns");
}

}  // namespace simple

FUCHSIA_DRIVER_EXPORT(simple::SimpleDriver);
