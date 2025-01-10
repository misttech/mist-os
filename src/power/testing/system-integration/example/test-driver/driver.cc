// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>

namespace {

class Driver final : public fdf::DriverBase {
 public:
  Driver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("example_driver", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    auto broker = incoming()->Connect<fuchsia_power_broker::Topology>();
    if (broker.is_error()) {
      FDF_LOG(INFO, "Failed to connect to broker");
    }

    auto sag = incoming()->Connect<fuchsia_power_system::ActivityGovernor>();
    if (sag.is_error()) {
      FDF_LOG(INFO, "Failed to connect to sag");
    }

    // Use the GetPowerElements call to see if we are successfully connected to the test realm's.
    fidl::Result power_elements = fidl::Call(*sag)->GetPowerElements();
    if (power_elements.is_error()) {
      FDF_LOG(INFO, "Failed to GetPowerElements from SAG: %s",
              power_elements.error_value().FormatDescription().c_str());
    } else {
      FDF_LOG(INFO, "Successfully did GetPowerElements.");
    }

    auto cpu_element = incoming()->Connect<fuchsia_power_system::CpuElementManager>();
    if (cpu_element.is_error()) {
      FDF_LOG(INFO, "Failed to connect to cpu element manager");
    }

    return zx::ok();
  }
};
}  // namespace

FUCHSIA_DRIVER_EXPORT(Driver);
