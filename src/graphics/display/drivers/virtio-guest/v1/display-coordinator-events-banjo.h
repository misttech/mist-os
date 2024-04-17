// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_COORDINATOR_EVENTS_BANJO_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_COORDINATOR_EVENTS_BANJO_H_

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/stdcompat/span.h>
#include <zircon/compiler.h>

#include <ddktl/device.h>
#include <fbl/mutex.h>

#include "src/graphics/display/drivers/virtio-guest/v1/display-coordinator-events-interface.h"

namespace virtio_display {

// Banjo <-> C++ bridge for the events interface with the Display Coordinator.
//
// Instances are thread-safe, because Banjo does not make any threading
// guarantees.
class DisplayCoordinatorEventsBanjo final : public DisplayCoordinatorEventsInterface {
 public:
  explicit DisplayCoordinatorEventsBanjo();

  DisplayCoordinatorEventsBanjo(const DisplayCoordinatorEventsBanjo&) = delete;
  DisplayCoordinatorEventsBanjo& operator=(const DisplayCoordinatorEventsBanjo&) = delete;

  ~DisplayCoordinatorEventsBanjo();

  // `display_controller_interface` may be null.
  void SetDisplayControllerInterface(
      const display_controller_interface_protocol_t* display_controller_interface);

  // DisplayCoordinatorEventsInterface:
  void OnDisplaysChanged(cpp20::span<const added_display_args_t> added_displays,
                         cpp20::span<const display::DisplayId> removed_display_ids) override;
  void OnDisplayVsync(display::DisplayId display_id, zx::time timestamp,
                      display::ConfigStamp config_stamp) override;
  void OnCaptureComplete() override;

 private:
  fbl::Mutex event_mutex_;
  display_controller_interface_protocol_t display_controller_interface_
      __TA_GUARDED(event_mutex_) = {};
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_COORDINATOR_EVENTS_BANJO_H_
