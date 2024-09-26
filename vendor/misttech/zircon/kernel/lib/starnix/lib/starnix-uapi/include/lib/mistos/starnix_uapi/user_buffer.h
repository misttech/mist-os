// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_USER_BUFFER_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_USER_BUFFER_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/small_vector.h>
#include <zircon/types.h>

#include <arch/defines.h>

namespace starnix_uapi {

static size_t MAX_RW_COUNT = static_cast<size_t>(1 << 31) - PAGE_SIZE;

struct UserBuffer;
using UserBuffers = util::SmallVector<UserBuffer, 1>;

// Matches iovec_t.
struct UserBuffer {
  UserAddress address_;
  size_t length_;

 public:
  // impl UserBuffer
  static fit::result<Errno, size_t> cap_buffers_to_max_rw_count(UserAddress max_address,
                                                                UserBuffers& buffers);

  fit::result<Errno> advance(size_t length);

  // Returns whether the buffer address is 0 and its length is 0.
  bool is_null() const { return address_.is_null() && is_empty(); }

  // Returns whether the buffer length is 0.
  bool is_empty() const { return length_ == 0; }
};

}  // namespace starnix_uapi

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_USER_BUFFER_H_
