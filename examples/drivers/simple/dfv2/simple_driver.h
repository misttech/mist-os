// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_DRIVERS_SIMPLE_DFV2_SIMPLE_DRIVER_H_
#define EXAMPLES_DRIVERS_SIMPLE_DFV2_SIMPLE_DRIVER_H_

#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>

namespace simple {

// Simple driver that demonstrates how the Start(), PrepareStop(), and Stop() functions are invoked.
// This driver also shows how to add logging and how to add a child node.
class SimpleDriver : public fdf::DriverBase {
 public:
  SimpleDriver(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  // Only implemented in this example to demonstrate the driver lifecycle. Drivers should avoid
  // implementing the destructor and perform in Start() and PrepareStop().
  ~SimpleDriver();

  // Called by the Driver Framework to initialize the driver instance.
  zx::result<> Start() override;

  // Called by the Driver Framework before it shutdowns all of of the driver's fdf_dispatchers
  // The driver should use this function initiate any teardowns on the fdf_dispatchers before
  // they're stopped and deallocated by the Driver Framework.
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // Called by the Driver Framework after all the fdf_dispatchers belonging to this driver have
  // been shutdown and before it deallocates the driver.
  void Stop() override;

 private:
  // Required for maintaining the topological path.
  compat::SyncInitializedDeviceServer compat_server_;

  // Client for controller the child node.
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> child_controller_;
};

}  // namespace simple

#endif  // EXAMPLES_DRIVERS_SIMPLE_DFV2_SIMPLE_DRIVER_H_
