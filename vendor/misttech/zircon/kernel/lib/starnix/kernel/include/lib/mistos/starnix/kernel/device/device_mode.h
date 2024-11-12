// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_DEVICE_MODE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_DEVICE_MODE_H_

#include <lib/mistos/util/range-map.h>
#include <zircon/types.h>

namespace starnix {

class CurrentTask;
class DeviceOpen;
class FileOps;
class FsNode;

constexpr uint32_t kChrdevMinorMax = 256;
constexpr uint32_t kBlkdevMinorMax = 1 << 20;

/// The mode or category of the device driver.
class DeviceMode {
 public:
  enum class Type : uint8_t {
    kChar,  /// This device is a character device
    kBlock  /// This device is a block device
  };

  /// Get the number of minor device numbers available for this device mode.
  uint32_t minor_count() const {
    switch (type_) {
      case Type::kChar:
        return kChrdevMinorMax;
      case Type::kBlock:
        return kBlkdevMinorMax;
    }
  }

  /// Get the range of minor device numbers available for this device mode.
  util::Range<uint32_t> minor_range() const {
    return util::Range<uint32_t>{.start = 0, .end = minor_count()};
  }

  explicit DeviceMode(Type type) : type_(type) {}

 private:
  friend class DeviceRegistry;
  friend class KObjectStore;

  Type type_;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_DEVICE_MODE_H_
