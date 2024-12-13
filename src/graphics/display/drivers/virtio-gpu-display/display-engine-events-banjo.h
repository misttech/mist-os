// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_DISPLAY_ENGINE_EVENTS_BANJO_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_DISPLAY_ENGINE_EVENTS_BANJO_H_

#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/stdcompat/span.h>
#include <zircon/compiler.h>

#include <fbl/mutex.h>

#include "src/graphics/display/drivers/virtio-gpu-display/display-engine-events-interface.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/mode.h"

namespace virtio_display {

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
                      cpp20::span<const display::Mode> preferred_modes,
                      cpp20::span<const fuchsia_images2::wire::PixelFormat> pixel_formats) override;
  void OnDisplayRemoved(display::DisplayId display_id) override;
  void OnDisplayVsync(display::DisplayId display_id, zx::time timestamp,
                      display::ConfigStamp config_stamp) override;
  void OnCaptureComplete() override;

 private:
  fbl::Mutex event_mutex_;
  display_engine_listener_protocol_t display_engine_listener_ __TA_GUARDED(event_mutex_) = {};
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_DISPLAY_ENGINE_EVENTS_BANJO_H_
