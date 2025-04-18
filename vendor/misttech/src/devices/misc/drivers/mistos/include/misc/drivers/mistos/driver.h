// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DRIVER_H_
#define VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DRIVER_H_

#include <lib/ddk/binding_priv.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <ktl/string_view.h>
#include <misc/drivers/mistos/device.h>
#include <misc/drivers/mistos/symbols.h>

namespace mistos {

Driver* CreateDriver(const zircon_driver_ldr_t* driver_loader);

class DriverBase {
 public:
  DriverBase(std::string_view name);

  DriverBase(const DriverBase&) = delete;
  DriverBase& operator=(const DriverBase&) = delete;

  // The destructor is called right after the |Stop| method.
  virtual ~DriverBase();

  // This method will be called by the factory to start the driver. This is when
  // the driver should setup the outgoing directory through `outgoing()->Add...` calls.
  // Do not call Serve, as it has already been called by the |DriverBase| constructor.
  // Child nodes can be created here synchronously or asynchronously as long as all of the
  // protocols being offered to the child has been added to the outgoing directory first.
  // There are two versions of this method which may be implemented depending on whether Start would
  // like to complete synchronously or asynchronously. The driver may override either one of these
  // methods, but must implement one. The asynchronous version will be called over the synchronous
  // version if both are implemented.
  virtual zx_status_t Start() { return ZX_ERR_NOT_SUPPORTED; }

  // This is called after all the driver dispatchers belonging to this driver have been shutdown.
  // This ensures that there are no pending tasks on any of the driver dispatchers that will access
  // the driver after it has been destroyed.
  virtual void Stop() {}

 protected:
  // The name of the driver that is given to the DriverBase constructor.
  ktl::string_view name() const { return name_; }

 private:
  ktl::string_view name_;
  // DriverStartArgs start_args_;
};

class Driver : public DriverBase {
  using Base = DriverBase;

 public:
  explicit Driver(device_t device);
  ~Driver() override;

  zx_status_t Start() override;

  // Returns the context that DFv1 driver provided.
  void* Context() const;

  zx_status_t AddDevice(Device* parent, device_add_args_t* args, zx_device_t** out);

 private:
  friend Driver* CreateDriver(const zircon_driver_ldr_t* driver_loader);

  // Loads the driver using the provided `vmos`.
  zx_status_t LoadDriver(const zircon_driver_ldr_t* loader);
  // Starts the DFv1 driver.
  zx_status_t StartDriver();

  std::string_view driver_name_;
  Device device_;

  // The next unique device id for devices. Starts at 1 because `device_` has id zero.
  uint32_t next_device_id_ = 1;

  void* library_ = nullptr;
  zx_driver_rec_t* record_ = nullptr;
  void* context_ = nullptr;
};

}  // namespace mistos

struct zx_driver : public mistos::Driver {
  // NOTE: Intentionally empty, do not add to this.
};

#endif  // VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DRIVER_H_
