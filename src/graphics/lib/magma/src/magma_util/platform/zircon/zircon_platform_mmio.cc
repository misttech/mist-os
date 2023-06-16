// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon_platform_mmio.h"

#include "magma_util/dlog.h"
#include "magma_util/short_macros.h"

namespace magma {

static_assert(ZX_CACHE_POLICY_CACHED == static_cast<int>(PlatformMmio::CACHE_POLICY_CACHED),
              "enum mismatch");
static_assert(ZX_CACHE_POLICY_UNCACHED == static_cast<int>(PlatformMmio::CACHE_POLICY_UNCACHED),
              "enum mismatch");
static_assert(ZX_CACHE_POLICY_UNCACHED_DEVICE ==
                  static_cast<int>(PlatformMmio::CACHE_POLICY_UNCACHED_DEVICE),
              "enum mismatch");
static_assert(ZX_CACHE_POLICY_WRITE_COMBINING ==
                  static_cast<int>(PlatformMmio::CACHE_POLICY_WRITE_COMBINING),
              "enum mismatch");

ZirconPlatformMmio::ZirconPlatformMmio(fdf::MmioBuffer mmio)
    // TODO(fxbug.dev/56253): Add MMIO_PTR to cast.
    : PlatformMmio((void*)mmio.get(), mmio.get_size()), mmio_(std::move(mmio)) {}

bool ZirconPlatformMmio::Pin(const zx::bti& bti) {
  zx_status_t status = mmio_.Pin(bti, &pinned_mmio_);
  if (status != ZX_OK) {
    return DRETF(false, "Failed to pin mmio: %d\n", status);
  }
  return true;
}

uint64_t ZirconPlatformMmio::physical_address() {
  MAGMA_DASSERT(pinned_mmio_);
  return pinned_mmio_->get_paddr();
}

ZirconPlatformMmio::~ZirconPlatformMmio() { DLOG("ZirconPlatformMmio dtor"); }

}  // namespace magma
