// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/buffers/io_buffers.h"

#include <lib/mistos/starnix/kernel/mm/memory_manager.h>

#include <algorithm>
#include <iterator>

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>

namespace starnix {

fit::result<Errno, size_t> OutputBuffer::write(const ktl::span<uint8_t>& buffer) {
  ktl::span buf = buffer;
  return write_each([&](ktl::span<uint8_t>& data) -> fit::result<Errno, size_t> {
    auto size = ktl::min(buf.size(), data.size());
    ktl::span to_clone(buf.data(), size);
    ktl::span remaining(buf.data() + size, buf.size() - size);
    memcpy(data.data(), to_clone.data(), size);
    buf = remaining;
    return fit::ok(size);
  });
}

fit::result<Errno, size_t> OutputBuffer::write_all(const ktl::span<uint8_t>& buffer) {
  auto result = write(buffer);
  if (result.is_error())
    return result.take_error();

  auto size = result.value();
  if (size != buffer.size()) {
    return fit::error(errno(EINVAL));
  }
  return fit::ok(size);
}

fit::result<Errno, size_t> OutputBuffer::write_buffer(InputBuffer& input) {
  return write_each([&](ktl::span<uint8_t>& data) -> fit::result<Errno, size_t> {
    auto size = ktl::min(data.size(), input.available());
    ktl::span<uint8_t> tmp{data.data(), size};
    return input.read_exact(tmp);
  });
}

fit::result<Errno, fbl::Vector<uint8_t>> InputBuffer::peek_all() {
  // SAFETY: self.peek returns the number of bytes read.
  return read_to_vec<Errno, uint8_t>(
      available(), [&](ktl::span<uint8_t>& buf) -> fit::result<Errno, NumberOfElementsRead> {
        auto peek_result = this->peek(buf);
        if (peek_result.is_error())
          return peek_result.take_error();
        return fit::ok(NumberOfElementsRead{peek_result.value()});
      });
}

VecInputBuffer VecInputBuffer::New(const ktl::span<const uint8_t>& data) {
  fbl::AllocChecker ac;
  fbl::Vector<uint8_t> buffer;
  buffer.resize(data.size(), &ac);
  ASSERT(ac.check());
  memcpy(buffer.data(), data.data(), data.size());
  return VecInputBuffer(ktl::move(buffer));
}

VecInputBuffer VecInputBuffer::from(fbl::Vector<uint8_t> buffer) {
  return VecInputBuffer(ktl::move(buffer));
}

}  // namespace starnix
