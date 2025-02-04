// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DISPLAY_INFO_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DISPLAY_INFO_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/audiotypes/c/banjo.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/inspect/cpp/inspect.h>

#include <cstdint>
#include <optional>
#include <queue>
#include <string_view>
#include <vector>

#include <fbl/array.h>
#include <fbl/ref_counted.h>
#include <fbl/string_printf.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/coordinator/client-id.h"
#include "src/graphics/display/drivers/coordinator/id-map.h"
#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/drivers/coordinator/migration-util.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/image-id.h"
#include "src/graphics/display/lib/edid/edid.h"

namespace display_coordinator {

class DisplayInfo : public IdMappable<fbl::RefPtr<DisplayInfo>, display::DisplayId>,
                    public fbl::RefCounted<DisplayInfo> {
 public:
  static zx::result<fbl::RefPtr<DisplayInfo>> Create(const raw_display_info_t& banjo_display_info);

  DisplayInfo(const DisplayInfo&) = delete;
  DisplayInfo(DisplayInfo&&) = delete;
  DisplayInfo& operator=(const DisplayInfo&) = delete;
  DisplayInfo& operator=(DisplayInfo&&) = delete;

  ~DisplayInfo();

  // Should be called after init_done is set to true.
  void InitializeInspect(inspect::Node* parent_node);

  // Guaranteed to be >= 0 and < 2^16.
  // Returns zero if the information is not available.
  int GetHorizontalSizeMm() const;

  // Guaranteed to be >= 0 and < 2^16.
  // Returns zero if the information is not available.
  int GetVerticalSizeMm() const;

  // Returns an empty view if the information is not available.
  // The returned string view is guaranteed to be of static storage duration.
  std::string_view GetManufacturerName() const;

  // Returns an empty string if the information is not available.
  std::string GetMonitorName() const;

  // Returns an empty string if the information is not available.
  std::string GetMonitorSerial() const;

  struct Edid {
    edid::Edid base;
    fbl::Vector<display::DisplayTiming> timings;
  };

  // Exactly one of `edid` and `mode` can be non-nullopt.
  std::optional<Edid> edid;
  std::optional<display::DisplayTiming> mode;

  fbl::Vector<CoordinatorPixelFormat> pixel_formats;

  // Flag indicating that the display is ready to be published to clients.
  bool init_done = false;

  // A list of all images which have been sent to display driver.
  Image::DoublyLinkedList images;

  // The number of layers in the applied configuration.
  uint32_t layer_count;

  // Set when a layer change occurs on this display and cleared in vsync
  // when the new layers are all active.
  bool pending_layer_change;
  // If a configuration applied by Controller has layer change to occur on the
  // display (i.e. |pending_layer_change| is true), this stores the Controller's
  // config stamp for that configuration; otherwise it stores an invalid stamp.
  display::DriverConfigStamp pending_layer_change_driver_config_stamp;

  // Flag indicating that a new configuration was delayed during a layer change
  // and should be reapplied after the layer change completes.
  bool delayed_apply;

  // True when we're in the process of switching between display clients.
  bool switching_client = false;

  // |config_image_queue| stores image IDs for each display configurations
  // applied in chronological order.
  // This is used by OnVsync() display events where clients receive image
  // IDs of the latest applied configuration on each Vsync.
  //
  // A |ClientConfigImages| entry is added to the queue once the config is
  // applied, and will be evicted when the config (or a newer config) is
  // already presented on the display at Vsync time.
  //
  // TODO(https://fxbug.dev/42152065): Remove once we remove image IDs in OnVsync() events.
  struct ConfigImages {
    const display::DriverConfigStamp config_stamp;

    struct ImageMetadata {
      display::ImageId image_id;
      ClientId client_id;
    };
    std::vector<ImageMetadata> images;
  };

  std::queue<ConfigImages> config_image_queue;

 private:
  DisplayInfo();
  inspect::Node node;
  inspect::ValueList properties;
};

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DISPLAY_INFO_H_
