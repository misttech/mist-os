// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVICES_LIB_MMIO_TEST_HELPER_H_
#define SRC_DEVICES_LIB_MMIO_TEST_HELPER_H_

#include <lib/mmio/mmio-buffer.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

namespace fdf_testing {
// This provides a helper to quickly create an MmioBuffer for use in tests.
// TODO(fxb/115988): This use of mmio_buffer_t is temporary to ease the
// transition of clients over to MmioBuffer, and can be switched over once we
// have no unmigrated users touching MmioBufferOps.
[[maybe_unused]] static fdf::MmioBuffer CreateMmioBuffer(
    size_t size, uint32_t cache_policy = ZX_CACHE_POLICY_UNCACHED,
    const ::fdf::internal::MmioBufferOps* ops = &::fdf::internal::kDefaultOps,
    void* ctx = nullptr) {
  zx::vmo vmo;
  zx_status_t status = zx::vmo::create(/*size=*/size, 0, &vmo);
  ZX_ASSERT(status == ZX_OK);
  mmio_buffer_t mmio{};
  status = mmio_buffer_init(&mmio, 0, size, vmo.release(), cache_policy);
  ZX_ASSERT(status == ZX_OK);
  return fdf::MmioBuffer(mmio, ops, ctx);
}
}  // namespace fdf_testing

#endif  // SRC_DEVICES_LIB_MMIO_TEST_HELPER_H_
