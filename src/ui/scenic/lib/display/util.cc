// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/util.h"

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <lib/zx/event.h>
#include <zircon/status.h>

#include "src/ui/scenic/lib/allocation/id.h"

namespace scenic_impl {

bool ImportBufferCollection(
    allocation::GlobalBufferCollectionId buffer_collection_id,
    const fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator,
    fuchsia::sysmem::BufferCollectionTokenSyncPtr token,
    const fuchsia::hardware::display::ImageConfig& image_config) {
  zx_status_t status;

  const fuchsia::hardware::display::BufferCollectionId display_buffer_collection_id =
      allocation::ToDisplayBufferCollectionId(buffer_collection_id);
  if (display_coordinator->ImportBufferCollection(display_buffer_collection_id, std::move(token),
                                                  &status) != ZX_OK ||
      status != ZX_OK) {
    FX_LOGS(ERROR) << "ImportBufferCollection failed - status: " << status;
    return false;
  }

  if (display_coordinator->SetBufferCollectionConstraints(display_buffer_collection_id,
                                                          image_config, &status) != ZX_OK ||
      status != ZX_OK) {
    FX_LOGS(ERROR) << "SetBufferCollectionConstraints failed.";

    if (display_coordinator->ReleaseBufferCollection(display_buffer_collection_id) != ZX_OK) {
      FX_LOGS(ERROR) << "ReleaseBufferCollection failed.";
    }
    return false;
  }

  return true;
}

DisplayEventId ImportEvent(
    const fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator,
    const zx::event& event) {
  static uint64_t id_generator = fuchsia::hardware::display::INVALID_DISP_ID + 1;

  zx::event dup;
  if (event.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup) != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to duplicate display controller event.";
    return {.value = fuchsia::hardware::display::INVALID_DISP_ID};
  }

  // Generate a new display ID after we've determined the event can be duplicated as to not
  // waste an id.
  DisplayEventId event_id = {.value = id_generator++};

  auto before = zx::clock::get_monotonic();
  auto status = display_coordinator->ImportEvent(std::move(dup), event_id);
  if (status != ZX_OK) {
    auto after = zx::clock::get_monotonic();
    FX_LOGS(ERROR) << "Failed to import display controller event. Waited "
                   << (after - before).to_msecs() << "msecs. Error code: " << status;
    return {.value = fuchsia::hardware::display::INVALID_DISP_ID};
  }
  return event_id;
}

bool IsCaptureSupported(const fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator) {
  fuchsia::hardware::display::Coordinator_IsCaptureSupported_Result capture_supported_result;
  auto status = display_coordinator->IsCaptureSupported(&capture_supported_result);

  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "IsCaptureSupported status failure: " << status;
    return false;
  }

  if (!capture_supported_result.is_response()) {
    FX_LOGS(ERROR) << "IsCaptureSupported did not return a valid response.";
    return false;
  }

  return capture_supported_result.response().supported;
}

zx_status_t ImportImageForCapture(
    const fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator,
    const fuchsia::hardware::display::ImageConfig& image_config,
    allocation::GlobalBufferCollectionId buffer_collection_id, uint32_t vmo_idx,
    allocation::GlobalImageId image_id) {
  if (buffer_collection_id == 0) {
    FX_LOGS(ERROR) << "Buffer collection id is 0.";
    return 0;
  }

  if (image_config.type != fuchsia::hardware::display::TYPE_CAPTURE) {
    FX_LOGS(ERROR) << "Image config type must be TYPE_CAPTURE.";
    return 0;
  }

  const fuchsia::hardware::display::BufferCollectionId display_buffer_collection_id =
      allocation::ToDisplayBufferCollectionId(buffer_collection_id);
  zx_status_t import_result;
  auto status = display_coordinator->ImportImage(image_config, display_buffer_collection_id,
                                                 image_id, vmo_idx, &import_result);

  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "FIDL transport error, status: " << status;
    return status;
  }
  if (import_result != ZX_OK) {
    FX_LOGS(ERROR) << "FIDL server error response: " << zx_status_get_string(import_result);
    return import_result;
  }
  return ZX_OK;
}

}  // namespace scenic_impl
