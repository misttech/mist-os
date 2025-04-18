// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DEVICE_H_
#define VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DEVICE_H_

#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>

#include <fbl/mutex.h>
#include <fbl/vector.h>
#include <misc/drivers/mistos/symbols.h>

namespace mistos {

class Driver;

class Device {
 public:
  Device(device_t device, Driver* driver);
  ~Device() = default;

  zx_device_t* ZxDevice();

  const char* Name() const;
  bool HasChildren() const;

  zx_status_t Add(device_add_args_t* zx_args, zx_device_t** out);

  Driver* driver() { return driver_; }

 private:
  Device(Device&&) = delete;
  Device& operator=(Device&&) = delete;

  const std::string_view name_;
  // A unique id for the device.
  uint32_t device_id_ = 0;

  // This device's driver. The driver owns all of its Device objects, so it
  // is garaunteed to outlive the Device.
  Driver* driver_ = nullptr;

  fbl::Mutex init_lock_;
  bool init_is_finished_ __TA_GUARDED(init_lock_) = false;
  zx_status_t init_status_ __TA_GUARDED(init_lock_) = ZX_OK;

  // The default protocol of the device.
  device_t compat_symbol_;
  const zx_protocol_device_t* ops_;

  // The Device's children. The Device has full ownership of the children,
  // but these are shared pointers so that the NodeController can get a weak
  // pointer to the child in order to erase them.
  //fbl::Vector<fbl::shared_ptr<Device>> children_;
};

}  // namespace mistos

struct zx_device : public mistos::Device {
  // NOTE: Intentionally empty, do not add to this.
};

#endif  // VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DEVICE_H_
