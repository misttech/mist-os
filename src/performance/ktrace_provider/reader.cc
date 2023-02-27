// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/ktrace_provider/reader.h"

#include <lib/fxt/fields.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zircon-internal/ktrace.h>
#include <zircon/assert.h>

namespace ktrace_provider {

Reader::Reader(const char* buffer, size_t buffer_size)
    : current_(buffer), marker_(buffer), end_(buffer + buffer_size) {
  // Ensure initial buffer is correctly aligned.
  ZX_ASSERT(reinterpret_cast<uintptr_t>(buffer) % alignof(uint64_t) == 0);
}

const uint64_t* Reader::ReadNextRecord() {
  if (AvailableBytes() < sizeof(uint64_t)) {
    ReadMoreData();
  }

  if (AvailableBytes() < sizeof(uint64_t)) {
    FX_VLOGS(10) << "No more records";
    return nullptr;
  }

  auto record = reinterpret_cast<const uint64_t*>(current_);
  size_t record_size_bytes = fxt::RecordFields::RecordSize::Get<size_t>(*record) * 8;
  if (record_size_bytes == 0) {
    // This can happen if the kernel writes a trace record without
    // committing it.  We can't skip over the record because we don't
    // know how large it is.
    FX_LOGS(WARNING) << "Uncommitted kernel trace record; stopping processing";
    return nullptr;
  }

  if (AvailableBytes() < record_size_bytes) {
    ReadMoreData();
  }

  if (AvailableBytes() < record_size_bytes) {
    FX_VLOGS(10) << "No more records, incomplete last record";
    return nullptr;
  }

  record = reinterpret_cast<const uint64_t*>(current_);

  current_ += record_size_bytes;
  number_bytes_read_ += record_size_bytes;
  number_records_read_ += 1;

  return record;
}

}  // namespace ktrace_provider
