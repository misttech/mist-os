// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_PROVIDER_LIB_DEVFS_FACTORY_HLCPP_H_
#define SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_PROVIDER_LIB_DEVFS_FACTORY_HLCPP_H_

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <lib/fidl/cpp/binding_set.h>

#include <cstdint>
#include <map>
#include <memory>

#include "src/lib/fsl/io/device_watcher.h"

namespace sys {
class ComponentContext;
}  // namespace sys

namespace display {

// Implements the FIDL fuchsia.hardware.display.Provider API.  Only provides access to the primary
// controller, not the Virtcon controller.
class DevFsCoordinatorFactoryHlcpp : public fuchsia::hardware::display::Provider {
 public:
  // |app_context| is used to publish this service.
  explicit DevFsCoordinatorFactoryHlcpp(sys::ComponentContext* app_context);

  // |fuchsia::hardware::display::Provider|.
  void OpenCoordinatorForVirtcon(
      ::fidl::InterfaceRequest<fuchsia::hardware::display::Coordinator> coordinator,
      OpenCoordinatorForVirtconCallback callback) override {
    callback(ZX_ERR_NOT_SUPPORTED);
  }

  // |fuchsia::hardware::display::Provider|.
  void OpenCoordinatorForPrimary(
      ::fidl::InterfaceRequest<fuchsia::hardware::display::Coordinator> coordinator,
      OpenCoordinatorForPrimaryCallback callback) override;

  void BindDisplayProvider(fidl::InterfaceRequest<fuchsia::hardware::display::Provider> request);

 private:
  fidl::BindingSet<fuchsia::hardware::display::Provider> bindings_;

  // The currently outstanding DeviceWatcher closures.  The closures will remove
  // themselves from here if they are invoked before shutdown.  Any closures
  // still outstanding will be handled by the destructor.
  // This approach assumes that the event loop is attached to
  // the main thread, else race conditions may occur.
  std::map<uint64_t, std::unique_ptr<fsl::DeviceWatcher>> holders_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_PROVIDER_LIB_DEVFS_FACTORY_HLCPP_H_
