// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/util.h"

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <lib/fidl/cpp/wire/status.h>
#include <lib/fit/defer.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <lib/zx/event.h>
#include <zircon/status.h>

#include "src/ui/scenic/lib/allocation/id.h"

namespace scenic_impl {

bool ImportBufferCollection(
    allocation::GlobalBufferCollectionId buffer_collection_id,
    const fidl::WireSharedClient<fuchsia_hardware_display::Coordinator>& display_coordinator,
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> token,
    const fuchsia_hardware_display_types::wire::ImageBufferUsage& image_buffer_usage) {
  const fuchsia_hardware_display::wire::BufferCollectionId display_buffer_collection_id =
      scenic_impl::ToDisplayFidlBufferCollectionId(buffer_collection_id);

  auto import_buffer_collection_result = display_coordinator.sync()->ImportBufferCollection(
      display_buffer_collection_id, std::move(token));
  if (import_buffer_collection_result->is_error()) {
    FX_LOGS(ERROR) << "Failed to call FIDL ImportBufferCollection: "
                   << zx_status_get_string(import_buffer_collection_result->error_value());
    return false;
  }

  auto set_buffer_collection_constraints_result =
      display_coordinator.sync()->SetBufferCollectionConstraints(display_buffer_collection_id,
                                                                 image_buffer_usage);
  auto release_buffer_collection_on_failure = fit::defer([&] {
    fidl::OneWayStatus release_result =
        display_coordinator.sync()->ReleaseBufferCollection(display_buffer_collection_id);
    if (!release_result.ok()) {
      FX_LOGS(ERROR) << "ReleaseBufferCollection failed: " << release_result.status_string();
    }
  });

  if (set_buffer_collection_constraints_result->is_error()) {
    FX_LOGS(ERROR) << "Failed to call FIDL SetBufferCollectionConstraints: "
                   << zx_status_get_string(set_buffer_collection_constraints_result->error_value());
    return false;
  }

  release_buffer_collection_on_failure.cancel();
  return true;
}

DisplayEventId ImportEvent(
    const fidl::WireSharedClient<fuchsia_hardware_display::Coordinator>& display_coordinator,
    const zx::event& event) {
  static uint64_t id_generator = fuchsia_hardware_display_types::kInvalidDispId + 1;

  zx::event dup;
  if (event.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup) != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to duplicate display controller event.";
    return {.value = fuchsia_hardware_display_types::kInvalidDispId};
  }

  // Generate a new display ID after we've determined the event can be duplicated as to not
  // waste an id.
  DisplayEventId event_id = {.value = id_generator++};

  auto before = zx::clock::get_monotonic();
  fidl::OneWayStatus import_result =
      display_coordinator.sync()->ImportEvent(std::move(dup), event_id);
  if (!import_result.ok()) {
    auto after = zx::clock::get_monotonic();
    FX_LOGS(ERROR) << "Failed to import display controller event. Waited "
                   << (after - before).to_msecs()
                   << "msecs. Error code: " << import_result.status_string();
    return {.value = fuchsia_hardware_display_types::kInvalidDispId};
  }
  return event_id;
}

bool IsCaptureSupported(
    const fidl::WireSharedClient<fuchsia_hardware_display::Coordinator>& display_coordinator) {
  auto result = display_coordinator.sync()->IsCaptureSupported();

  if (result->is_error()) {
    FX_LOGS(ERROR) << "FIDL IsCaptureSupported call failed: "
                   << zx_status_get_string(result->error_value());
    return false;
  }
  return (*result)->supported;
}

zx_status_t ImportImageForCapture(
    const fidl::WireSharedClient<fuchsia_hardware_display::Coordinator>& display_coordinator,
    const fuchsia_hardware_display_types::wire::ImageMetadata& image_metadata,
    allocation::GlobalBufferCollectionId buffer_collection_id, uint32_t vmo_idx,
    allocation::GlobalImageId image_id) {
  if (buffer_collection_id == 0) {
    FX_LOGS(ERROR) << "Buffer collection id is 0.";
    return 0;
  }

  if (image_metadata.tiling_type != fuchsia_hardware_display_types::kImageTilingTypeCapture) {
    FX_LOGS(ERROR) << "Image config tiling type must be IMAGE_TILING_TYPE_CAPTURE.";
    return 0;
  }

  const fuchsia_hardware_display::wire::BufferCollectionId display_buffer_collection_id =
      scenic_impl::ToDisplayFidlBufferCollectionId(buffer_collection_id);
  const fuchsia_hardware_display::wire::ImageId fidl_image_id =
      scenic_impl::ToDisplayFidlImageId(image_id);

  auto import_image_result = display_coordinator.sync()->ImportImage(
      image_metadata,
      {
          .buffer_collection_id = display_buffer_collection_id,
          .buffer_index = vmo_idx,
      },
      fidl_image_id);

  if (import_image_result->is_error()) {
    FX_LOGS(ERROR) << "FIDL ImportImage error: "
                   << zx_status_get_string(import_image_result->error_value());
    return import_image_result->error_value();
  }
  return ZX_OK;
}

}  // namespace scenic_impl
