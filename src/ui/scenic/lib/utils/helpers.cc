// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/utils/helpers.h"

#include <fidl/fuchsia.images2/cpp/fidl.h>
#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem/cpp/hlcpp_conversion.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/hlcpp_conversion.h>
#include <lib/fdio/directory.h>
#include <lib/image-format/image_format.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/trace/event.h>

#include <fbl/algorithm.h>

#include "src/lib/fsl/handles/object_info.h"

#include <glm/gtc/constants.hpp>

using fuchsia::ui::composition::Orientation;

namespace utils {

fuchsia::ui::scenic::Present2Args CreatePresent2Args(zx_time_t requested_presentation_time,
                                                     std::vector<zx::event> acquire_fences,
                                                     std::vector<zx::event> release_fences,
                                                     zx_duration_t requested_prediction_span) {
  fuchsia::ui::scenic::Present2Args args;
  args.set_requested_presentation_time(requested_presentation_time);
  args.set_acquire_fences(std::move(acquire_fences));
  args.set_release_fences(std::move(release_fences));
  args.set_requested_prediction_span(requested_prediction_span);

  return args;
}

zx_koid_t ExtractKoid(const fuchsia::ui::views::ViewRef& view_ref) {
  return fsl::GetKoid(view_ref.reference.get());
}

template <typename ZX_T>
static auto CopyZxHandle(const ZX_T& handle) -> ZX_T {
  ZX_T handle_copy;
  if (handle.duplicate(ZX_RIGHT_SAME_RIGHTS, &handle_copy) != ZX_OK) {
    FX_LOGS(ERROR) << "Copying zx object handle failed.";
    FX_DCHECK(false);
  }
  return handle_copy;
}

zx::event CopyEvent(const zx::event& event) { return CopyZxHandle(event); }

zx::eventpair CopyEventpair(const zx::eventpair& eventpair) { return CopyZxHandle(eventpair); }

std::vector<zx::event> CopyEventArray(const std::vector<zx::event>& events) {
  std::vector<zx::event> result;
  const size_t count = events.size();
  result.reserve(count);
  for (size_t i = 0; i < count; i++) {
    result.push_back(CopyEvent(events[i]));
  }
  return result;
}

bool IsEventSignalled(const zx::event& event, zx_signals_t signal) {
  zx_signals_t pending = 0u;
  event.wait_one(signal, zx::time(), &pending);
  return (pending & signal) != 0u;
}

zx::event CreateEvent() {
  TRACE_DURATION("gfx", "CreateEvent");
  zx::event event;
  FX_CHECK(zx::event::create(0, &event) == ZX_OK);
  return event;
}

std::vector<zx::event> CreateEventArray(size_t n) {
  std::vector<zx::event> events;
  events.reserve(n);
  for (size_t i = 0; i < n; i++) {
    events.push_back(CreateEvent());
  }
  return events;
}

std::vector<zx_koid_t> ExtractKoids(const std::vector<zx::event>& events) {
  std::vector<zx_koid_t> result;
  result.reserve(events.size());
  for (auto& evt : events) {
    zx_info_handle_basic_t info;
    zx_status_t status = evt.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
    FX_DCHECK(status == ZX_OK);
    result.push_back(info.koid);
  }
  return result;
}

fuchsia::sysmem2::AllocatorSyncPtr CreateSysmemAllocatorSyncPtr(
    const std::string& debug_name_suffix) {
  FX_CHECK(!debug_name_suffix.empty());
  fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator;
  zx_status_t status = fdio_service_connect("/svc/fuchsia.sysmem2.Allocator",
                                            sysmem_allocator.NewRequest().TakeChannel().release());
  FX_DCHECK(status == ZX_OK);
  auto debug_name = fsl::GetCurrentProcessName() + " " + debug_name_suffix;
  constexpr size_t kMaxNameLength = 64;  // from fuchsia.sysmem/allocator.fidl
  FX_DCHECK(debug_name.length() <= kMaxNameLength)
      << "Sysmem client debug name exceeded max length of " << kMaxNameLength << " (\""
      << debug_name << "\")";

  fuchsia::sysmem2::AllocatorSetDebugClientInfoRequest set_debug_request;
  set_debug_request.set_name(std::move(debug_name));
  set_debug_request.set_id(fsl::GetCurrentProcessKoid());
  sysmem_allocator->SetDebugClientInfo(std::move(set_debug_request));

  return sysmem_allocator;
}

SysmemTokens CreateSysmemTokens(fuchsia::sysmem2::Allocator_Sync* sysmem_allocator) {
  FX_DCHECK(sysmem_allocator);
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr local_token;
  fuchsia::sysmem2::AllocatorAllocateSharedCollectionRequest allocate_request;
  allocate_request.set_token_request(local_token.NewRequest());
  zx_status_t status = sysmem_allocator->AllocateSharedCollection(std::move(allocate_request));
  FX_DCHECK(status == ZX_OK);
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr dup_token;
  fuchsia::sysmem2::BufferCollectionTokenDuplicateRequest dup_request;
  dup_request.set_rights_attenuation_mask(ZX_RIGHT_SAME_RIGHTS);
  dup_request.set_token_request(dup_token.NewRequest());
  status = local_token->Duplicate(std::move(dup_request));
  FX_DCHECK(status == ZX_OK);
  fuchsia::sysmem2::Node_Sync_Result sync_result;
  status = local_token->Sync(&sync_result);
  FX_DCHECK(status == ZX_OK);
  FX_DCHECK(sync_result.is_response());

  return {std::move(local_token), std::move(dup_token)};
}

fuchsia::sysmem2::BufferCollectionConstraints CreateDefaultConstraints(uint32_t buffer_count,
                                                                       uint32_t width,
                                                                       uint32_t height) {
  fuchsia::sysmem2::BufferCollectionConstraints constraints;
  constraints.mutable_buffer_memory_constraints()->set_cpu_domain_supported(true);
  constraints.mutable_buffer_memory_constraints()->set_ram_domain_supported(true);
  constraints.mutable_usage()->set_cpu(fuchsia::sysmem2::CPU_USAGE_READ_OFTEN |
                                       fuchsia::sysmem2::CPU_USAGE_WRITE_OFTEN);
  constraints.set_min_buffer_count(buffer_count);

  auto& image_constraints = constraints.mutable_image_format_constraints()->emplace_back();
  image_constraints.mutable_color_spaces()->push_back(fuchsia::images2::ColorSpace::SRGB);
  image_constraints.set_pixel_format(fuchsia::images2::PixelFormat::B8G8R8A8);
  image_constraints.set_pixel_format_modifier(fuchsia::images2::PixelFormatModifier::LINEAR);

  image_constraints.set_required_min_size({.width = width, .height = height});
  image_constraints.set_required_max_size({.width = width, .height = height});
  image_constraints.set_bytes_per_row_divisor(4);
  return constraints;
}

bool RectFContainsPoint(const fuchsia::math::RectF& rect, float x, float y) {
  constexpr float kEpsilon = 1e-3f;
  return rect.x - kEpsilon <= x && x <= rect.x + rect.width + kEpsilon && rect.y - kEpsilon <= y &&
         y <= rect.y + rect.height + kEpsilon;
}

fuchsia::math::RectF ConvertRectToRectF(const fuchsia::math::Rect& rect) {
  return {.x = static_cast<float>(rect.x),
          .y = static_cast<float>(rect.y),
          .width = static_cast<float>(rect.width),
          .height = static_cast<float>(rect.height)};
}

// Prints in row-major order.
void PrettyPrintMat3(std::string name, const std::array<float, 9>& mat3) {
  FX_LOGS(INFO) << "\n"
                << name << ":\n"
                << mat3[0] << "," << mat3[3] << "," << mat3[6] << "\n"
                << mat3[1] << "," << mat3[4] << "," << mat3[7] << "\n"
                << mat3[2] << "," << mat3[5] << "," << mat3[8];
}

float GetOrientationAngle(fuchsia::ui::composition::Orientation orientation) {
  switch (orientation) {
    case Orientation::CCW_0_DEGREES:
      return 0.f;
    case Orientation::CCW_90_DEGREES:
      return -glm::half_pi<float>();
    case Orientation::CCW_180_DEGREES:
      return -glm::pi<float>();
    case Orientation::CCW_270_DEGREES:
      return -glm::three_over_two_pi<float>();
  }
}

namespace {

uint32_t GetBytesPerRow(const fuchsia::sysmem2::ImageFormatConstraints& image_format_constraints,
                        uint32_t image_width, uint32_t bytes_per_pixel) {
  uint32_t bytes_per_row_divisor = image_format_constraints.bytes_per_row_divisor();
  uint32_t min_bytes_per_row = image_format_constraints.min_bytes_per_row();
  uint32_t bytes_per_row = fbl::round_up(std::max(image_width * bytes_per_pixel, min_bytes_per_row),
                                         bytes_per_row_divisor);
  return bytes_per_row;
}
uint32_t GetBytesPerRow(const fuchsia::sysmem::ImageFormatConstraints& image_format_constraints,
                        uint32_t image_width, uint32_t bytes_per_pixel) {
  uint32_t bytes_per_row_divisor = image_format_constraints.bytes_per_row_divisor;
  uint32_t min_bytes_per_row = image_format_constraints.min_bytes_per_row;
  uint32_t bytes_per_row = fbl::round_up(std::max(image_width * bytes_per_pixel, min_bytes_per_row),
                                         bytes_per_row_divisor);
  return bytes_per_row;
}

}  // namespace

uint32_t GetBytesPerPixel(const fuchsia::sysmem2::SingleBufferSettings& settings) {
  return GetBytesPerPixel(settings.image_format_constraints());
}
uint32_t GetBytesPerPixel(const fuchsia::sysmem::SingleBufferSettings& settings) {
  return GetBytesPerPixel(settings.image_format_constraints);
}

uint32_t GetBytesPerPixel(
    const fuchsia::sysmem2::ImageFormatConstraints& image_format_constraints) {
  fuchsia::images2::PixelFormat pixel_format = image_format_constraints.pixel_format();
  fuchsia::images2::PixelFormatModifier pixel_format_modifier;
  if (image_format_constraints.has_pixel_format_modifier()) {
    pixel_format_modifier = image_format_constraints.pixel_format_modifier();
  } else {
    pixel_format_modifier = fuchsia::images2::PixelFormatModifier::LINEAR;
  }
  PixelFormatAndModifier pixel_format_and_modifier(fidl::HLCPPToNatural(pixel_format),
                                                   fidl::HLCPPToNatural(pixel_format_modifier));
  return ImageFormatStrideBytesPerWidthPixel(pixel_format_and_modifier);
}
uint32_t GetBytesPerPixel(const fuchsia::sysmem::ImageFormatConstraints& image_format_constraints) {
  auto hlcpp_pixel_format = image_format_constraints.pixel_format;
  fidl::Arena arena;
  auto wire_pixel_format = fidl::ToWire(arena, fidl::HLCPPToNatural(hlcpp_pixel_format));
  return ImageFormatStrideBytesPerWidthPixel(wire_pixel_format);
}

uint32_t GetBytesPerRow(const fuchsia::sysmem2::SingleBufferSettings& settings,
                        uint32_t image_width) {
  return GetBytesPerRow(settings.image_format_constraints(), image_width);
}
uint32_t GetBytesPerRow(const fuchsia::sysmem::SingleBufferSettings& settings,
                        uint32_t image_width) {
  return GetBytesPerRow(settings.image_format_constraints, image_width);
}

uint32_t GetBytesPerRow(const fuchsia::sysmem2::ImageFormatConstraints& image_format_constraints,
                        uint32_t image_width) {
  uint32_t bytes_per_pixel = GetBytesPerPixel(image_format_constraints);
  return GetBytesPerRow(image_format_constraints, image_width, bytes_per_pixel);
}
uint32_t GetBytesPerRow(const fuchsia::sysmem::ImageFormatConstraints& image_format_constraints,
                        uint32_t image_width) {
  uint32_t bytes_per_pixel = GetBytesPerPixel(image_format_constraints);
  return GetBytesPerRow(image_format_constraints, image_width, bytes_per_pixel);
}

uint32_t GetPixelsPerRow(const fuchsia::sysmem2::SingleBufferSettings& settings,
                         uint32_t image_width) {
  return GetPixelsPerRow(settings.image_format_constraints(), image_width);
}

uint32_t GetPixelsPerRow(const fuchsia::sysmem::SingleBufferSettings& settings,
                         uint32_t image_width) {
  return GetPixelsPerRow(settings.image_format_constraints, image_width);
}

uint32_t GetPixelsPerRow(const fuchsia::sysmem2::ImageFormatConstraints& image_format_constraints,
                         uint32_t image_width) {
  uint32_t bytes_per_pixel = GetBytesPerPixel(image_format_constraints);
  return GetBytesPerRow(image_format_constraints, image_width, bytes_per_pixel) / bytes_per_pixel;
}
uint32_t GetPixelsPerRow(const fuchsia::sysmem::ImageFormatConstraints& image_format_constraints,
                         uint32_t image_width) {
  uint32_t bytes_per_pixel = GetBytesPerPixel(image_format_constraints);
  return GetBytesPerRow(image_format_constraints, image_width, bytes_per_pixel) / bytes_per_pixel;
}

}  // namespace utils
