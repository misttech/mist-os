// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_DSI_BANJO_CONVERSION_H_
#define SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_DSI_BANJO_CONVERSION_H_

#include <fuchsia/hardware/dsiimpl/c/banjo.h>

#include "src/graphics/display/lib/designware-dsi/dpi-interface-config.h"
#include "src/graphics/display/lib/designware-dsi/dsi-host-controller-config.h"

namespace designware_dsi {

// `color_code` must be a valid `ColorCode` enum.
DpiColorComponentMapping ToDpiColorComponentMapping(color_code_t color_code);

// `color_code` must be a valid `ColorCode` enum.
mipi_dsi::DsiPixelStreamPacketFormat ToDsiPixelStreamPacketFormat(color_code_t color_code);

// `video_mode` must be a valid `VideoCode` enum.
mipi_dsi::DsiVideoModePacketSequencing ToDsiVideoModePacketSequencing(video_mode_t video_mode);

// The `vendor_config_buffer` field in `dsi_config` must not be null and
// the buffer must store a `designware_config_t` struct.
DsiHostControllerConfig ToDsiHostControllerConfig(const dsi_config_t& dsi_config,
                                                  int64_t dphy_data_lane_bits_per_second);

}  // namespace designware_dsi

#endif  // SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_DSI_BANJO_CONVERSION_H_
