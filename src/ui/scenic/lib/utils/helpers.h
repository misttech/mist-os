// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_UTILS_HELPERS_H_
#define SRC_UI_SCENIC_LIB_UTILS_HELPERS_H_

#include <fidl/fuchsia.ui.composition/cpp/fidl.h>
#include <fidl/fuchsia.ui.views/cpp/fidl.h>
#include <fuchsia/sysmem/cpp/fidl.h>
#include <fuchsia/ui/composition/cpp/fidl.h>
#include <fuchsia/ui/views/cpp/fidl.h>
#include <lib/zx/event.h>

#include "fuchsia/images2/cpp/fidl.h"

namespace utils {

using SysmemTokens = struct {
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr local_token;
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr dup_token;
};

constexpr std::array<float, 2> kDefaultPixelScale = {1.f, 1.f};

// Helper for extracting the koid from a ViewRef.
zx_koid_t ExtractKoid(const fuchsia::ui::views::ViewRef& view_ref);
zx_koid_t ExtractKoid(const fuchsia_ui_views::ViewRef& view_ref);

// Create an unsignalled zx::event.
zx::event CreateEvent();

// Create a std::vector populated with |n| unsignalled zx::event elements.
std::vector<zx::event> CreateEventArray(size_t n);

// Create a std::vector populated with koids of the input vector of zx:event.
std::vector<zx_koid_t> ExtractKoids(const std::vector<zx::event>& events);

// Copy a zx::event.
zx::event CopyEvent(const zx::event& event);

// Copy a zx::eventpair.
zx::eventpair CopyEventpair(const zx::eventpair& eventpair);

// Copy a std::vector of events.
std::vector<zx::event> CopyEventArray(const std::vector<zx::event>& events);

// Synchronously checks whether the event has signalled any of the bits in |signal|.
bool IsEventSignalled(const zx::event& event, zx_signals_t signal);

// Create sysmem allocator.
fuchsia::sysmem2::AllocatorSyncPtr CreateSysmemAllocatorSyncPtr(
    const std::string& debug_name_suffix = std::string());

// Create local and dup tokens for sysmem.
SysmemTokens CreateSysmemTokens(fuchsia::sysmem2::Allocator_Sync* sysmem_allocator);

// Creates default constraints for |buffer_collection|
fuchsia::sysmem2::BufferCollectionConstraints CreateDefaultConstraints(
    uint32_t buffer_count, uint32_t kWidth, uint32_t kHeight,
    fuchsia::images2::PixelFormat format = fuchsia::images2::PixelFormat::B8G8R8A8);

void PrettyPrintMat3(std::string, const std::array<float, 9>& mat3);

template <std::size_t Dim>
std::string GetArrayString(const std::string& name, const std::array<float, Dim>& array) {
  std::string result = name + ": [";
  for (uint32_t i = 0; i < array.size(); i++) {
    result += std::to_string(array[i]);
    if (i < array.size() - 1) {
      result += ", ";
    }
  }
  result += "]\n";
  return result;
}

float GetOrientationAngle(fuchsia::ui::composition::Orientation orientation);
float GetOrientationAngle(fuchsia_ui_composition::Orientation orientation);

uint32_t GetBytesPerPixel(const fuchsia::sysmem2::SingleBufferSettings& settings);
uint32_t GetBytesPerPixel(const fuchsia::sysmem::SingleBufferSettings& settings);

uint32_t GetBytesPerPixel(const fuchsia::sysmem2::ImageFormatConstraints& image_format_constraints);
uint32_t GetBytesPerPixel(const fuchsia::sysmem::ImageFormatConstraints& image_format_constraints);

uint32_t GetBytesPerRow(const fuchsia::sysmem2::SingleBufferSettings& settings,
                        uint32_t image_width);
uint32_t GetBytesPerRow(const fuchsia::sysmem::SingleBufferSettings& settings,
                        uint32_t image_width);

uint32_t GetBytesPerRow(const fuchsia::sysmem2::ImageFormatConstraints& image_format_constraints,
                        uint32_t image_width);
uint32_t GetBytesPerRow(const fuchsia::sysmem::ImageFormatConstraints& image_format_constraints,
                        uint32_t image_width);

uint32_t GetPixelsPerRow(const fuchsia::sysmem2::SingleBufferSettings& settings,
                         uint32_t image_width);
uint32_t GetPixelsPerRow(const fuchsia::sysmem::SingleBufferSettings& settings,
                         uint32_t image_width);

uint32_t GetPixelsPerRow(const fuchsia::sysmem2::ImageFormatConstraints& image_format_constraints,
                         uint32_t image_width);
uint32_t GetPixelsPerRow(const fuchsia::sysmem::ImageFormatConstraints& image_format_constraints,
                         uint32_t image_width);

}  // namespace utils

#endif  // SRC_UI_SCENIC_LIB_UTILS_HELPERS_H_
