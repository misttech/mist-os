// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ENGINE_LISTENER_BANJO_ADAPTER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ENGINE_LISTENER_BANJO_ADAPTER_H_

#include <fuchsia/hardware/display/controller/cpp/banjo.h>

#include "src/graphics/display/drivers/coordinator/engine-listener.h"

namespace display_coordinator {

// Translates Banjo API calls to `EngineListener` C++ method calls.
//
// This adapter implements the
// [`fuchsia.hardware.display.controller/DisplayEngineListener`] Banjo API.
//
// Instances are thread-safe, because Banjo does not make any threading
// guarantees.
class EngineListenerBanjoAdapter
    : public ddk::DisplayEngineListenerProtocol<EngineListenerBanjoAdapter> {
 public:
  // `engine_listener` must be valid and outlive `dispatcher`.
  // `engine_listener` is only invoked on `dispatcher`.
  explicit EngineListenerBanjoAdapter(EngineListener* engine_listener,
                                      fdf::UnownedSynchronizedDispatcher dispatcher);

  // `fuchsia.hardware.display.controller/DisplayEngineListener`:
  void DisplayEngineListenerOnDisplayAdded(const raw_display_info_t* banjo_display_info);
  void DisplayEngineListenerOnDisplayRemoved(uint64_t banjo_display_id);
  void DisplayEngineListenerOnDisplayVsync(uint64_t banjo_display_id, zx_instant_mono_t timestamp,
                                           const config_stamp_t* config_stamp);
  void DisplayEngineListenerOnCaptureComplete();

  display_engine_listener_protocol_t GetProtocol();

 private:
  EngineListener& engine_listener_;
  fdf::UnownedSynchronizedDispatcher dispatcher_;
};

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ENGINE_LISTENER_BANJO_ADAPTER_H_
