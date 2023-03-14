// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/ktrace_provider/device_reader.h"

#include <lib/syslog/cpp/macros.h>
#include <limits.h>
#include <sys/stat.h>
#include <unistd.h>
#include <zircon/status.h>

#include <algorithm>

#include <src/lib/files/eintr_wrapper.h>

namespace ktrace_provider {

DeviceReader::DeviceReader() : Reader(buffer_, kChunkSize) {}

zx_status_t DeviceReader::Init(const std::shared_ptr<sys::ServiceDirectory>& svc) {
  return svc->Connect(ktrace_reader_.NewRequest());
}

void DeviceReader::ReadMoreData() {
  memmove(buffer_, current_, AvailableBytes());
  char* new_marker = buffer_ + AvailableBytes();

  while (new_marker < end_) {
    size_t read_size = std::distance(const_cast<const char*>(new_marker), end_);
    read_size = std::clamp(read_size, 0lu, static_cast<size_t>(fuchsia::tracing::kernel::MAX_BUF));
    zx_status_t out_status;
    std::vector<uint8_t> buf;
    auto status = ktrace_reader_->ReadAt(read_size, offset_, &out_status, &buf);
    if (status != ZX_OK || out_status != ZX_OK) {
      FX_LOGS(ERROR) << "Failed to read from ktrace reader status:" << status
                     << "out_status:" << out_status;
      break;
    }

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
