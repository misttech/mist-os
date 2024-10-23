// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fit/defer.h>
#include <lib/fit/function.h>
#include <lib/power-management/controller-dpc.h>
#include <lib/power-management/energy-model.h>
#include <lib/power-management/kernel-registry.h>
#include <lib/power-management/port-power-level-controller.h>
#include <lib/power-management/power-level-controller.h>
#include <lib/power-management/power-state.h>
#include <lib/unittest/unittest.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls/port.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <cstdint>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <kernel/deadline.h>
#include <kernel/event.h>
#include <kernel/thread.h>
#include <ktl/limits.h>
#include <ktl/optional.h>
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

// Fake PowerLevelController.

class FakePowerLevelController final : public power_management::PowerLevelController {
 public:
  zx::result<> Post(const PowerLevelUpdateRequest& req) final {
    count_++;
    request_ = req;
    signal_posted_.Signal(ZX_OK);
    return zx::ok();
  }

  uint64_t id() const final { return 0; }

  zx_status_t Wait(Deadline dl) {
    return signal_posted_.WaitDeadline(dl.when(), Interruptible::Yes);
  }

  auto& request() { return request_; }
  int count() const { return count_; }

 private:
  Event signal_posted_;
  int count_ = 0;
  ktl::optional<PowerLevelUpdateRequest> request_ = ktl::nullopt;
};

bool ControllerDpcPostsUpdateWhenPending() {
  BEGIN_TEST;
  // Create a fake power domain, only thing that matters is the controller.
  auto model = power_management::PowerModel();
  fbl::AllocChecker ac;
  auto controller = fbl::MakeRefCountedChecked<FakePowerLevelController>(&ac);
  ASSERT_TRUE(ac.check());
  auto domain = fbl::MakeRefCountedChecked<power_management::PowerDomain>(
      &ac, 1, zx_cpu_set_t{.mask = {1}}, ktl::move(model), controller);
  ASSERT_TRUE(ac.check());
  power_management::ControllerDpc::TransitionDetails details = {
      .domain = domain,
      .request =
          PowerLevelUpdateRequest{
              .domain_id = 1,
              .target_id = 2,
              .control = power_management::ControlInterface::kCpuDriver,
              .control_argument = 123,
              .options = 3,
          },
  };

  power_management::ControllerDpc controller_dpc([&details]() { return details; });
  controller_dpc.NotifyPendingUpdate();
  ASSERT_EQ(controller->Wait(Deadline::infinite()), ZX_OK);
  ASSERT_EQ(controller->count(), 1);

  auto req = controller->request();
  ASSERT_TRUE(req);
  EXPECT_EQ(req->control, details.request->control);
  EXPECT_EQ(req->control_argument, details.request->control_argument);
  EXPECT_EQ(req->domain_id, details.request->domain_id);
  EXPECT_EQ(req->options, details.request->options);
  EXPECT_EQ(req->target_id, details.request->target_id);

  END_TEST;
}

bool ControllerDpcTryQueueManyTimesIsOk() {
  BEGIN_TEST;
  if (true) {
    // TODO(https://fxbug.dev/375221894): Disabled until underlying issue is fixed.
    printf("Test disabled due to https://fxbug.dev/375221894\n");
    END_TEST;
  }

  // Create a fake power domain, only thing that matters is the controller.
  auto model = power_management::PowerModel();
  fbl::AllocChecker ac;
  auto controller = fbl::MakeRefCountedChecked<FakePowerLevelController>(&ac);
  ASSERT_TRUE(ac.check());
  auto domain = fbl::MakeRefCountedChecked<power_management::PowerDomain>(
      &ac, 1, zx_cpu_set_t{.mask = {1}}, ktl::move(model), controller);
  ASSERT_TRUE(ac.check());
  power_management::ControllerDpc::TransitionDetails details = {
      .domain = domain,
      .request =
          PowerLevelUpdateRequest{
              .domain_id = 1,
              .target_id = 2,
              .control = power_management::ControlInterface::kCpuDriver,
              .control_argument = 123,
              .options = 3,
          },
  };

  power_management::ControllerDpc controller_dpc([&details]() { return details; });
  for (size_t i = 0; i < 150; ++i) {
    controller_dpc.NotifyPendingUpdate();
  }
  ASSERT_EQ(controller->Wait(Deadline::infinite()), ZX_OK);

  ASSERT_GE(controller->count(), 1);
  auto req = controller->request();
  ASSERT_TRUE(req);
  EXPECT_EQ(req->control, details.request->control);
  EXPECT_EQ(req->control_argument, details.request->control_argument);
  EXPECT_EQ(req->domain_id, details.request->domain_id);
  EXPECT_EQ(req->options, details.request->options);
  EXPECT_EQ(req->target_id, details.request->target_id);

  END_TEST;
}

