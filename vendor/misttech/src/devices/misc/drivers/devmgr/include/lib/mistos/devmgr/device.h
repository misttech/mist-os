// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_DEVMGR_INCLUDE_LIB_MISTOS_DEVMGR_DEVICE_H_
#define VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_DEVMGR_INCLUDE_LIB_MISTOS_DEVMGR_DEVICE_H_

namespace devmgr {

class Coordinator;

class Device {
 public:
  Device(Coordinator* coordinator);
  ~Device();
};

}  // namespace devmgr

#endif  // VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_DEVMGR_INCLUDE_LIB_MISTOS_DEVMGR_DEVICE_H_
