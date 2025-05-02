// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/trace_manager/deferred_buffer_forwarder.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/trace-engine/fields.h>

namespace tracing {

namespace {
const char* kTraceFile = "/traces/trace.fxt";
}

DeferredBufferForwarder::~DeferredBufferForwarder() {
  Flush();
  std::remove(kTraceFile);
}
TransferStatus DeferredBufferForwarder::Flush() {
  if (flushed_) {
    return TransferStatus::kComplete;
  }
  FILE* f = fopen(kTraceFile, "r");
  if (f == nullptr) {
    FX_LOGS(ERROR) << "Failed to open trace file: " << kTraceFile << " for read!";
    return TransferStatus::kWriteError;
  }

  const size_t BUFFER_SIZE = 4096;
  uint8_t buffer[BUFFER_SIZE];
  for (;;) {
    size_t bytes_read = fread(buffer, sizeof(uint8_t), BUFFER_SIZE, f);
    if (bytes_read <= 0) {
      break;
    }
    size_t actual = 0;
    std::span<uint8_t> data{buffer, bytes_read};
    while (!data.empty()) {
      if (zx_status_t status = destination_.write(0u, data.data(), data.size(), &actual);
          status != ZX_OK) {
        if (status == ZX_ERR_SHOULD_WAIT) {
          zx_signals_t pending = 0;
          if (zx_status_t status = destination_.wait_one(ZX_SOCKET_WRITABLE | ZX_SOCKET_PEER_CLOSED,
                                                         zx::time::infinite(), &pending);
              status != ZX_OK) {
            FX_PLOGS(ERROR, status) << "Wait on socket failed: " << status;
            return TransferStatus::kWriteError;
          }

          if (pending & ZX_SOCKET_WRITABLE) {
            continue;
          }

          if (pending & ZX_SOCKET_PEER_CLOSED) {
            FX_PLOGS(ERROR, status) << "Peer closed while writing to socket";
            return TransferStatus::kReceiverDead;
          }
        }

        return TransferStatus::kWriteError;
      }
      data = data.subspan(actual);
    }
  }
  flushed_ = true;
  return TransferStatus::kComplete;
}

TransferStatus DeferredBufferForwarder::WriteBuffer(cpp20::span<const uint8_t> data) const {
  FILE* f = fopen(kTraceFile, "a+");
  if (f == nullptr) {
    FX_LOGS(ERROR) << "Failed to open trace file for write: " << kTraceFile;
    return TransferStatus::kWriteError;
  }
  while (!data.empty()) {
    size_t actual = fwrite(data.data(), sizeof(uint8_t), data.size(), f);
    data = data.subspan(actual);
  }
  fclose(f);
  return TransferStatus::kComplete;
}

}  // namespace tracing
