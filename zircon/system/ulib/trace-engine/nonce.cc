// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/trace-engine/instrumentation.h>
#include <lib/zx/clock.h>

#include <atomic>

namespace {

std::atomic_uint64_t g_nonce{1u};

}  // namespace

__EXPORT uint64_t trace_generate_nonce() {
  return g_nonce.fetch_add(1u, std::memory_order_relaxed);
}

#if FUCHSIA_API_LEVEL_AT_LEAST(NEXT)
__EXPORT uint64_t trace_time_based_id(zx_koid_t thread_id) {
  uint64_t time = zx::clock::get_boot().get();
  uint64_t ts = time << 16;
  uint64_t tid = (thread_id & ((1ULL << 9) - 1)) << 8;          // only keep u8
  uint64_t nonce = trace_generate_nonce() & ((1ULL << 9) - 1);  // only keep u8
  return ts | tid | nonce;
}
#endif