bool ControllerDpcNoPendingRequestWhenNoneAvailable() {
  BEGIN_TEST;
  // Create a fake power domain, only thing that matters is the controller.
  auto model = power_management::PowerModel();
  fbl::AllocChecker ac;
  auto controller = fbl::MakeRefCountedChecked<FakePowerLevelController>(&ac);
  ASSERT_TRUE(ac.check());
  auto domain = fbl::MakeRefCountedChecked<power_management::PowerDomain>(
      &ac, 1, zx_cpu_set_t{.mask = {1}}, ktl::move(model), controller);
  ASSERT_TRUE(ac.check());
  power_management::ControllerDpc::TransitionDetails details = {
      .domain = domain,
      .request = ktl::nullopt,
  };

  power_management::ControllerDpc controller_dpc([&details]() { return details; });
  controller_dpc.NotifyPendingUpdate();
  ASSERT_EQ(controller->Wait(Deadline::after(zx_duration_from_msec(30))), ZX_ERR_TIMED_OUT);
  ASSERT_GE(controller->count(), 0);

  END_TEST;
}

// Essentially a Timer that arms the timer. Close as we can get.
bool ControllerDpcTryQueueFromIrqContext() {
  BEGIN_TEST;
  // Create a fake power domain, only thing that matters is the controller.
  auto model = power_management::PowerModel();
  fbl::AllocChecker ac;
  auto controller = fbl::MakeRefCountedChecked<FakePowerLevelController>(&ac);
  ASSERT_TRUE(ac.check());
  auto domain = fbl::MakeRefCountedChecked<power_management::PowerDomain>(
      &ac, 1, zx_cpu_set_t{.mask = {1}}, ktl::move(model), controller);
  ASSERT_TRUE(ac.check());
  power_management::ControllerDpc::TransitionDetails details = {
      .domain = domain,
      .request =
          PowerLevelUpdateRequest{
              .domain_id = 1,
              .target_id = 2,
              .control = power_management::ControlInterface::kCpuDriver,
              .control_argument = 123,
              .options = 3,
          },
  };

  power_management::ControllerDpc controller_dpc([&details]() { return details; });

  static auto try_queue_from_irq_details = [](Timer* timer, zx_time_t now, void* arg) {
    auto* controller_dpc = static_cast<power_management::ControllerDpc*>(arg);
    controller_dpc->NotifyPendingUpdate();
  };

  Timer try_queue_timer;
  try_queue_timer.Set(Deadline::after(ZX_TIME_INFINITE_PAST), try_queue_from_irq_details,
                      &controller_dpc);
  ASSERT_EQ(controller->Wait(Deadline::infinite()), ZX_OK);
  ASSERT_EQ(controller->count(), 1);
  auto req = controller->request();
  ASSERT_TRUE(req);
  EXPECT_EQ(req->control, details.request->control);
  EXPECT_EQ(req->control_argument, details.request->control_argument);
  EXPECT_EQ(req->domain_id, details.request->domain_id);
  EXPECT_EQ(req->options, details.request->options);
  EXPECT_EQ(req->target_id, details.request->target_id);
  END_TEST;
}

UNITTEST_START_TESTCASE(pm_controller)
UNITTEST("Controller queues port packet", PortPowerLevelControllerPost)
UNITTEST("Controller stops serving when zero handles reached",
         PortPowerLevelControllerStopServingOnZeroHandles)
UNITTEST("Controller DPC queues task.", ControllerDpcTryQueueManyTimesIsOk)
UNITTEST("Controller DPC TryQueue called from IRQ context.", ControllerDpcTryQueueFromIrqContext)
UNITTEST("Controller DPC its ok to be called many times.", ControllerDpcPostsUpdateWhenPending)
UNITTEST("Controller DPC wont queue in DPC thread if no requests available.",
         ControllerDpcNoPendingRequestWhenNoneAvailable)
UNITTEST_END_TESTCASE(pm_controller, "pm_port_controller",
                      "Validates power management port based power level controller.")

}  // namespace
