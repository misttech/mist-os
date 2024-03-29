// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_DRIVERS_template_template_DRIVER_H_
#define EXAMPLES_DRIVERS_template_template_DRIVER_H_

#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>

namespace template_driver {

// Your copy-pasteable SDK-compatible template driver, with basic functionality, unit testing.
class TemplateDriver : public fdf::DriverBase {
 public:
  TemplateDriver(fdf::DriverStartArgs start_args,
                 fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  // Called by the driver framework to initialize the driver instance.
  zx::result<> Start() override;

  // Called by the Driver Framework before it shutdowns all of of the driver's fdf_dispatchers
  // The driver should use this function initiate any teardowns on the fdf_dispatchers before
  // they're stopped and deallocated by the Driver Framework.
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // Called by the Driver Framework after all the fdf_dispatchers belonging to this driver have
  // been shutdown and before it deallocates the driver.
  void Stop() override;

 private:
  // Client for controller the child node.
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> child_controller_;
};

}  // namespace template_driver

#endif  // EXAMPLES_DRIVERS_template_template_DRIVER_H_
