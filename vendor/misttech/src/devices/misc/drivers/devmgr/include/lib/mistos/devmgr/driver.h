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
  Driver();
  ~Driver();

  // From Zircon
  // void set_name(std::string_view name) { driver_name_ = name; }
  // void set_driver_rec(const zx_driver_rec_t* driver_rec) { record_ = driver_rec; }
  void set_binding(ktl::unique_ptr<const zx_bind_inst_t[]> binding) /*__TA_EXCLUDES(lock_)*/ {
    binding_ = std::move(binding);
  }
  void set_binding_size(uint32_t binding_size) { binding_size_ = binding_size; }
  void set_flags(uint32_t flags) { flags_ = flags; }

 private:
  friend class Coordinator;

  // From Zircon
  ktl::unique_ptr<const zx_bind_inst_t[]> binding_;
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
