// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SUSPEND_DRIVERS_AML_SUSPEND_TESTS_AML_SUSPEND_FOR_TESTS_H_
#define SRC_DEVICES_SUSPEND_DRIVERS_AML_SUSPEND_TESTS_AML_SUSPEND_FOR_TESTS_H_

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/inspect/contrib/cpp/bounded_list_node.h>
#include <lib/inspect/cpp/inspect.h>

#include <sdk/lib/driver/outgoing/cpp/outgoing_directory.h>

#include "src/devices/suspend/drivers/aml-suspend/aml-suspend.h"
#include "src/devices/testing/syscall-intercept/syscall-intercept.h"

namespace suspend {

class AmlSuspendForTests : public suspend::AmlSuspend {
 public:
  using AmlSuspend::AmlSuspend;

 protected:
  void AtStart() override;

 private:
  std::unique_ptr<syscall_intercept::SuspendObserver> suspend_observer_;
};

}  // namespace suspend

#endif  // SRC_DEVICES_SUSPEND_DRIVERS_AML_SUSPEND_TESTS_AML_SUSPEND_FOR_TESTS_H_
