// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_LIB_MAGMA_SRC_MAGMA_UTIL_PLATFORM_ZIRCON_ZIRCON_VMO_SEMAPHORE_H_
#define SRC_GRAPHICS_LIB_MAGMA_SRC_MAGMA_UTIL_PLATFORM_ZIRCON_ZIRCON_VMO_SEMAPHORE_H_

#include <lib/zx/vmo.h>

#include <chrono>

#include "magma_util/short_macros.h"
#include "platform_semaphore.h"
#include "platform_trace.h"

namespace magma {

// VMO semaphores support timestamps.
// They aren't created by default since they're less memory efficient than the event-based
// ZirconPlatformSemaphore, but they can be imported given a VMO handle.
// Timestamp is updated on Signal and Reset but it's a bit racy.
class ZirconVmoSemaphore : public PlatformSemaphore {
 public:
  ZirconVmoSemaphore(zx::vmo vmo, uint64_t koid, uint64_t flags)
      : PlatformSemaphore(flags), vmo_(std::move(vmo)), koid_(koid) {}

  void set_local_id(uint64_t id) override {
    DASSERT(id);
    DASSERT(!local_id_);
    local_id_ = id;
  }

  uint64_t koid() const { return koid_; }

  uint64_t id() const override { return local_id_ ? local_id_ : koid_; }
  uint64_t global_id() const override { return koid_; }

  bool duplicate_handle(uint32_t* handle_out) const override;
  bool duplicate_handle(zx::handle* handle_out) const override;

  void Reset() override;
  void Signal() override;

  magma::Status WaitNoReset(uint64_t timeout_ms) override;
  magma::Status Wait(uint64_t timeout_ms) override;

  bool WaitAsync(PlatformPort* port, uint64_t key) override;

  zx_handle_t zx_handle() const { return vmo_.get(); }

  static zx_signals_t zx_signal() { return ZX_USER_SIGNAL_0; }

  zx_signals_t GetZxSignal() const override { return zx_signal(); }

  bool GetTimestamp(uint64_t* timestamp_ns_out) override;

 private:
  void WriteTimestamp(uint64_t timestamp_ns);

  zx::vmo vmo_;
  uint64_t koid_;
  uint64_t local_id_ = 0;
};

}  // namespace magma

#endif  // SRC_GRAPHICS_LIB_MAGMA_SRC_MAGMA_UTIL_PLATFORM_ZIRCON_ZIRCON_VMO_SEMAPHORE_H_
