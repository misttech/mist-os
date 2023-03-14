// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_KTRACE_PROVIDER_DEVICE_READER_H_
#define SRC_PERFORMANCE_KTRACE_PROVIDER_DEVICE_READER_H_

#include <fuchsia/tracing/kernel/cpp/fidl.h>
#include <lib/sys/cpp/service_directory.h>

#include "src/performance/ktrace_provider/reader.h"

namespace ktrace_provider {

class DeviceReader : public Reader {
 public:
  DeviceReader();

  zx_status_t Init(const std::shared_ptr<sys::ServiceDirectory>& svc);

 private:
  static constexpr size_t kChunkSize{16 * 4 * 1024};

  void ReadMoreData() override;

  fuchsia::tracing::kernel::ReaderSyncPtr ktrace_reader_;
  size_t offset_ = 0;
  char buffer_[kChunkSize];

  DeviceReader(const DeviceReader&) = delete;
  DeviceReader(DeviceReader&&) = delete;
  DeviceReader& operator=(const DeviceReader&) = delete;
  DeviceReader& operator=(DeviceReader&&) = delete;
};

}  // namespace ktrace_provider

#endif  // SRC_PERFORMANCE_KTRACE_PROVIDER_DEVICE_READER_H_
