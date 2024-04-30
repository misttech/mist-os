// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_ACCESSOR_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_ACCESSOR_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>

#include <ktl/byte.h>
#include <ktl/span.h>

using namespace starnix_uapi;

namespace starnix {

class MemoryAccessor {
 public:
  // virtual std::vector<uint8_t>* read_memory(UserAddress addr, std::vector<std::byte>& bytes) = 0;
  // virtual std::vector<uint8_t>* read_memory_partial_until_null_byte(
  //     UserAddress addr, std::vector<std::byte>& bytes) = 0;
  // virtual std::vector<uint8_t>* read_memory_partial(UserAddress addr,
  //                                                   std::vector<std::byte>& bytes) = 0;
  virtual fit::result<Errno, size_t> write_memory(
      UserAddress addr, const ktl::span<const ktl::byte>& bytes) const = 0;
  // virtual size_t write_memory_partial(UserAddress addr, const std::vector<std::byte>& bytes) = 0;
  // virtual size_t zero(UserAddress addr, size_t length) = 0;
  virtual ~MemoryAccessor() = default;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_ACCESSOR_H_
