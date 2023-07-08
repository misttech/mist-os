// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_DISPLAY_UTIL_H_
#define SRC_UI_SCENIC_LIB_DISPLAY_UTIL_H_

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/sysmem/cpp/fidl.h>

#include <cstdint>

#include "src/ui/scenic/lib/allocation/id.h"

namespace scenic_impl {
using DisplayEventId = fuchsia::hardware::display::EventId;

// Imports a sysmem buffer collection token to a display controller, and sets the constraints.
// A successful import will return true, otherwise it will return false.
bool ImportBufferCollection(
    allocation::GlobalBufferCollectionId identifier,
    const fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator,
    fuchsia::sysmem::BufferCollectionTokenSyncPtr token,
    const fuchsia::hardware::display::ImageConfig& image_config);

// Imports a zx::event to the provided display controller. The return value is an ID to
// reference that event on other display controller functions that take an event as an
// argument. On failure, the return value will be fuchsia::hardware::display::INVALID_DISP_ID.
DisplayEventId ImportEvent(
    const fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator,
    const zx::event& event);

// Returns true if the display controller backend is capable of image capture (i.e.
// screenshotting).
bool IsCaptureSupported(const fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator);

// Imports an image to be used for capture (i.e. screenshotting) of the display
// controller, associated with `capture_image_id`.
//
// `capture_image_id` must not be used for any images, including images
// imported for display.
//
// On failure, this function returns the error value.
//
// TODO(fxbug.dev/130268): Unify this method with ImportBufferImage().
zx_status_t ImportImageForCapture(
    const fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator,
    const fuchsia::hardware::display::ImageConfig& image_config,
    allocation::GlobalBufferCollectionId buffer_collection_id, uint32_t vmo_idx,
    allocation::GlobalImageId capture_image_id);

}  // namespace scenic_impl

#endif  // SRC_UI_SCENIC_LIB_DISPLAY_UTIL_H_
