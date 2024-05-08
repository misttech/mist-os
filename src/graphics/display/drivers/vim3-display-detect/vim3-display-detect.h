// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIM3_DISPLAY_DETECT_VIM3_DISPLAY_DETECT_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIM3_DISPLAY_DETECT_VIM3_DISPLAY_DETECT_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/stdcompat/span.h>

#include <cstdint>
#include <string_view>

#include <bind/fuchsia/display/cpp/bind.h>

namespace vim3 {

class Vim3DisplayDetect : public fdf::DriverBase {
 public:
  Vim3DisplayDetect(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("vim3-display-detect", std::move(start_args), std::move(dispatcher)) {}

  void Start(fdf::StartCompleter completer) override;

 private:
  zx::result<> DetectDisplay();

  zx::result<uint8_t> ReadGpio(std::string_view gpio_node_name);

  fidl::SyncClient<fuchsia_driver_framework::Node> parent_;
  fidl::SyncClient<fuchsia_driver_framework::NodeController> controller_;
};

}  // namespace vim3

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIM3_DISPLAY_DETECT_VIM3_DISPLAY_DETECT_H_
