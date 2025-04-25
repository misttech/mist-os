// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_DEVMGR_INCLUDE_LIB_MISTOS_DEVMGR_DRIVER_H_
#define VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_DEVMGR_INCLUDE_LIB_MISTOS_DEVMGR_DRIVER_H_

#include <lib/ddk/binding.h>
#include <lib/ddk/driver.h>
#include <lib/fit/function.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <fbl/auto_lock.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/mutex.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <ktl/unique_ptr.h>

namespace devmgr {

using DriverHooks = const zx_driver_rec_t*;

class Driver : public fbl::RefCounted<Driver>,
               public fbl::DoublyLinkedListable<fbl::RefPtr<Driver>> {
 public:
  Driver(std::string_view url, void* library, DriverHooks hooks);
  ~Driver();

  // Starts the driver.
  void Start(fbl::RefPtr<Driver> self, /*fuchsia_driver_framework::DriverStartArgs start_args,
             fdf::Dispatcher dispatcher,*/
             fit::callback<void(zx::result<>)> cb) /*__TA_EXCLUDES(lock_)*/;

  const std::string_view& url() const { return url_; }
  void set_binding(ktl::unique_ptr<const zx_bind_inst_t[]> binding) /*__TA_EXCLUDES(lock_)*/;
  const zx_bind_inst_t* binding() const /*__TA_EXCLUDES(lock_)*/ { return binding_->get(); }

  void set_binding_size(uint32_t binding_size) { binding_size_ = binding_size; }
  uint32_t binding_size() const { return binding_size_; }

 private:
  friend class Coordinator;
  std::string_view url_;
  void* library_;

  // fbl::Mutex lock_;

  // The hooks to initialize and destroy the driver. Backed by the registration symbol.
  DriverHooks hooks_ /*__TA_GUARDED(lock_)*/;

  std::optional<ktl::unique_ptr<const zx_bind_inst_t[]>> binding_ /*__TA_GUARDED(lock_)*/;

  // Binding size in number of bytes, not number of entries
  // TODO: Change it to number of entries
  uint32_t binding_size_ = 0;
  uint32_t flags_ = 0;
};

using DriverLoadCallback = fit::function<void(fbl::RefPtr<Driver> driver, const char* version)>;

void load_driver(const char* name, DriverLoadCallback func);
void find_loadable_drivers(const void* start, const void* end, DriverLoadCallback func);

}  // namespace devmgr

#endif  // VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_DEVMGR_INCLUDE_LIB_MISTOS_DEVMGR_DRIVER_H_
