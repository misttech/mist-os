// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/drivers/template/template_driver.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/logger.h>

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
  auto properties = std::vector{fdf::MakeProperty(bind_fuchsia_test::TEST_CHILD, "simple")};
  zx::result child_result = AddChild(child_name, properties, {});
  if (child_result.is_error()) {
    return child_result.take_error();
  }

  child_controller_.Bind(std::move(child_result.value()));
  return zx::ok();
}

void TemplateDriver::PrepareStop(fdf::PrepareStopCompleter completer) {
  fdf::info(
      "TemplateDriver::PrepareStop() invoked. This is called before "
      "the driver dispatchers are shutdown. Only implement this function "
      "if you need to manually clearn up objects (ex/ unique_ptrs) in the driver dispatchers");
  completer(zx::ok());
}

void TemplateDriver::Stop() {
  fdf::info(
      "TemplateDriver::Stop() invoked. This is called after all driver dispatchers are "
      "shutdown. Use this function to perform any remaining teardowns");
}

}  // namespace template_driver

FUCHSIA_DRIVER_EXPORT(template_driver::TemplateDriver);
