// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fit/defer.h>
#include <lib/power-management/energy-model.h>
#include <lib/power-management/kernel-registry.h>
#include <lib/power-management/port-power-level-controller.h>
#include <lib/power-management/power-state.h>
#include <lib/unittest/unittest.h>
#include <zircon/rights.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls/port.h>

#include <fbl/ref_ptr.h>
#include <kernel/deadline.h>
#include <ktl/limits.h>
#include <object/handle.h>
#include <object/port_dispatcher.h>

namespace {

using power_management::PortPowerLevelController;
using power_management::PowerLevelUpdateRequest;

bool PortPowerLevelControllerPost() {
  BEGIN_TEST;
  KernelHandle<PortDispatcher> h;
  zx_rights_t r = PortDispatcher::default_rights();
  ASSERT_EQ(PortDispatcher::Create(0, &h, &r), ZX_OK);
  PortPowerLevelController proxy(h.dispatcher());
  ASSERT_TRUE(proxy.is_serving());
  ASSERT_EQ(h.dispatcher()->current_handle_count(), 0u);
  // Fake being owned by a process. We only care about the handle count.
  auto decrese_handle_count = fit::defer([&]() { h.dispatcher()->decrement_handle_count(); });
  h.dispatcher()->increment_handle_count();
  PowerLevelUpdateRequest req = {
      .domain_id = 0xFEE7,
      .target_id = 0x0B00,
      .control = power_management::ControlInterface::kCpuDriver,
      .control_argument = 0xF00D,
      .options = 4321,
  };

  ASSERT_TRUE(proxy.Post(req).is_ok());

  // Check a port with the domain id as key was queued.
  ASSERT_TRUE(h.dispatcher()->CancelQueued(nullptr, req.domain_id));
  END_TEST;
}

bool PortPowerLevelControllerStopServingOnZeroHandles() {
  BEGIN_TEST;
  KernelHandle<PortDispatcher> h;
  zx_rights_t r = PortDispatcher::default_rights();
  ASSERT_EQ(PortDispatcher::Create(0, &h, &r), ZX_OK);
  PortPowerLevelController proxy(h.dispatcher());
  ASSERT_TRUE(proxy.is_serving());
  ASSERT_EQ(h.dispatcher()->current_handle_count(), 0u);
  // Fake being owned by a process. We only care about the handle count.
  PowerLevelUpdateRequest req = {
      .domain_id = 0xFEE7,
      .target_id = 0x0B00,
      .control = power_management::ControlInterface::kCpuDriver,
      .control_argument = 0xF00D,
      .options = 4321,
  };

  ASSERT_TRUE(proxy.Post(req).is_error());
  ASSERT_FALSE(proxy.is_serving());
  END_TEST;
}

UNITTEST_START_TESTCASE(pm_controller)
UNITTEST("Controller queues port packet", PortPowerLevelControllerPost)
UNITTEST("Controller stops serving when zero handles reached",
         PortPowerLevelControllerStopServingOnZeroHandles)
UNITTEST_END_TESTCASE(pm_controller, "pm_port_controller",
                      "Validates power management port based power level controller.")

}  // namespace
