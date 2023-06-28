// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_EXAMPLES_EXAMPLE_BOARD_EXAMPLE_BOARD_H_
#define LIB_DRIVER_DEVICETREE_EXAMPLES_EXAMPLE_BOARD_EXAMPLE_BOARD_H_

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devicetree/manager.h>
#include <lib/zx/result.h>

#include <optional>

namespace example_board {

// Example device tree based board driver
class ExampleBoard : public fdf::DriverBase {
 public:
  ExampleBoard(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("example-board", std::move(start_args), std::move(dispatcher)) {}
  zx::result<> Start() override;

 private:
  std::optional<fdf_devicetree::Manager> manager_;
  fidl::SyncClient<fuchsia_driver_framework::Node> node_;
};

}  // namespace example_board

#endif  // LIB_DRIVER_DEVICETREE_EXAMPLES_EXAMPLE_BOARD_EXAMPLE_BOARD_H_
