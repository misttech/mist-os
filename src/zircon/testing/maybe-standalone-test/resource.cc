// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/maybe-standalone-test/maybe-standalone.h>
#include <lib/standalone-test/standalone.h>
#include <lib/zx/resource.h>

// Redeclare the standalone-test function as weak here.
[[gnu::weak]] decltype(standalone::GetMmioResource) standalone::GetMmioResource;
[[gnu::weak]] decltype(standalone::GetSystemResource) standalone::GetSystemResource;

namespace maybe_standalone {

zx::unowned_resource GetMmioResource() {
  zx::unowned_resource mmio_resource;
  if (standalone::GetMmioResource) {
    mmio_resource = standalone::GetMmioResource();
  }
  return mmio_resource;
}

zx::unowned_resource GetSystemResource() {
  zx::unowned_resource system_resource;
  if (standalone::GetSystemResource) {
    system_resource = standalone::GetSystemResource();
  }
  return system_resource;
}

zx::result<zx::resource> GetSystemResourceWithBase(zx::unowned_resource& system_resource,
                                                   uint64_t base) {
  zx::resource new_resource;
  const zx_status_t status = zx::resource::create(*system_resource, ZX_RSRC_KIND_SYSTEM, base, 1,
                                                  nullptr, 0, &new_resource);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(new_resource));
}

}  // namespace maybe_standalone
