// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/drivers/template/template_driver.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include <bind/fuchsia/test/cpp/bind.h>

namespace template_driver {

TemplateDriver::TemplateDriver(fdf::DriverStartArgs start_args,
                               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("template_driver", std::move(start_args), std::move(driver_dispatcher)) {}

zx::result<> TemplateDriver::Start() {
  // Instructions: Put driver initialization logic in this function, such as adding children
  // and setting up client-server transport connections.
  // If the initialization logic is asynchronous, prefer to override
  // DriverBase::Start(fdf::StartCompleter completer) over this function.

  // Set up an example child node for testing purposes
  auto child_name = "example_child";

  // Add a child node.
  fidl::Arena arena;
  auto properties = std::vector{fdf::MakeProperty(arena, bind_fuchsia_test::TEST_CHILD, "simple")};
  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, child_name)
                  .properties(arena, std::move(properties))
                  .Build();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed to create endpoints: %s",
                controller_endpoints.status_string());
  child_controller_.Bind(std::move(controller_endpoints->client));

  fidl::WireResult result =
      fidl::WireCall(node())->AddChild(args, std::move(controller_endpoints->server), {});
  if (!result.ok()) {
    FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
    return zx::error(result.status());
  }

  return zx::ok();
}

void TemplateDriver::PrepareStop(fdf::PrepareStopCompleter completer) {
  FDF_LOG(INFO,
          "TemplateDriver::PrepareStop() invoked. This is called before "
          "the driver dispatchers are shutdown. Only implement this function "
          "if you need to manually clearn up objects (ex/ unique_ptrs) in the driver dispatchers");
  completer(zx::ok());
}

void TemplateDriver::Stop() {
  FDF_LOG(INFO,
          "TemplateDriver::Stop() invoked. This is called after all driver dispatchers are "
          "shutdown. Use this function to perform any remaining teardowns");
}

}  // namespace template_driver

FUCHSIA_DRIVER_EXPORT(template_driver::TemplateDriver);
