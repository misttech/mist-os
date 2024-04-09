// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/image-info.h"

#include <fidl/fuchsia.hardware.amlogiccanvas/cpp/wire.h>

#include "src/graphics/display/lib/driver-framework-migration-utils/logging/zxlogf.h"

namespace amlogic_display {

ImageInfo::~ImageInfo() {
  zxlogf(INFO, "Destroying image on canvas %d", canvas_idx);
  if (canvas.has_value()) {
    fidl::WireResult result = fidl::WireCall(canvas.value())->Free(canvas_idx);
    if (!result.ok()) {
      zxlogf(WARNING, "Failed to call Canvas Free: %s", result.error().FormatDescription().c_str());
    } else if (result->is_error()) {
      zxlogf(WARNING, "Canvas Free failed: %s", zx_status_get_string(result->error_value()));
    }
  }
  if (pmt) {
    pmt.unpin();
  }
}

}  // namespace amlogic_display
