// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_DISPLAY_ENGINE_EVENTS_INTERFACE_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_DISPLAY_ENGINE_EVENTS_INTERFACE_H_

#include <lib/stdcompat/span.h>
#include <lib/zx/time.h>

#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/mode-and-id.h"
#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"

namespace display {

// The events in the [`fuchsia.hardware.display.engine/Engine`] FIDL interface.
//
// This abstract base class only represents the events in the FIDL interface.
// The methods are represented by `DisplayEngineInterface`.
//
// This abstract base class also represents the
// [`fuchsia.hardware.display.controller/DisplayEngineListener`] Banjo
// interface.
class DisplayEngineEventsInterface {
 public:
  DisplayEngineEventsInterface() = default;

  DisplayEngineEventsInterface(const DisplayEngineEventsInterface&) = delete;
  DisplayEngineEventsInterface(DisplayEngineEventsInterface&&) = delete;
  DisplayEngineEventsInterface& operator=(const DisplayEngineEventsInterface&) = delete;
  DisplayEngineEventsInterface& operator=(DisplayEngineEventsInterface&&) = delete;

  virtual void OnDisplayAdded(display::DisplayId display_id,
                              cpp20::span<const display::ModeAndId> preferred_modes,
                              cpp20::span<const display::PixelFormat> pixel_formats) = 0;
  virtual void OnDisplayRemoved(display::DisplayId display_id) = 0;
  virtual void OnDisplayVsync(display::DisplayId display_id, zx::time timestamp,
                              display::DriverConfigStamp config_stamp) = 0;
  virtual void OnCaptureComplete() = 0;

 protected:
  // Destruction via base class pointer is not supported intentionally.
  // Instances are not expected to be owned by pointers to base classes.
  ~DisplayEngineEventsInterface() = default;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_DISPLAY_ENGINE_EVENTS_INTERFACE_H_
