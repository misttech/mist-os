// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_TRACE_MANAGER_DEFERRED_BUFFER_FORWARDER_H_
#define SRC_PERFORMANCE_TRACE_MANAGER_DEFERRED_BUFFER_FORWARDER_H_
#include <lib/stdcompat/span.h>
#include <lib/zx/socket.h>

#include <utility>

#include "src/performance/trace_manager/buffer_forwarder.h"
#include "src/performance/trace_manager/util.h"

namespace tracing {
// A version of the BufferForwarder which first buffers to the local filesystem before flushing to
// the resulting socked
class DeferredBufferForwarder : public BufferForwarder {
 public:
  explicit DeferredBufferForwarder(zx::socket destination)
      : BufferForwarder(std::move(destination)) {}

  TransferStatus Flush() override;

  ~DeferredBufferForwarder() override;

  // Protected for testing
 protected:
  // Writes the contents of |data| to a temporary location on disk. Data can then be flushed to the
  // socket once the trace is complete.
  //
  // Returns TransferStatus::kComplete if the entire buffer has been
  // successfully transferred. A return value of
  // TransferStatus::kReceiverDead indicates that the peer was closed
  // during the transfer.
  TransferStatus WriteBuffer(cpp20::span<const uint8_t> data) const override;
  bool flushed_{false};
};
}  // namespace tracing

#endif  // SRC_PERFORMANCE_TRACE_MANAGER_DEFERRED_BUFFER_FORWARDER_H_
