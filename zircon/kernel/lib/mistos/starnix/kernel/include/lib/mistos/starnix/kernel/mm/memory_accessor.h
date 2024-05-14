// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_ACCESSOR_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_ACCESSOR_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>

#include <fbl/vector.h>
#include <ktl/byte.h>
#include <ktl/span.h>

using namespace starnix_uapi;

namespace starnix {

class MemoryAccessor {
 public:
  /// Reads exactly `bytes.len()` bytes of memory from `addr` into `bytes`.
  ///
  /// In case of success, the number of bytes read will always be `bytes.len()`.
  ///
  /// Consider using `MemoryAccessorExt::read_memory_to_*` methods if you do not require control
  /// over the allocation.
  virtual fit::result<Errno, ktl::span<uint8_t>> read_memory(UserAddress addr,
                                                             ktl::span<uint8_t>& bytes) const = 0;

  /// Reads bytes starting at `addr`, continuing until either a null byte is read, `bytes.len()`
  /// bytes have been read or no more bytes can be read from the target.
  ///
  /// This is used, for example, to read null-terminated strings where the exact length is not
  /// known, only the maximum length is.
  ///
  /// Returns the bytes that have been read to on success.
  // virtual fit::result<Errno, ktl::span<uint8_t>> read_memory_partial_until_null_byte(
  //     UserAddress addr, ktl::span<uint8_t>& bytes);

  /// Reads bytes starting at `addr`, continuing until either `bytes.len()` bytes have been read
  /// or no more bytes can be read from the target.
  ///
  /// This is used, for example, to read null-terminated strings where the exact length is not
  /// known, only the maximum length is.
  ///
  /// Consider using `MemoryAccessorExt::read_memory_partial_to_*` methods if you do not require
  /// control over the allocation.
  // virtual fit::result<Errno, ktl::span<uint8_t>> read_memory_partial(UserAddress addr,
  //                                                                    ktl::span<uint8_t>& bytes) =
  //                                                                    0;

  /// Writes the provided bytes to `addr`.
  ///
  /// In case of success, the number of bytes written will always be `bytes.len()`.
  ///
  /// # Parameters
  /// - `addr`: The address to write to.
  /// - `bytes`: The bytes to write from.
  virtual fit::result<Errno, size_t> write_memory(UserAddress addr,
                                                  const ktl::span<const uint8_t>& bytes) const = 0;

  /// Writes bytes starting at `addr`, continuing until either `bytes.len()` bytes have been
  /// written or no more bytes can be written.
  ///
  /// # Parameters
  /// - `addr`: The address to write to.
  /// - `bytes`: The bytes to write from.
  // virtual fit::result<Errno, size_t> write_memory_partial(
  //     UserAddress addr, const ktl::span<const uint8_t>& bytes) = 0;

  /// Writes zeros starting at `addr` and continuing for `length` bytes.
  ///
  /// Returns the number of bytes that were zeroed.
  // virtual fit::result<Errno, size_t> zero(UserAddress addr, size_t length) = 0;

  virtual ~MemoryAccessor() = default;
};

class MemoryAccessorExt : public MemoryAccessor {
 public:
  /// Read exactly `len` bytes of memory, returning them as a a fbl::Vector.
  virtual fit::result<Errno, fbl::Vector<uint8_t>> read_memory_to_vec(UserAddress addr,
                                                                      size_t len) const;

  virtual ~MemoryAccessorExt() = default;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_ACCESSOR_H_
