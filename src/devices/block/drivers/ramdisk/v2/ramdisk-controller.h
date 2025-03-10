// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_RAMDISK_V2_RAMDISK_CONTROLLER_H_
#define SRC_DEVICES_BLOCK_DRIVERS_RAMDISK_V2_RAMDISK_CONTROLLER_H_

#include <fidl/fuchsia.driver.framework/cpp/natural_messaging.h>
#include <fidl/fuchsia.hardware.ramdisk/cpp/wire.h>
#include <lib/async/cpp/wait.h>
#include <lib/driver/component/cpp/driver_base.h>

#include <memory>

namespace ramdisk_v2 {

class Ramdisk;

class RamdiskController : public fdf::DriverBase,
                          public fidl::WireServer<fuchsia_hardware_ramdisk::Controller> {
 public:
  RamdiskController(fdf::DriverStartArgs start_args,
                    fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  zx::result<> Start() override;

  using fdf::DriverBase::outgoing;

 private:
  // FIDL Interface Controller.
  void Create(CreateRequestView request, CreateCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_ramdisk::Controller> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  fidl::SharedClient<fuchsia_driver_framework::Node> node_client_;
  std::unordered_map<int, std::pair<std::unique_ptr<Ramdisk>, std::unique_ptr<async::WaitOnce>>>
      ramdisks_;
};

}  // namespace ramdisk_v2

#endif  // SRC_DEVICES_BLOCK_DRIVERS_RAMDISK_V2_RAMDISK_CONTROLLER_H_
