// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "driver_host_context.h"

#include <stdio.h>
#include <zircon/status.h>

#include <fbl/auto_lock.h>

DriverHostContext::~DriverHostContext() {
  while (!dead_devices_.is_empty()) {
    delete dead_devices_.pop_front();
  }
}

void DriverHostContext::AddDriver(fbl::RefPtr<dfv2::Driver> driver) {
  dfv2_drivers_.push_back(std::move(driver));
}

void DriverHostContext::RemoveDriver(dfv2::Driver& driver) { dfv2_drivers_.erase(driver); }
