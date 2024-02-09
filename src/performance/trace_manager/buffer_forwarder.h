// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_TRACE_MANAGER_BUFFER_FORWARDER_H_
#define SRC_PERFORMANCE_TRACE_MANAGER_BUFFER_FORWARDER_H_
#include <lib/zx/socket.h>

#include <utility>

#include "src/performance/trace_manager/util.h"

namespace tracing {
class BufferForwarder {
 public:
  explicit BufferForwarder(zx::socket destination) : destination_(std::move(destination)) {}

  // Writes |len| bytes from |buffer| to the output socket. Returns
  // TransferStatus::kComplete if the entire buffer has been
  // successfully transferred. A return value of
  // TransferStatus::kReceiverDead indicates that the peer was closed
  // during the transfer.
  TransferStatus WriteBuffer(const void* buffer, size_t len) const;

 private:
  const zx::socket destination_;
};
}  // namespace tracing

#endif  // SRC_PERFORMANCE_TRACE_MANAGER_BUFFER_FORWARDER_H_
