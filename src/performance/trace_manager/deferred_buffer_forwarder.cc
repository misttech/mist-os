// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/trace_manager/deferred_buffer_forwarder.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/trace-engine/fields.h>

#include <filesystem>
#include <format>

namespace tracing {

namespace {
const char* kTraceDir = "/traces";
}

DeferredBufferForwarder::DeferredBufferForwarder(zx::socket destination)
    : BufferForwarder(std::move(destination)) {
  // In the event where trace_manager was killed while copying a trace, we might have an old file
  // laying around. Remove them just in case.
  std::filesystem::path dir_path = kTraceDir;  // Current directory
  for (auto& p : std::filesystem::directory_iterator(dir_path)) {
    std::filesystem::remove(p);
  }
  std::chrono::time_point now = std::chrono::system_clock::now();
  std::string fname = std::format("trace_{}.fxt", now.time_since_epoch().count());
  buffer_path_ = dir_path / fname;
  buffer_file_ = fopen(buffer_path_.c_str(), "a+");
}

DeferredBufferForwarder::~DeferredBufferForwarder() {
  Flush();
  if (buffer_file_ != nullptr) {
    fclose(buffer_file_);
  }
  std::filesystem::remove(buffer_path_);
}
TransferStatus DeferredBufferForwarder::Flush() {
  if (flushed_) {
    return TransferStatus::kComplete;
  }
  if (buffer_file_ == nullptr) {
    FX_LOGS(ERROR) << "Failed to open trace file: " << buffer_path_ << " for read!";
    return TransferStatus::kWriteError;
  }
  if (fseek(buffer_file_, 0, SEEK_SET) != 0) {
    FX_LOGS(ERROR) << "Failed to seek to beginning of: " << buffer_path_ << " for read!";
    return TransferStatus::kWriteError;
  }

  const size_t BUFFER_SIZE = 4096;
  uint8_t buffer[BUFFER_SIZE];
  for (;;) {
    size_t bytes_read = fread(buffer, sizeof(uint8_t), BUFFER_SIZE, buffer_file_);
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
  if (buffer_file_ == nullptr) {
    FX_LOGS(ERROR) << "Failed to open trace file for write: " << buffer_path_;
    return TransferStatus::kWriteError;
  }
  while (!data.empty()) {
    size_t actual = fwrite(data.data(), sizeof(uint8_t), data.size(), buffer_file_);
    data = data.subspan(actual);
  }
  return TransferStatus::kComplete;
}

}  // namespace tracing
