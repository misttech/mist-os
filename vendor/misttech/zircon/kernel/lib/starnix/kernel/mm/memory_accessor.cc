// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/mm/memory_accessor.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix_uapi/errors.h>

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>

#include <linux/uio.h>

namespace starnix {

fit::result<Errno, fbl::Vector<uint8_t>> MemoryAccessorExt::read_memory_to_vec(UserAddress addr,
                                                                               size_t len) const {
  return read_to_vec<Errno, uint8_t>(
      len, [&](ktl::span<uint8_t> b) -> fit::result<Errno, NumberOfElementsRead> {
        auto read_result = this->read_memory(addr, b);
        if (read_result.is_error()) {
          return read_result.take_error();
        }
        DEBUG_ASSERT(len == read_result.value().size());
        return fit::ok(NumberOfElementsRead{read_result.value().size()});
      });
}

fit::result<Errno, fbl::Vector<uint8_t>> MemoryAccessorExt::read_memory_partial_to_vec(
    UserAddress addr, size_t max_len) const {
  return read_to_vec<Errno, uint8_t>(
      max_len, [&](ktl::span<uint8_t> b) -> fit::result<Errno, NumberOfElementsRead> {
        auto read_result = this->read_memory_partial(addr, b);
        if (read_result.is_error()) {
          return read_result.take_error();
        }
        return fit::ok(NumberOfElementsRead{read_result.value().size()});
      });
}

fit::result<Errno, UserBuffers> MemoryAccessorExt::read_iovec(
    UserAddress iovec_addr, UserValue<uint32_t> iovec_count) const {
  auto local_iovec_count = iovec_count.try_into<size_t>();
  if (!local_iovec_count.has_value()) {
    return fit::error(errno(EINVAL));
  }
  if (local_iovec_count > UIO_MAXIOV) {
    return fit::error(errno(EINVAL));
  }

  return read_objects_to_smallvec<UserBuffer, 1>(UserRef<UserBuffer>::New(iovec_addr),
                                                 *local_iovec_count);
}

fit::result<Errno, FsString> MemoryAccessorExt::read_c_string_to_vec(UserCString string,
                                                                     size_t max_size) const {
  auto chunk_size = ktl::min(static_cast<size_t>(PAGE_SIZE), max_size);
  fbl::AllocChecker ac;
  fbl::Vector<uint8_t> buf;
  buf.reserve(chunk_size, &ac);
  if (!ac.check()) {
    return fit::error(errno(ENOMEM));
  }
  size_t index = 0;

  do {
    // This operation should never overflow: we should fail to read before that.
    auto addr = string->checked_add(index);
    if (!addr.has_value()) {
      return fit::error(errno(EFAULT));
    }
    ASSERT(index + chunk_size <= buf.capacity());

    ktl::span<uint8_t> spare_capacity{buf.data() + buf.size(), buf.capacity() - buf.size()};
    auto to_read = spare_capacity.subspan(0, chunk_size);
    auto read_bytes = read_memory_partial_until_null_byte(addr.value(), to_read) _EP(read_bytes);
    auto read_len = read_bytes->size();

    // Check if the last byte read is the null byte.
    if (read_bytes->last(1)[0] == '\0') {
      auto null_index = index + read_len - 1;
      buf.set_size(null_index);
      if (buf.size() > max_size) {
        return fit::error(errno(ENAMETOOLONG));
      }
      return fit::ok(FsString(reinterpret_cast<char*>(buf.data()), buf.size()));
    }

    index += read_len;

    if ((read_len < chunk_size) || (index >= max_size)) {
      // There's no more for us to read.
      return fit::error(errno(ENAMETOOLONG));
    }

    // Set the correct size for the next iteration.
    buf.set_size(index);

    // Trigger a capacity increase.
    buf.reserve(index + chunk_size, &ac);
    if (!ac.check()) {
      return fit::error(errno(ENOMEM));
    }

  } while (true);
}

fit::result<Errno, FsString> MemoryAccessorExt::read_c_string(UserCString string,
                                                              ktl::span<uint8_t>& buffer) const {
  auto buffer_or_error = this->read_memory_partial_until_null_byte(*string, buffer);
  if (buffer_or_error.is_error()) {
    return buffer_or_error.take_error();
  }
  // Make sure the last element holds the null byte.
  if (buffer_or_error->last(1)[0] == '\0') {
    return fit::ok(
        FsString(reinterpret_cast<char*>(buffer_or_error->data()), buffer_or_error->size() - 1));
  }
  return fit::error(errno(ENAMETOOLONG));
}

}  // namespace starnix
