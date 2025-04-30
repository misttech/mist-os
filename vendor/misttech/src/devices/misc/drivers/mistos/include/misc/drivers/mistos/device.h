// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DEVICE_H_
#define VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DEVICE_H_

#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/mistos/util/bstring.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>

#include <fbl/mutex.h>
#include <fbl/vector.h>
#include <misc/drivers/mistos/device_server.h>
#include <misc/drivers/mistos/symbols.h>

namespace mistos {

// The DFv1 ops: zx_protocol_device_t.
constexpr char kOps[] = "compat-ops";

class Driver;

class Device {
 public:
  Device(device_t device, const zx_protocol_device_t* ops, Driver* driver,
         std::optional<Device*> parent);

  ~Device();

  zx_device_t* ZxDevice();

  const char* Name() const;
  bool HasChildren() const;

  // Functions to implement the DFv1 device API.
  zx_status_t Add(device_add_args_t* zx_args, zx_device_t** out);

  zx_status_t GetProtocol(uint32_t proto_id, void* out) const;
  zx_status_t GetFragmentProtocol(const char* fragment, uint32_t proto_id, void* out);
  zx_status_t AddMetadata(uint32_t type, const void* data, size_t size);
  zx_status_t GetMetadata(uint32_t type, void* buf, size_t buflen, size_t* actual);
  zx_status_t GetMetadataSize(uint32_t type, size_t* out_size);
  void InitReply(zx_status_t status);

  Driver* driver() { return driver_; }

  fbl::Vector<zx_device_str_prop>& properties() { return properties_; }

  const fbl::Vector<ktl::unique_ptr<Device>>& children() const { return children_; }

 private:
  Device(Device&&) = delete;
  Device& operator=(Device&&) = delete;

  zx_status_t ExportAfterInit();

  bool HasChildNamed(std::string_view name) const;

  fbl::Vector<zx_device_str_prop> properties_;

  DeviceServer device_server_;

  const mtl::BString name_;

  // A unique id for the device.
  uint32_t device_id_ = 0;

  uint32_t device_flags_ = 0;
  fbl::Vector<std::string> fragments_;

  // This device's driver. The driver owns all of its Device objects, so it
  // is garaunteed to outlive the Device.
  Driver* driver_ = nullptr;

  fbl::Mutex init_lock_;
  // bool init_is_finished_ __TA_GUARDED(init_lock_) = false;
  // zx_status_t init_status_ __TA_GUARDED(init_lock_) = ZX_OK;

  // The default protocol of the device.
  device_t compat_symbol_;
  const zx_protocol_device_t* ops_;

  std::optional<Device*> parent_;

  // The Device's children. The Device has full ownership of the children,
  // but these are shared pointers so that the NodeController can get a weak
  // pointer to the child in order to erase them.
  fbl::Vector<ktl::unique_ptr<Device>> children_;
};

}  // namespace mistos

struct zx_device : public mistos::Device {
  // NOTE: Intentionally empty, do not add to this.
};

#endif  // VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DEVICE_H_
