// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_KTRACE_PROVIDER_DEVICE_READER_H_
#define SRC_PERFORMANCE_KTRACE_PROVIDER_DEVICE_READER_H_

#include <lib/zx/resource.h>

#include "src/performance/ktrace_provider/reader.h"

namespace ktrace_provider {

class DeviceReader : public Reader {
 public:
  explicit DeviceReader(zx::resource debug_resource);

 private:
  static constexpr size_t kChunkSize{16 * 4 * 1024};

  void ReadMoreData() override;

  uint32_t offset_ = 0;
  zx::resource debug_resource_;

  // We read data into this buffer in byte sized chunks, but we want to read out aligned 8 byte
  // fxt words.
  alignas(8) char buffer_[kChunkSize];

  // Delete both copy and new as Reader holds a raw pointer to the storage
  DeviceReader(const DeviceReader&) = delete;
  DeviceReader(DeviceReader&&) = delete;
  DeviceReader& operator=(const DeviceReader&) = delete;
  DeviceReader& operator=(DeviceReader&&) = delete;
};

}  // namespace ktrace_provider

#endif  // SRC_PERFORMANCE_KTRACE_PROVIDER_DEVICE_READER_H_
