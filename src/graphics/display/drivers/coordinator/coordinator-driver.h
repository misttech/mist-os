// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_COORDINATOR_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_COORDINATOR_DRIVER_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <lib/ddk/device.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>

#include "src/graphics/display/drivers/coordinator/controller.h"

namespace display {

class CoordinatorDriver;

using DeviceType = ddk::Device<CoordinatorDriver, ddk::Unbindable,
                               ddk::Messageable<fuchsia_hardware_display::Provider>::Mixin>;

// Interfaces with the Driver Framework v1 and manages the driver lifetime of
// the display coordinator Controller device.
class CoordinatorDriver : public DeviceType,
                          public ddk::EmptyProtocol<ZX_PROTOCOL_DISPLAY_COORDINATOR> {
 public:
  // Factory method for production use.
  //
  // `parent` must not be null.
  static zx::result<> Create(zx_device_t* parent);

  // Production code must use the `Create()` factory method.
  //
  // `parent` and `controller` must not be null.
  explicit CoordinatorDriver(zx_device_t* parent, std::unique_ptr<Controller> controller);

  CoordinatorDriver(const CoordinatorDriver&) = delete;
  CoordinatorDriver(CoordinatorDriver&&) = delete;
  CoordinatorDriver& operator=(const CoordinatorDriver&) = delete;
  CoordinatorDriver& operator=(CoordinatorDriver&&) = delete;

  ~CoordinatorDriver() override;

  // ddk::Unbindable:
  void DdkUnbind(ddk::UnbindTxn txn);

  // ddk::Device:
  void DdkRelease();

  // fuchsia_hardware_display::Provider:
  void OpenCoordinatorForVirtcon(OpenCoordinatorForVirtconRequestView request,
                                 OpenCoordinatorForVirtconCompleter::Sync& completer) override;

  void OpenCoordinatorForPrimary(OpenCoordinatorForPrimaryRequestView request,
                                 OpenCoordinatorForPrimaryCompleter::Sync& completer) override;

  Controller* controller() const { return controller_.get(); }

 private:
  // Binds the coordinator driver to the driver manager.
  zx::result<> Bind();

  std::unique_ptr<Controller> controller_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_COORDINATOR_DRIVER_H_
