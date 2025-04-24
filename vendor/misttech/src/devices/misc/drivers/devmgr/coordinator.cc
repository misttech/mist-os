// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/devmgr/coordinator.h"

#include <lib/mistos/devmgr/driver.h>

#include <fbl/auto_lock.h>

namespace devmgr {

void Coordinator::DriverAdded(fbl::RefPtr<Driver> drv, const char* version) {
  // fbl::AutoLock lock(&mutex_);
  // drivers_.push_back(drv);
}

void Coordinator::DriverAddedInit(fbl::RefPtr<Driver> drv, const char* version) {
  // fbl::AutoLock lock(&mutex_);
  // drivers_.push_back(drv);
}

// void Coordinator::StartDriver(fbl::RefPtr<Driver> driver) {
// fbl::AutoLock lock(&mutex_);
/*for (auto& drv : drivers_) {
  if (drv.driver_name_ == name) {
    return drv.Start();
  }
}*/
//}

void Coordinator::DumpState() {
  // fbl::AutoLock lock(&mutex_);
}

void Coordinator::DumpDrivers() {
  fbl::AutoLock lock(&mutex_);
  // bool first = true;
  for (const auto& drv : drivers_) {
    // printf("%sName    : %.*s\n", first ? "" : "\n", static_cast<int>(drv.name().size()),
    //        drv.name().data());
    // printf("Driver  : %.*s\n", static_cast<int>(drv.driver_name_.size()),
    // drv.driver_name_.data());
    printf("Flags   : 0x%08x\n", drv.flags_);
    if (drv.binding_size_) {
      uint32_t count = drv.binding_size_ / static_cast<uint32_t>(sizeof(drv.binding_[0]));
      printf("Binding : %u instruction%s (%u bytes)\n", count, (count == 1) ? "" : "s",
             drv.binding_size_);
      // char line[256];
      // for (uint32_t i = 0; i < count; ++i) {
      //  di_dump_bind_inst(&drv.binding[i], line, sizeof(line));
      // printf("[%u/%u]: %s\n", i + 1, count, line);
      //}
    }
    // first = false;
  }
}

}  // namespace devmgr
