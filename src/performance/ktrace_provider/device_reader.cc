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

DeviceReader::DeviceReader(zx::resource debug_resource)
    : Reader(buffer_, kChunkSize), debug_resource_(std::move(debug_resource)) {}

void DeviceReader::ReadMoreData() {
  memmove(buffer_, current_, AvailableBytes());
  char* new_marker = buffer_ + AvailableBytes();

  while (new_marker < end_) {
    size_t read_size = std::distance(const_cast<const char*>(new_marker), end_);
    size_t actual;
    if (zx_status_t status =
            zx_ktrace_read(debug_resource_.get(), new_marker, offset_, read_size, &actual);
        status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to read from zx_ktrace open";
      break;
    }

    if (actual == 0) {
      break;
    }

    offset_ += actual;
    new_marker += actual;
  }

  marker_ = new_marker;
  current_ = buffer_;
}

}  // namespace ktrace_provider
