// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix_uapi/user_buffer.h"

#include <lib/mistos/util/num.h>

namespace starnix_uapi {

fit::result<Errno, size_t> UserBuffer::cap_buffers_to_max_rw_count(UserAddress max_address,
                                                                   UserBuffers& buffers) {
  for (auto& buffer : buffers) {
    auto result = buffer.address_.checked_add(buffer.length_);
    if (!result.has_value()) {
      return fit::error(errno(EINVAL));
    }
    if (buffer.address_ > max_address || result.value() > max_address) {
      return fit::error(errno(EFAULT));
    }
  }

  auto max_rw_count = MAX_RW_COUNT;
  auto total = 0ul;
  auto offset = 0ul;
  while (offset < buffers.size()) {
    auto result = mtl::checked_add(total, buffers[offset].length_);
    if (!result.has_value()) {
      return fit::error(errno(EINVAL));
    }
    total = result.value();
    if (total >= max_rw_count) {
      buffers[offset].length_ -= total - max_rw_count;
      total = max_rw_count;
      buffers.truncate(offset + 1);
      break;
    }
    offset += 1;
  }
  return fit::ok(total);
}

fit::result<Errno> UserBuffer::advance(size_t length) {
  auto add = address_.checked_add(length);
  if (!add.has_value()) {
    return fit::error(errno(EINVAL));
  }
  address_ = *add;

  auto sub = mtl::checked_sub(length_, length);
  if (!sub.has_value()) {
    return fit::error(errno(EINVAL));
  }
  length_ = *sub;

  return fit::ok();
}

}  // namespace starnix_uapi
