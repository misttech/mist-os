// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fit/defer.h>
#include <lib/fit/function.h>
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

#include <arch/ops.h>
#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <kernel/cpu.h>
#include <kernel/deadline.h>
#include <kernel/event.h>
#include <kernel/thread.h>
#include <ktl/limits.h>
#include <ktl/optional.h>
#include <object/handle.h>
#include <object/port_dispatcher.h>

namespace {

using power_management::ControlInterface;
using power_management::EnergyModel;
using power_management::PortPowerLevelController;
using power_management::PowerDomain;
using power_management::PowerLevelController;
using power_management::PowerLevelUpdateRequest;

constexpr uint8_t kIdlePowerLevel = 0;
constexpr uint8_t kLowPowerLevel = 1;
constexpr uint8_t kMediumPowerLevel = 2;
constexpr uint8_t kHighPowerLevel = 3;
constexpr uint8_t kMaxPowerLevel = 4;

constexpr auto kPowerLevels = cpp20::to_array<zx_processor_power_level_t>({
    {
        .options = 0,
        .processing_rate = 0,                // 0%
        .power_coefficient_nw = 10'000'000,  // 10 mW
        .control_interface = cpp23::to_underlying(ControlInterface::kArmWfi),
        .control_argument = kIdlePowerLevel,
        .diagnostic_name = "wfi",
    },
    {
        .options = 0,
        .processing_rate = 250,               // 25%
        .power_coefficient_nw = 100'000'000,  // 100 mW
        .control_interface = cpp23::to_underlying(ControlInterface::kCpuDriver),
        .control_argument = kLowPowerLevel,
        .diagnostic_name = "OPP 1",
    },
    {
        .options = 0,
        .processing_rate = 500,               // 50%
        .power_coefficient_nw = 250'000'000,  // 250 mW
        .control_interface = cpp23::to_underlying(ControlInterface::kCpuDriver),
        .control_argument = kMediumPowerLevel,
        .diagnostic_name = "OPP 1",
    },
    {
        .options = 0,
        .processing_rate = 750,               // 75%
        .power_coefficient_nw = 400'000'000,  // 400 mW
        .control_interface = cpp23::to_underlying(ControlInterface::kCpuDriver),
        .control_argument = kHighPowerLevel,
        .diagnostic_name = "OPP 2",
    },
    {
        .options = 0,
        .processing_rate = 1000,              // 100%
        .power_coefficient_nw = 600'000'000,  // 600 mW
        .control_interface = cpp23::to_underlying(ControlInterface::kCpuDriver),
        .control_argument = kMaxPowerLevel,
        .diagnostic_name = "OPP 3",
    },
});

constexpr auto kTransitions = cpp20::to_array<zx_processor_power_level_transition_t>({{}});

bool PortPowerLevelControllerPost() {
  BEGIN_TEST;

  KernelHandle<PortDispatcher> handle;
  zx_rights_t rights = PortDispatcher::default_rights();
  ASSERT_EQ(PortDispatcher::Create(0, &handle, &rights), ZX_OK);

  PortPowerLevelController controller(handle.dispatcher());
  ASSERT_TRUE(controller.is_serving());
  ASSERT_EQ(handle.dispatcher()->current_handle_count(), 0u);

  // Fake being owned by a process. We only care about the handle count.
  auto decrese_handle_count = fit::defer([&]() { handle.dispatcher()->decrement_handle_count(); });
  handle.dispatcher()->increment_handle_count();

  PowerLevelUpdateRequest request = {
      .domain_id = 0xFEE7,
      .target_id = 0x0B00,
      .control = ControlInterface::kCpuDriver,
      .control_argument = 0xF00D,
      .options = 4321,
  };

  ASSERT_TRUE(controller.Post(request).is_ok());

  // Check a port with the domain id as key was queued.
  ASSERT_TRUE(handle.dispatcher()->CancelQueued(nullptr, request.domain_id));

  END_TEST;
}

bool PortPowerLevelControllerStopServingOnZeroHandles() {
  BEGIN_TEST;

  KernelHandle<PortDispatcher> handle;
  zx_rights_t rights = PortDispatcher::default_rights();
  ASSERT_EQ(PortDispatcher::Create(0, &handle, &rights), ZX_OK);

  PortPowerLevelController controller(handle.dispatcher());
  ASSERT_TRUE(controller.is_serving());
  ASSERT_EQ(handle.dispatcher()->current_handle_count(), 0u);

  // Fake being owned by a process. We only care about the handle count.
  PowerLevelUpdateRequest request = {
      .domain_id = 0xFEE7,
      .target_id = 0x0B00,
      .control = ControlInterface::kCpuDriver,
      .control_argument = 0xF00D,
      .options = 4321,
  };

  ASSERT_TRUE(controller.Post(request).is_error());
  ASSERT_FALSE(controller.is_serving());

  END_TEST;
}

class FakePowerLevelController final : public PowerLevelController {
 public:
  zx::result<> Post(const PowerLevelUpdateRequest& request) final {
    count_++;
    request_ = request;
    signal_posted_.Signal(ZX_OK);
    return zx::ok();
  }

