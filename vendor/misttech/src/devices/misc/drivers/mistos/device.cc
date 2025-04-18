// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "misc/drivers/mistos/device.h"

namespace mistos {

Device::Device(device_t device, Driver* driver) : name_(device.name), driver_(driver) {}

zx_device_t* Device::ZxDevice() { return static_cast<zx_device_t*>(this); }

const char* Device::Name() const { return name_.data(); }

bool Device::HasChildren() const {  // return !children_.is_empty();
  return false;
}

}  // namespace mistos
