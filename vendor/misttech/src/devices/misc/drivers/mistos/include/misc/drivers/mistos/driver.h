// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DRIVER_H_
#define VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DRIVER_H_

#include <lib/ddk/binding_priv.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <fbl/auto_lock.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/mutex.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <ktl/string_view.h>
#include <misc/drivers/mistos/device.h>
#include <misc/drivers/mistos/symbols.h>

#include "misc/drivers/mistos/device.h"

namespace mistos {

/*Driver* CreateDriver(const zircon_driver_note_t* note, zx_driver_rec_t* rec,
                     ktl::unique_ptr<const zx_bind_inst_t[]> binding);*/

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
  virtual zx::result<> Start() { return zx::error(ZX_ERR_NOT_SUPPORTED); }
  // virtual void Start(StartCompleter completer) { completer(Start()); }

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

class Driver : public fbl::DoublyLinkedListable<Driver*>, public DriverBase {
  using Base = DriverBase;

 public:
  Driver(device_t device, const zx_protocol_device_t* ops);
  ~Driver() override;

  zx::result<> Start() override;

  // Returns the context that DFv1 driver provided.
  void* Context() const;

  void* GetInfoResource();

  void* GetIommuResource();

  void* GetMmioResource();

  void* GetMsiResource();

  void* GetPowerResource();

  void* GetIoportResource();

  void* GetIrqResource();

  void* GetSmcResource();

  // zx_status_t GetProperties(device_props_args_t* out_args,
  //                           const std::string& parent_node_name = "default");

  zx_status_t AddDevice(Device* parent, device_add_args_t* args, zx_device_t** out);

  zx_status_t GetProtocol(uint32_t proto_id, void* out);
  zx_status_t GetFragmentProtocol(const char* fragment, uint32_t proto_id, void* out);

  Device& GetDevice() { return device_; }

  void CompleteStart(zx::result<> result);

  uint32_t GetNextDeviceId() { return next_device_id_++; }

 private:
  friend class Coordinator;
  /*friend Driver* CreateDriver(const zircon_driver_note_t* note, zx_driver_rec_t* rec,
                              ktl::unique_ptr<const zx_bind_inst_t[]> binding);*/

  // Loads the driver using the provided `vmos`.
  zx::result<> LoadDriver();

  // Starts the DFv1 driver.
  zx::result<> StartDriver();

  std::string_view driver_name_;
  Device device_;

  // The next unique device id for devices. Starts at 1 because `device_` has id zero.
  uint32_t next_device_id_ = 1;

  // void* library_ = nullptr;
  const zx_driver_rec_t* record_ = nullptr;
  void* context_ = nullptr;
};

}  // namespace mistos

struct zx_driver : public mistos::Driver {
  // NOTE: Intentionally empty, do not add to this.
};

#endif  // VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DRIVER_H_