  uint64_t id() const final { return 0; }

  zx_status_t Wait(Deadline deadline) {
    return signal_posted_.WaitDeadline(deadline.when(), Interruptible::Yes);
  }

  auto& request() { return request_; }
  size_t count() const { return count_; }

 private:
  AutounsignalEvent signal_posted_;
  ktl::atomic<size_t> count_ = 0;
  ktl::optional<PowerLevelUpdateRequest> request_ = ktl::nullopt;
};

bool SchedulerFlushesPendingControlRequests() {
  BEGIN_TEST;

  // Use the scheduler for the current CPU to test the power level control functionality. It doesn't
  // matter if the test thread remains on the same CPU, since power level requests can be initiated
  // from a different CPU than the target CPU.
  Scheduler& scheduler = percpu::GetCurrent().scheduler;

  zx::result energy_model = EnergyModel::Create(kPowerLevels, kTransitions);
  ASSERT_TRUE(energy_model.is_ok());

  fbl::AllocChecker ac;
  fbl::RefPtr controller = fbl::MakeRefCountedChecked<FakePowerLevelController>(&ac);
  ASSERT_TRUE(ac.check());

  fbl::RefPtr domain = fbl::MakeRefCountedChecked<PowerDomain>(
      &ac, 1, zx_cpu_set_t{.mask = {cpu_num_to_mask(scheduler.this_cpu())}},
      ktl::move(*energy_model), controller);
  ASSERT_TRUE(ac.check());

  // Set the current power domain, saving the previous domain to restore at the end of the test.
  const ktl::optional<uint8_t> restore_power_level = scheduler.GetPowerLevel();
  auto restore_domain = fit::defer([&, previous_domian = scheduler.ExchangePowerDomain(domain)] {
    scheduler.ExchangePowerDomain(previous_domian);
    if (restore_power_level.has_value()) {
      DEBUG_ASSERT(scheduler.SetPowerLevel(*restore_power_level).is_ok());
    }
  });
  ASSERT_EQ(domain.get(), scheduler.GetPowerDomainForTesting().get());
  ASSERT_OK(scheduler.SetPowerLevel(kMaxPowerLevel).status_value());

  // Request a transition to each active power level.
  for (size_t count = 0;
       uint8_t power_level : {kLowPowerLevel, kMediumPowerLevel, kHighPowerLevel, kMaxPowerLevel}) {
    scheduler.RequestPowerLevelForTesting(power_level);

    ASSERT_EQ(controller->Wait(Deadline::infinite()), ZX_OK);
    ASSERT_EQ(controller->count(), ++count);

    ASSERT_TRUE(controller->request());
    EXPECT_EQ(controller->request()->control, ControlInterface::kCpuDriver);
    EXPECT_EQ(controller->request()->control_argument, power_level);
    EXPECT_EQ(controller->request()->domain_id, domain->id());
    EXPECT_EQ(controller->request()->options, 0u);
    EXPECT_EQ(controller->request()->target_id, domain->id());

    // Simulate the controller acking the transition. Failing to ack the transition will cause this
    // loop to get stuck in FakePowerLevelController::Wait if the requested transition matches the
    // current power level (i.e. kMaxPowerLevel set above), since redundant requests are dropped.
    ASSERT_OK(scheduler.SetPowerLevel(power_level).status_value());

    controller->request().reset();
  }

  // Requesting the same power level as the current power level should be ignored.
  scheduler.RequestPowerLevelForTesting(kMaxPowerLevel);
  EXPECT_EQ(controller->Wait(Deadline::after_mono(zx_duration_from_sec(1))), ZX_ERR_TIMED_OUT);

  END_TEST;
}

bool SchedulerElidesPendingControlRequests() {
  BEGIN_TEST;

  // Use the scheduler for the current CPU to test the power level control functionality. It doesn't
  // matter if the test thread remains on the same CPU, since power level requests can be initiated
  // from a different CPU than the target CPU.
  Scheduler& scheduler = percpu::GetCurrent().scheduler;

  zx::result energy_model = EnergyModel::Create(kPowerLevels, kTransitions);
  ASSERT_TRUE(energy_model.is_ok());

  fbl::AllocChecker ac;
  fbl::RefPtr controller = fbl::MakeRefCountedChecked<FakePowerLevelController>(&ac);
  ASSERT_TRUE(ac.check());

  fbl::RefPtr domain = fbl::MakeRefCountedChecked<PowerDomain>(
      &ac, 1, zx_cpu_set_t{.mask = {cpu_num_to_mask(scheduler.this_cpu())}},
      ktl::move(*energy_model), controller);
  ASSERT_TRUE(ac.check());

  // Set the current power domain, saving the previous domain to restore at the end of the test.
  const ktl::optional<uint8_t> restore_power_level = scheduler.GetPowerLevel();
  auto restore_domain = fit::defer([&, previous_domian = scheduler.ExchangePowerDomain(domain)] {
    scheduler.ExchangePowerDomain(previous_domian);
    if (restore_power_level.has_value()) {
      DEBUG_ASSERT(scheduler.SetPowerLevel(*restore_power_level).is_ok());
    }
  });
  ASSERT_EQ(domain.get(), scheduler.GetPowerDomainForTesting().get());
  ASSERT_OK(scheduler.SetPowerLevel(kMaxPowerLevel).status_value());

  for (size_t i = 0; i < 150; ++i) {
    scheduler.RequestPowerLevelForTesting(kHighPowerLevel);
  }

  ASSERT_EQ(controller->Wait(Deadline::infinite()), ZX_OK);
  ASSERT_GE(controller->count(), 1u);

  ASSERT_TRUE(controller->request());
  EXPECT_EQ(controller->request()->control, ControlInterface::kCpuDriver);
  EXPECT_EQ(controller->request()->control_argument, kHighPowerLevel);
  EXPECT_EQ(controller->request()->domain_id, domain->id());
  EXPECT_EQ(controller->request()->options, 0u);
  EXPECT_EQ(controller->request()->target_id, domain->id());

  END_TEST;
}

bool SchedulerCanPendControlRequestsInIrqContext() {
  BEGIN_TEST;
  // Use the scheduler for the current CPU to test the power level control functionality. It doesn't
  // matter if the test thread remains on the same CPU, since power level requests can be initiated
  // from a different CPU than the target CPU.
  Scheduler& scheduler = percpu::GetCurrent().scheduler;

  zx::result energy_model = EnergyModel::Create(kPowerLevels, kTransitions);
  ASSERT_TRUE(energy_model.is_ok());

  fbl::AllocChecker ac;
  fbl::RefPtr controller = fbl::MakeRefCountedChecked<FakePowerLevelController>(&ac);
  ASSERT_TRUE(ac.check());

  fbl::RefPtr domain = fbl::MakeRefCountedChecked<PowerDomain>(
      &ac, 1, zx_cpu_set_t{.mask = {cpu_num_to_mask(scheduler.this_cpu())}},
      ktl::move(*energy_model), controller);
  ASSERT_TRUE(ac.check());

  // Set the current power domain, saving the previous domain to restore at the end of the test.
  const ktl::optional<uint8_t> restore_power_level = scheduler.GetPowerLevel();
  auto restore_domain = fit::defer([&, previous_domian = scheduler.ExchangePowerDomain(domain)] {
    scheduler.ExchangePowerDomain(previous_domian);
    if (restore_power_level.has_value()) {
      DEBUG_ASSERT(scheduler.SetPowerLevel(*restore_power_level).is_ok());
    }
  });
  ASSERT_EQ(domain.get(), scheduler.GetPowerDomainForTesting().get());
  ASSERT_OK(scheduler.SetPowerLevel(kMaxPowerLevel).status_value());

  auto timer_handler = +[](Timer* timer, zx_instant_mono_t now, void* arg) {
    Scheduler* scheduler = static_cast<Scheduler*>(arg);
    scheduler->RequestPowerLevelForTesting(kHighPowerLevel);
  };

  Timer timer;
  timer.Set(Deadline::infinite_past(), timer_handler, &scheduler);

  ASSERT_EQ(controller->Wait(Deadline::infinite()), ZX_OK);
  ASSERT_EQ(controller->count(), 1u);

  ASSERT_TRUE(controller->request());
  EXPECT_EQ(controller->request()->control, ControlInterface::kCpuDriver);
  EXPECT_EQ(controller->request()->control_argument, kHighPowerLevel);
  EXPECT_EQ(controller->request()->domain_id, domain->id());
  EXPECT_EQ(controller->request()->options, 0u);
  EXPECT_EQ(controller->request()->target_id, domain->id());

  END_TEST;
}

bool SchedulerCanPendControlRequestsAcrossCpus() {
  BEGIN_TEST;

  if (arch_max_num_cpus() < 2) {
    printf("Skipping test that requires more than one CPU.\n");
    END_TEST;
  }

  // Use the scheduler for the current CPU to test the power level control functionality. It doesn't
  // matter if the test thread remains on the same CPU, since power level requests can be initiated
  // from a different CPU than the target CPU.
  Scheduler& scheduler = percpu::GetCurrent().scheduler;

  zx::result energy_model = EnergyModel::Create(kPowerLevels, kTransitions);
  ASSERT_TRUE(energy_model.is_ok());

  fbl::AllocChecker ac;
  fbl::RefPtr controller = fbl::MakeRefCountedChecked<FakePowerLevelController>(&ac);
  ASSERT_TRUE(ac.check());

  fbl::RefPtr domain = fbl::MakeRefCountedChecked<PowerDomain>(
      &ac, 1, zx_cpu_set_t{.mask = {cpu_num_to_mask(scheduler.this_cpu())}},
      ktl::move(*energy_model), controller);
  ASSERT_TRUE(ac.check());

  // Set the current power domain, saving the previous domain to restore at the end of the test.
  const ktl::optional<uint8_t> restore_power_level = scheduler.GetPowerLevel();
  auto restore_domain = fit::defer([&, previous_domian = scheduler.ExchangePowerDomain(domain)] {
    scheduler.ExchangePowerDomain(previous_domian);
    if (restore_power_level.has_value()) {
      DEBUG_ASSERT(scheduler.SetPowerLevel(*restore_power_level).is_ok());
    }
  });
  ASSERT_EQ(domain.get(), scheduler.GetPowerDomainForTesting().get());
  ASSERT_OK(scheduler.SetPowerLevel(kMaxPowerLevel).status_value());

  // Move the test thread to a different CPU than the target scheduler serves.
  const cpu_mask_t temporary_affinity =
      Scheduler::PeekActiveMask() & ~cpu_num_to_mask(scheduler.this_cpu());
  auto restore_affinity =
      fit::defer([previous_affinity = Thread::Current::Get()->SetCpuAffinity(temporary_affinity)] {
        Thread::Current::Get()->SetCpuAffinity(previous_affinity);
      });
  ASSERT_NE(scheduler.this_cpu(), arch_curr_cpu_num());

  scheduler.RequestPowerLevelForTesting(kHighPowerLevel);

  ASSERT_EQ(controller->Wait(Deadline::infinite()), ZX_OK);
  ASSERT_EQ(controller->count(), 1u);

  ASSERT_TRUE(controller->request());
  EXPECT_EQ(controller->request()->control, ControlInterface::kCpuDriver);
  EXPECT_EQ(controller->request()->control_argument, kHighPowerLevel);
  EXPECT_EQ(controller->request()->domain_id, domain->id());
  EXPECT_EQ(controller->request()->options, 0u);
  EXPECT_EQ(controller->request()->target_id, domain->id());

  END_TEST;
}

UNITTEST_START_TESTCASE(pm_controller)
UNITTEST("Port controller queues a packet.", PortPowerLevelControllerPost)
UNITTEST("Port controller stops serving when there are zero handles.",
         PortPowerLevelControllerStopServingOnZeroHandles)
UNITTEST("Scheduler flushes pending requests to the controller.",
         SchedulerFlushesPendingControlRequests)
UNITTEST("Scheduler elides multiple pending requests to the controller.",
         SchedulerElidesPendingControlRequests)
UNITTEST("Scheduler control requests may occur in IRQ context.",
         SchedulerCanPendControlRequestsInIrqContext)
UNITTEST("Scheduler control requests may pend from a different CPU.",
         SchedulerCanPendControlRequestsAcrossCpus)
UNITTEST_END_TESTCASE(pm_controller, "pm_controller", "Kernel CPU power level controller tests.")

}  // namespace
