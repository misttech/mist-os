// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_GOLDFISH_DISPLAY_DISPLAY_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_GOLDFISH_DISPLAY_DISPLAY_DRIVER_H_

#include <lib/ddk/device.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/zx/result.h>

#include <cstdint>
#include <memory>

#include <ddktl/device.h>

#include "src/graphics/display/drivers/goldfish-display/display-engine.h"

namespace goldfish {

class DisplayDriver;
using DisplayType = ddk::Device<DisplayDriver, ddk::GetProtocolable>;

class DisplayDriver : public DisplayType {
 public:
  // Factory method used by the device manager glue code.
  //
  // `parent` must not be null.
  static zx::result<> Create(zx_device_t* parent);

  // Prefer to use the `Create()` factory method instead.
  //
  // `parent` must not be null.
  // `display_event_dispatcher` must be valid.
  // `display_engine` must not be null.
  explicit DisplayDriver(zx_device_t* parent, fdf::SynchronizedDispatcher display_event_dispatcher,
                         std::unique_ptr<DisplayEngine> display_engine);

  DisplayDriver(const DisplayDriver&) = delete;
  DisplayDriver(DisplayDriver&&) = delete;
  DisplayDriver& operator=(const DisplayDriver&) = delete;
  DisplayDriver& operator=(DisplayDriver&&) = delete;

  ~DisplayDriver();

  // ddk::Device
  void DdkRelease();

  // ddk::GetProtocolable
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out);

  zx::result<> Bind();

 private:
  // Must outlive `display_engine_`.
  fdf::SynchronizedDispatcher display_event_dispatcher_;

  std::unique_ptr<DisplayEngine> display_engine_;
};

}  // namespace goldfish

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_GOLDFISH_DISPLAY_DISPLAY_DRIVER_H_
