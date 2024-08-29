// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_USER_BUFFER_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_USER_BUFFER_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <zircon/types.h>

namespace starnix_uapi {

// pub static MAX_RW_COUNT: Lazy<usize> = Lazy::new(|| ((1 << 31) - *PAGE_SIZE) as usize);

// Matches iovec_t.
struct UserBuffer {
  UserAddress address;
  size_t length;

  fit::result<Errno, size_t> cap_buffers_to_max_rw_count(UserAddress max_address,
                                                         UserBuffer buffers);

  fit::result<Errno> advance(size_t length);

  // Returns whether the buffer address is 0 and its length is 0.
  bool is_null() { return address.is_null() && is_empty(); }

  // Returns whether the buffer length is 0.
  bool is_empty() { return length == 0; }

  /*
  pub fn cap_buffers_to_max_rw_count(
        max_address: UserAddress,
        buffers: &mut UserBuffers,
    ) -> Result<usize, Errno> {

       pub fn advance(&mut self, length: usize) -> Result<(), Errno> {
        self.address = self.address.checked_add(length).ok_or_else(|| errno!(EINVAL))?;
        self.length = self.length.checked_sub(length).ok_or_else(|| errno!(EINVAL))?;
        Ok(())

  */
};

}  // namespace starnix_uapi

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_USER_BUFFER_H_
