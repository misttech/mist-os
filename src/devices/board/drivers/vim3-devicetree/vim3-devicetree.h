// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_DRIVERS_VIM3_DEVICETREE_VIM3_DEVICETREE_H_
#define SRC_DEVICES_BOARD_DRIVERS_VIM3_DEVICETREE_VIM3_DEVICETREE_H_

#include <lib/driver/component/cpp/driver_base.h>

#include <optional>

#include "sdk/lib/driver/devicetree/manager.h"

namespace vim3_dt {

// Vim3 board driver based on device tree
class Vim3Devicetree : public fdf::DriverBase {
 public:
  Vim3Devicetree(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("vim3-devicetree", std::move(start_args), std::move(dispatcher)) {}

  zx::result<> Start() final;

 private:
  std::optional<fdf_devicetree::Manager> manager_;
  fidl::SyncClient<fuchsia_driver_framework::Node> node_;
};

}  // namespace vim3_dt

#endif  // SRC_DEVICES_BOARD_DRIVERS_VIM3_DEVICETREE_VIM3_DEVICETREE_H_
