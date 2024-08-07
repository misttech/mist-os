// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/util.h"

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/hlcpp_conversion.h>
#include <lib/fidl/cpp/hlcpp_conversion.h>
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
    fidl::UnownedClientEnd<fuchsia_hardware_display::Coordinator> display_coordinator,
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> token,
    const fuchsia_hardware_display_types::ImageBufferUsage& image_buffer_usage) {
  const fuchsia_hardware_display::BufferCollectionId display_buffer_collection_id =
      allocation::ToDisplayBufferCollectionId(buffer_collection_id);

  fidl::Result import_buffer_collection_result =
      fidl::Call(display_coordinator)
          ->ImportBufferCollection({{
              .buffer_collection_id = display_buffer_collection_id,
              .buffer_collection_token =
                  fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken>(token.TakeChannel()),
          }});
  if (import_buffer_collection_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to call FIDL ImportBufferCollection: "
                   << import_buffer_collection_result.error_value().FormatDescription();
    return false;
  }

  fidl::Result set_buffer_collection_constraints_result =
      fidl::Call(display_coordinator)
          ->SetBufferCollectionConstraints({{
              .buffer_collection_id = display_buffer_collection_id,
              .buffer_usage = image_buffer_usage,
          }});
  auto release_buffer_collection_on_failure = fit::defer([&] {
    fit::result<fidl::OneWayStatus> release_result =
        fidl::Call(display_coordinator)->ReleaseBufferCollection(display_buffer_collection_id);
    if (release_result.is_error()) {
      FX_LOGS(ERROR) << "ReleaseBufferCollection failed: "
                     << release_result.error_value().FormatDescription();
    }
  });

  if (set_buffer_collection_constraints_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to call FIDL SetBufferCollectionConstraints: "
                   << set_buffer_collection_constraints_result.error_value().FormatDescription();
    return false;
  }

  release_buffer_collection_on_failure.cancel();
  return true;
}

DisplayEventId ImportEvent(
    fidl::UnownedClientEnd<fuchsia_hardware_display::Coordinator> display_coordinator,
    const zx::event& event) {
  static uint64_t id_generator = fuchsia::hardware::display::types::INVALID_DISP_ID + 1;

  zx::event dup;
  if (event.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup) != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to duplicate display controller event.";
    return {.value = fuchsia::hardware::display::types::INVALID_DISP_ID};
  }

  // Generate a new display ID after we've determined the event can be duplicated as to not
  // waste an id.
  DisplayEventId event_id = {.value = id_generator++};

  auto before = zx::clock::get_monotonic();
  fit::result<fidl::OneWayStatus> import_result =
      fidl::Call(display_coordinator)
          ->ImportEvent({{.event = std::move(dup), .id = fidl::HLCPPToNatural(event_id)}});
  if (import_result.is_error()) {
    auto after = zx::clock::get_monotonic();
    FX_LOGS(ERROR) << "Failed to import display controller event. Waited "
                   << (after - before).to_msecs()
                   << "msecs. Error: " << import_result.error_value().FormatDescription();
    return fidl::NaturalToHLCPP(
        fuchsia_hardware_display::EventId(fuchsia_hardware_display_types::kInvalidDispId));
  }
  return event_id;
}

bool IsCaptureSupported(
    fidl::UnownedClientEnd<fuchsia_hardware_display::Coordinator> display_coordinator) {
  fidl::Result result = fidl::Call(display_coordinator)->IsCaptureSupported();

  if (result.is_error()) {
    FX_LOGS(ERROR) << "FIDL IsCaptureSupported call failed: " << result.error_value();
    return false;
  }
  return result.value().supported();
}

zx_status_t ImportImageForCapture(
    fidl::UnownedClientEnd<fuchsia_hardware_display::Coordinator> display_coordinator,
    const fuchsia_hardware_display_types::ImageMetadata& image_metadata,
    allocation::GlobalBufferCollectionId buffer_collection_id, uint32_t vmo_idx,
    allocation::GlobalImageId image_id) {
  if (buffer_collection_id == 0) {
    FX_LOGS(ERROR) << "Buffer collection id is 0.";
    return 0;
  }

  if (image_metadata.tiling_type() != fuchsia_hardware_display_types::kImageTilingTypeCapture) {
    FX_LOGS(ERROR) << "Image config tiling type must be IMAGE_TILING_TYPE_CAPTURE.";
    return 0;
  }

  const fuchsia_hardware_display::BufferCollectionId display_buffer_collection_id =
      allocation::ToDisplayBufferCollectionId(buffer_collection_id);
  const fuchsia_hardware_display::ImageId fidl_image_id = allocation::ToFidlImageId(image_id);

  fidl::Result import_image_result =
      fidl::Call(display_coordinator)
          ->ImportImage({{.image_metadata = image_metadata,
                          .buffer_id = {{
                              .buffer_collection_id = display_buffer_collection_id,
                              .buffer_index = vmo_idx,
                          }},
                          .image_id = fidl_image_id}});

  if (import_image_result.is_error()) {
    const auto& error_value = import_image_result.error_value();
    FX_LOGS(ERROR) << "FIDL ImportImage error: " << error_value.FormatDescription();
    return (error_value.is_framework_error()) ? error_value.framework_error().status()
                                              : error_value.domain_error();
  }
  return ZX_OK;
}

}  // namespace scenic_impl
