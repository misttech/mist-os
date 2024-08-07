// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_DISPLAY_UTIL_H_
#define SRC_UI_SCENIC_LIB_DISPLAY_UTIL_H_

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <fuchsia/hardware/display/cpp/fidl.h>

#include <cstdint>

#include "src/ui/scenic/lib/allocation/id.h"

namespace scenic_impl {
using DisplayEventId = fuchsia::hardware::display::EventId;

// Imports a sysmem buffer collection token to a display controller, and sets the constraints.
// A successful import will return true, otherwise it will return false.
bool ImportBufferCollection(
    allocation::GlobalBufferCollectionId buffer_collection_id,
    fidl::UnownedClientEnd<fuchsia_hardware_display::Coordinator> display_coordinator,
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> token,
    const fuchsia_hardware_display_types::ImageBufferUsage& image_buffer_usage);

// Imports a zx::event to the provided display controller. The return value is an ID to
// reference that event on other display controller functions that take an event as an
// argument. On failure, the return value will be
// fuchsia::hardware::display::types::INVALID_DISP_ID.
DisplayEventId ImportEvent(
    fidl::UnownedClientEnd<fuchsia_hardware_display::Coordinator> display_coordinator,
    const zx::event& event);

// Returns true if the display controller backend is capable of image capture (i.e.
// screenshotting).
bool IsCaptureSupported(
    fidl::UnownedClientEnd<fuchsia_hardware_display::Coordinator> display_coordinator);

// Imports an image to be used for capture (i.e. screenshotting) of the display
// controller, associated with `capture_image_id`.
//
// `capture_image_id` must not be used for any images, including images
// imported for display.
//
// On failure, this function returns the error value.
//
// TODO(https://fxbug.dev/42080575): Unify this method with ImportBufferImage().
zx_status_t ImportImageForCapture(
    fidl::UnownedClientEnd<fuchsia_hardware_display::Coordinator> display_coordinator,
    const fuchsia_hardware_display_types::ImageMetadata& image_metadata,
    allocation::GlobalBufferCollectionId buffer_collection_id, uint32_t vmo_idx,
    allocation::GlobalImageId capture_image_id);

inline fidl::UnownedClientEnd<fuchsia_hardware_display::Coordinator> GetUnowned(
    const fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator) {
  return fidl::UnownedClientEnd<fuchsia_hardware_display::Coordinator>(
      display_coordinator.unowned_channel());
}

inline fidl::UnownedClientEnd<fuchsia_hardware_display::Coordinator> GetUnowned(
    const fidl::ClientEnd<fuchsia_hardware_display::Coordinator>& display_coordinator) {
  return fidl::UnownedClientEnd<fuchsia_hardware_display::Coordinator>(
      display_coordinator.borrow());
}

}  // namespace scenic_impl

#endif  // SRC_UI_SCENIC_LIB_DISPLAY_UTIL_H_
