// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_OPEN_FLAGS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_OPEN_FLAGS_H_

#include <lib/mistos/util/bitflags.h>

#include <linux/fcntl.h>

namespace starnix_uapi::inner_flags {

// Enum class to represent mapping options
enum class OpenFlagsEnum : uint32_t {
  ACCESS_MASK = 0x3,

  // The access modes are not really bits. Instead, they're an enum
  // embedded in the bitfield. Use ACCESS_MASK to extract the enum
  // or use the OpenFlags::can_read and OpenFlags::can_write functions.
  RDONLY = O_RDONLY,
  WRONLY = O_WRONLY,
  RDWR = O_RDWR,

  CREAT = O_CREAT,
  EXCL = O_EXCL,
  NOCTTY = O_NOCTTY,
  TRUNC = O_TRUNC,
  APPEND = O_APPEND,
  NONBLOCK = O_NONBLOCK,
  DSYNC = O_DSYNC,
  ASYNC = FASYNC,
  DIRECT = O_DIRECT,
  LARGEFILE = O_LARGEFILE,
  DIRECTORY = O_DIRECTORY,
  NOFOLLOW = O_NOFOLLOW,
  NOATIME = O_NOATIME,
  CLOEXEC = O_CLOEXEC,
  SYNC = O_SYNC,
  PATH = O_PATH,
  TMPFILE = O_TMPFILE,
  NDELAY = O_NDELAY,
};

using OpenFlags = Flags<OpenFlagsEnum>;

class OpenFlagsImpl : public OpenFlags {
 public:
  explicit OpenFlagsImpl(OpenFlags flag) : OpenFlags(flag) {}

  static OpenFlagsImpl from_bits_truncate(OpenFlags::Bits bits) {
    return OpenFlagsImpl(from_bits_retain(bits & OpenFlags::all().bits()));
  }

  bool can_read() {
    auto access_mode = bits() & OpenFlags(OpenFlagsEnum::ACCESS_MASK).bits();
    return access_mode == O_RDONLY || access_mode == O_RDWR;
  }

  bool can_write() {
    auto access_mode = bits() & OpenFlags(OpenFlagsEnum::ACCESS_MASK).bits();
    return access_mode == O_WRONLY || access_mode == O_RDWR;
  }
};

}  // namespace starnix_uapi::inner_flags

template <>
constexpr Flag<starnix_uapi::inner_flags::OpenFlagsEnum>
    Flags<starnix_uapi::inner_flags::OpenFlagsEnum>::FLAGS[] = {
        {starnix_uapi::inner_flags::OpenFlagsEnum::ACCESS_MASK},
        {starnix_uapi::inner_flags::OpenFlagsEnum::CREAT},
        {starnix_uapi::inner_flags::OpenFlagsEnum::EXCL},
        {starnix_uapi::inner_flags::OpenFlagsEnum::NOCTTY},
        {starnix_uapi::inner_flags::OpenFlagsEnum::TRUNC},
        {starnix_uapi::inner_flags::OpenFlagsEnum::APPEND},
        {starnix_uapi::inner_flags::OpenFlagsEnum::NONBLOCK},
        {starnix_uapi::inner_flags::OpenFlagsEnum::DSYNC},
        {starnix_uapi::inner_flags::OpenFlagsEnum::ASYNC},
        {starnix_uapi::inner_flags::OpenFlagsEnum::DIRECT},
        {starnix_uapi::inner_flags::OpenFlagsEnum::LARGEFILE},
        {starnix_uapi::inner_flags::OpenFlagsEnum::DIRECTORY},
        {starnix_uapi::inner_flags::OpenFlagsEnum::NOFOLLOW},
        {starnix_uapi::inner_flags::OpenFlagsEnum::NOATIME},
        {starnix_uapi::inner_flags::OpenFlagsEnum::CLOEXEC},
        {starnix_uapi::inner_flags::OpenFlagsEnum::SYNC},
        {starnix_uapi::inner_flags::OpenFlagsEnum::PATH},
        {starnix_uapi::inner_flags::OpenFlagsEnum::TMPFILE},
        {starnix_uapi::inner_flags::OpenFlagsEnum::NDELAY},
};

namespace starnix_uapi {

using OpenFlagsEnum = inner_flags::OpenFlagsEnum;
using OpenFlags = inner_flags::OpenFlags;
using OpenFlagsImpl = inner_flags::OpenFlagsImpl;

}  // namespace starnix_uapi

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_OPEN_FLAGS_H_
