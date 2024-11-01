// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_DEVICE_TYPE_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_DEVICE_TYPE_H_

#include <lib/mistos/util/range-map.h>
#include <zircon/types.h>

namespace starnix_uapi {

constexpr uint32_t MEM_MAJOR = 1;
constexpr uint32_t TTY_ALT_MAJOR = 5;
constexpr uint32_t LOOP_MAJOR = 7;
constexpr uint32_t MISC_MAJOR = 10;
constexpr uint32_t INPUT_MAJOR = 13;
constexpr uint32_t FB_MAJOR = 29;

// These minor device numbers in the MISC major device appear to be dynamically allocated.
// The lower bound is taken from observing /proc/misc an Android device. The upper bound is
// taken from the value of /dev/beep in devices.txt.
constexpr util::Range<uint32_t> MISC_DYNANIC_MINOR_RANGE{.start = 52, .end = 128};

// TODO: The range for dynamic character devices actually goes all the way to 254, but we
// still have a few hardcoded devices registered at high numbers. We can expand this range
// to 254 once we dynamically allocate those devices.
constexpr util::Range<uint32_t> DYN_MAJOR_RANGE{.start = 234, .end = 251};

constexpr uint32_t REMOTE_BLOCK_MAJOR = 251;
// Unclear if this device number is assigned dynamically, but this value is what abarth observed
// once for /dev/block/zram0.
constexpr uint32_t ZRAM_MAJOR = 252;

// This value is observed from dmsetup.
constexpr uint32_t DEVICE_MAPPER_MAJOR = 254;

class DeviceType {
 public:
  static const DeviceType NONE;

  // MEM
  static const DeviceType _NULL;
  static const DeviceType ZERO;
  static const DeviceType FULL;
  static const DeviceType RANDOM;
  static const DeviceType URANDOM;
  static const DeviceType KMSG;

  // TTY_ALT
  static const DeviceType TTY;
  static const DeviceType PTMX;

  // MISC
  static const DeviceType HW_RANDOM;
  static const DeviceType UINPUT;
  static const DeviceType FUSE;
  static const DeviceType DEVICE_MAPPER;
  static const DeviceType LOOP_CONTROL;

  // Frame buffer
  static const DeviceType FB0;

  // TUN
  static const DeviceType TUN;

  DeviceType(uint64_t val) : value_(val) {}

  static DeviceType New(uint32_t major, uint32_t minor) {
    // This encoding is part of the Linux UAPI. The encoded value is
    // returned to userspace in the stat struct.
    // See <https://man7.org/linux/man-pages/man3/makedev.3.html>.
    return DeviceType((((major & 0xfffff000ULL) << 32) | ((major & 0xfffULL) << 8) |
                       ((minor & 0xffffff00ULL) << 12) | (minor & 0xffULL)));
  }

  static util::Range<DeviceType> new_range(uint32_t major, util::Range<uint32_t> minor_range) {
    return util::Range<DeviceType>{.start = New(major, minor_range.start),
                                   .end = New(major, minor_range.end)};
  }

  static DeviceType from_bits(uint64_t dev) { return DeviceType(dev); }

  uint64_t bits() const { return value_; }

  DeviceType next_minor() const {
    uint32_t next = minor() + 1;
    if (next < minor()) {
      return NONE;
    }
    return New(major(), next);
  }

  uint32_t major() const {
    return ((value_ >> 32 & 0xfffff000ULL) | ((value_ >> 8) & 0xfffULL)) & 0xFFFFFFFF;
  }

  uint32_t minor() const {
    return ((value_ >> 12 & 0xffffff00ULL) | (value_ & 0xffULL)) & 0xFFFFFFFF;
  }

 private:
  uint64_t value_;
};

}  // namespace starnix_uapi

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_DEVICE_TYPE_H_
