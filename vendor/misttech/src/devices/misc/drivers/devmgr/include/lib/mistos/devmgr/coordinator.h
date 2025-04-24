// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_DEVMGR_INCLUDE_LIB_MISTOS_DEVMGR_COORDINATOR_H_
#define VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_DEVMGR_INCLUDE_LIB_MISTOS_DEVMGR_COORDINATOR_H_

#include <fbl/intrusive_double_list.h>
#include <fbl/mutex.h>
#include <fbl/ref_counted.h>

namespace devmgr {

class Driver;
class Coordinator {
 public:
  Coordinator() = default;

  void DriverAdded(fbl::RefPtr<Driver> drv, const char* version);
  void DriverAddedInit(fbl::RefPtr<Driver> drv, const char* version);

  void DumpDrivers();
  void DumpState();

  //void StartDriver(fbl::RefPtr<Driver> driver);
  // zx::result<> StartRootDriver(std::string_view name);

 private:
  // All Drivers
  fbl::Mutex mutex_;
  fbl::DoublyLinkedList<fbl::RefPtr<Driver>> drivers_ __TA_GUARDED(mutex_);
};

}  // namespace devmgr

#endif  // VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_DEVMGR_INCLUDE_LIB_MISTOS_DEVMGR_COORDINATOR_H_
