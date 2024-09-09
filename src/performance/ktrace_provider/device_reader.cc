// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/ktrace_provider/device_reader.h"

#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>
#include <limits.h>
#include <sys/stat.h>
#include <unistd.h>
#include <zircon/status.h>

#include <src/lib/files/eintr_wrapper.h>

namespace ktrace_provider {

DeviceReader::DeviceReader() : Reader(buffer_, kChunkSize) {}

zx_status_t DeviceReader::Init() {
  zx::result client_end = component::Connect<fuchsia_tracing_kernel::Reader>();
  if (!client_end.is_ok()) {
    FX_PLOGS(ERROR, client_end.status_value()) << "Failed to connect to fuchsia.tracing.kernel";
    return client_end.error_value();
  }
  ktrace_reader_ = fidl::SyncClient(std::move(*client_end));

  return ZX_OK;
}

void DeviceReader::ReadMoreData() {
  memmove(buffer_, current_, AvailableBytes());
  char* new_marker = buffer_ + AvailableBytes();

  while (new_marker < end_) {
    size_t read_size = std::distance(const_cast<const char*>(new_marker), end_);
    read_size = std::clamp(read_size, 0lu, static_cast<size_t>(fuchsia_tracing_kernel::kMaxBuf));
    auto status = ktrace_reader_->ReadAt({{.count = read_size, .offset = offset_}});
    if (status.is_error()) {
      FX_LOGS(ERROR) << "Failed to read from ktrace reader status: "
                     << status.error_value().status_string();
    }
    if (status->status() != ZX_OK) {
      FX_PLOGS(ERROR, status->status()) << "Failed to read from ktrace reader";
      break;
    }
    std::vector<uint8_t>& buf = status->data();

    if (buf.empty()) {
      break;
    }

    memcpy(new_marker, buf.data(), buf.size());
    offset_ += buf.size();
    new_marker += buf.size();
  }

  marker_ = new_marker;
  current_ = buffer_;
}

}  // namespace ktrace_provider
