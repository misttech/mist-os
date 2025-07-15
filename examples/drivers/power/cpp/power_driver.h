// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_DRIVERS_POWER_CPP_POWER_DRIVER_H_
#define EXAMPLES_DRIVERS_POWER_CPP_POWER_DRIVER_H_

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/power/cpp/suspend.h>

namespace power {

// Power driver that demonstrates how the Suspend() and Resume() functions are registered and
// invoked.
class PowerDriver final : public fdf::DriverBase, public fdf_power::Suspendable<PowerDriver> {
 public:
  PowerDriver(fdf::DriverStartArgs start_args,
              fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  // Only implemented in this example to demonstrate the driver lifecycle. Drivers should avoid
  // implementing the destructor and perform in Start() and PrepareStop().
  ~PowerDriver();

  // Called by the Driver Framework to initialize the driver instance.
  zx::result<> Start() override;

  // Called by the Driver Framework before it shutdowns all of of the driver's fdf_dispatchers
  // The driver should use this function initiate any teardowns on the fdf_dispatchers before
  // they're stopped and deallocated by the Driver Framework.
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // Called by the Driver Framework after all the fdf_dispatchers belonging to this driver have
  // been shutdown and before it deallocates the driver.
  void Stop() override;

  void Suspend(fdf_power::SuspendCompleter cb) override;
  void Resume(fdf_power::ResumeCompleter cb) override;
};

}  // namespace power

#endif  // EXAMPLES_DRIVERS_POWER_CPP_POWER_DRIVER_H_
