// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-gpu-display/imported-image.h"

#include <lib/driver/logging/cpp/logger.h>
#include <lib/zx/bti.h>
#include <lib/zx/pmt.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <cstddef>
#include <cstdint>
#include <utility>

#include "src/graphics/lib/virtio/virtio-abi.h"

namespace virtio_display {

// static
zx::result<ImportedImage> ImportedImage::Create(const zx::bti& bti, zx::vmo& image_vmo,
                                                uint64_t image_vmo_offset, size_t image_size) {
  zx::pmt pinned_memory_token;
  zx_paddr_t image_physical_address;
  zx_status_t pin_status = bti.pin(ZX_BTI_PERM_READ | ZX_BTI_CONTIGUOUS, image_vmo,
                                   image_vmo_offset, image_size, &image_physical_address,
                                   /*addrs_count=*/1, &pinned_memory_token);
  if (pin_status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to pin image VMO: %s", zx_status_get_string(pin_status));
    return zx::error(pin_status);
  }

  return zx::ok(ImportedImage(image_physical_address, std::move(pinned_memory_token),
                              virtio_abi::kInvalidResourceId));
}

// static
ImportedImage ImportedImage::CreateEmpty() { return ImportedImage(); }

ImportedImage::ImportedImage() = default;

ImportedImage::ImportedImage(zx_paddr_t physical_address, zx::pmt pinned_memory_token,
                             uint32_t virtio_resource_id)
    : physical_address_(physical_address),
      pinned_memory_token_(std::move(pinned_memory_token)),
      virtio_resource_id_(virtio_resource_id) {
  ZX_DEBUG_ASSERT(pinned_memory_token_.is_valid());
}

ImportedImage::~ImportedImage() {
  if (!pinned_memory_token_.is_valid()) {
    return;
  }
  zx_status_t unpin_status = pinned_memory_token_.unpin();
  if (unpin_status != ZX_OK) {
    FDF_LOG(WARNING, "Failed to unpin image memory: %s", zx_status_get_string(unpin_status));
  }
}

}  // namespace virtio_display
