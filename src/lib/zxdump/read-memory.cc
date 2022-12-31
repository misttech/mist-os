// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zxdump/task.h>

#include <algorithm>
#include <cinttypes>
#include <cstdio>

#include "buffer-impl.h"
#include "dump-file.h"

namespace zxdump {

fit::result<Error, Buffer<>> Process::ReadMemoryImpl(uint64_t vaddr, size_t size,
                                                     ReadMemorySize size_mode) {
  if (live()) {
    return ReadLiveMemory(vaddr, size, size_mode);
  }

  auto possible = [vaddr](const auto& elt) -> bool {
    const auto& [segment_vaddr, segment] = elt;
    return segment_vaddr + segment.memsz <= vaddr;
  };

  auto found = std::partition_point(memory_.begin(), memory_.end(), possible);
  if (found != memory_.end()) {
    const auto& [segment_vaddr, segment] = *found;
    if (vaddr >= segment_vaddr && vaddr - segment_vaddr < segment.memsz) {
      // This segment covers the address.

      if (segment.filesz <= vaddr - segment_vaddr || !dump_) {
        // The portion of this segment we need was elided, or the whole dump
        // was inserted with read_memory=false,
        return fit::ok(Buffer<>{});
      }

      // Map the segment to a region of the file.
      internal::FileRange where = {segment.offset, segment.filesz};
      where %= vaddr - segment_vaddr;

      // If the next segment is contiguous we can fetch across them.
      while (++found != memory_.end() && found->first == vaddr + where.size &&
             found->second.offset == where.offset + where.size) {
        where.size += found->second.filesz;
      }

      auto result = dump_->ReadMemory(where);
      if (result.is_error()) {
        return result.take_error();
      }
      Buffer<> buffer = *std::move(result);
      ZX_ASSERT(buffer->size_bytes() >= size);
      if (size_mode == ReadMemorySize::kExact) {
        buffer.data_ = buffer.data_.subspan(0, size);
      }
      return fit::ok(std::move(buffer));
    }
  }

  return fit::error(Error{"no such memory", ZX_ERR_NO_MEMORY});
}

}  // namespace zxdump
