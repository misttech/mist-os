// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/buffers/io_buffers.h"

#include <lib/mistos/starnix/kernel/mm/memory_manager.h>

#include <vector>

namespace starnix {

fit::result<Errno, size_t> OutputBuffer::write(ktl::span<uint8_t> _buffer) {
  ktl::span buffer{_buffer.data(), _buffer.size()};

  return write_each([&buffer](ktl::span<uint8_t> data) -> fit::result<Errno, size_t> {
    auto size = std::min(buffer.size(), data.size());
    ktl::span to_clone(buffer.data(), size);
    ktl::span remaining(buffer.data() + size, buffer.size() - size);
    __unsanitized_memcpy(data.data(), to_clone.data(), size);
    buffer = remaining;
    return fit::ok(size);
  });
}

fit::result<Errno, size_t> OutputBuffer::write_all(ktl::span<uint8_t> buffer) {
  auto result = write(buffer);
  if (result.is_error())
    return result.take_error();

  auto size = result.value();
  if (size != buffer.size()) {
    return fit::error(errno(EINVAL));
  } else {
    return fit::ok(size);
  }
}

fit::result<Errno, size_t> OutputBuffer::write_buffer(InputBuffer* input) {
  return write_each([input](ktl::span<uint8_t> data) -> fit::result<Errno, size_t> {
    auto size = std::min(data.size(), input->available());
    ktl::span<uint8_t> tmp{data.data(), size};
    return input->read_exact(tmp);
  });
}

fit::result<Errno, std::vector<uint8_t>> InputBuffer::peek_all() {
  // SAFETY: self.peek returns the number of bytes read.
  return read_to_vec<uint8_t, Errno>(
      available(), [&](ktl::span<uint8_t> buf) -> fit::result<Errno, NumberOfElementsRead> {
        auto peek_result = this->peek(buf);
        if (peek_result.is_error())
          return peek_result.take_error();
        return fit::ok(NumberOfElementsRead{peek_result.value()});
      });
}

VecInputBuffer VecInputBuffer::New(ktl::span<uint8_t> data) {
  return VecInputBuffer(std::vector<uint8_t>(data.begin(), data.end()));
}

VecInputBuffer VecInputBuffer::from(const std::vector<uint8_t>& buffer) {
  return VecInputBuffer(buffer);
}

}  // namespace starnix
