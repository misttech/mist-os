// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_DISPLAY_ENGINE_EVENTS_BANJO_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_DISPLAY_ENGINE_EVENTS_BANJO_H_

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/time.h>
#include <zircon/compiler.h>

#include <mutex>

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-events-interface.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/mode-and-id.h"
#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"

namespace display {

// Translates `DisplayEngineEventsInterface` C++ method calls to Banjo.
//
// This adapter targets the
// [`fuchsia.hardware.display.controller/DisplayEngineListener`] Banjo API.
//
// Instances are thread-safe, because Banjo does not make any threading
// guarantees.
class DisplayEngineEventsBanjo final : public DisplayEngineEventsInterface {
 public:
  explicit DisplayEngineEventsBanjo();

  DisplayEngineEventsBanjo(const DisplayEngineEventsBanjo&) = delete;
  DisplayEngineEventsBanjo& operator=(const DisplayEngineEventsBanjo&) = delete;

  ~DisplayEngineEventsBanjo();

  // `display_engine_listener` may be null.
  void SetListener(const display_engine_listener_protocol_t* display_engine_listener);

  // DisplayEngineEventsInterface:
  void OnDisplayAdded(display::DisplayId display_id,
                      cpp20::span<const display::ModeAndId> preferred_modes,
                      cpp20::span<const display::PixelFormat> pixel_formats) override;
  void OnDisplayRemoved(display::DisplayId display_id) override;
  void OnDisplayVsync(display::DisplayId display_id, zx::time timestamp,
                      display::DriverConfigStamp config_stamp) override;
  void OnCaptureComplete() override;

 private:
  std::mutex event_mutex_;
  ddk::DisplayEngineListenerProtocolClient display_engine_listener_ __TA_GUARDED(event_mutex_);
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_DISPLAY_ENGINE_EVENTS_BANJO_H_
