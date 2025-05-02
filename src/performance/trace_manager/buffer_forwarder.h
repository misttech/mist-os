// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_TRACE_MANAGER_BUFFER_FORWARDER_H_
#define SRC_PERFORMANCE_TRACE_MANAGER_BUFFER_FORWARDER_H_
#include <lib/stdcompat/span.h>
#include <lib/zx/socket.h>

#include <utility>

#include "src/performance/trace_manager/util.h"

namespace tracing {
class BufferForwarder {
 public:
  explicit BufferForwarder(zx::socket destination) : destination_(std::move(destination)) {}

  // Write the FxT Magic Bytes to the underlying socket.
  TransferStatus WriteMagicNumberRecord() const;

  TransferStatus WriteProviderInfoRecord(uint32_t provider_id, const std::string& name) const;
  TransferStatus WriteProviderSectionRecord(uint32_t provider_id) const;
  TransferStatus WriteProviderBufferOverflowEvent(uint32_t provider_id) const;

  enum class ForwardStrategy : bool {
    Size,
    Records,
  };

  // Write the records in |buffer| at |vmo_offset| to the output. |size| is the size in bytes of the
  // chunk to examine, which may be more than was written if |strategy| is
  // `ForwardStrategy::Record`. It must always be a multiple of 8.
  //
  // In oneshot mode we assume the end of written records don't look like records and we can just
  // run through the buffer examining records to compute how many are there. This is problematic
  // (without extra effort) in circular and streaming modes as records are written and rewritten.
  // This function handles both cases. If |strategy| is ForwardStrategy::Record then run through the
  // buffer computing the size of each record until we find no more records. If |strategy| is
  // ForwardStrategy::Size then |size| is the number of bytes to write.
  TransferStatus WriteChunkBy(ForwardStrategy strategy, const zx::vmo& vmo, size_t vmo_offset,
                              size_t size) const;

  virtual TransferStatus Flush() { return TransferStatus::kComplete; }
  virtual ~BufferForwarder() = default;

 protected:
  // Writes the contents of |data| to the output socket. Returns
  // TransferStatus::kComplete if the entire buffer has been
  // successfully transferred. A return value of
  // TransferStatus::kReceiverDead indicates that the peer was closed
  // during the transfer.
  virtual TransferStatus WriteBuffer(cpp20::span<const uint8_t> data) const;
  const zx::socket destination_;
};
}  // namespace tracing

#endif  // SRC_PERFORMANCE_TRACE_MANAGER_BUFFER_FORWARDER_H_
