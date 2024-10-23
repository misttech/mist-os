// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_CONTROLLER_DPC_H_
#define ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_CONTROLLER_DPC_H_

#include <lib/fit/function.h>
#include <lib/power-management/energy-model.h>
#include <lib/power-management/power-state.h>

#include <fbl/ref_ptr.h>
#include <kernel/deadline.h>
#include <kernel/dpc.h>
#include <kernel/timer.h>

namespace power_management {

// This class provides the mechanism to escape scheduler context and enter a thread context.
// It does so by scheduling a `Timer` to fire immediately after reenabling IRQs, which then proceeds
// to queue a task in the given CPU DPC thread.
//
// In order for power management port power level controller to queue a packet in the port, it must
// acquire a mutex. We are not allowed to acquire mutexes outside of thread context, because there
// wouldn't be any thread to put in a wait queue. Each scheduler relies on `ControllerDpc` member
// that provides the storage for the Timer and DPC object.
class ControllerDpc {
 public:
  struct TransitionDetails {
    fbl::RefPtr<PowerDomain> domain;
    ktl::optional<PowerLevelUpdateRequest> request;
  };

  using TransitionDetailsProvider = fit::inline_function<TransitionDetails()>;

  explicit ControllerDpc(TransitionDetailsProvider provider)
      : provider_(ktl::move(provider)), dpc_(&Invoke, this) {}

  // If `timer_` is armed, it will be cancelled, and rearmed. DPC could be queued still.
  //
  // When `timer_` fires, we will attempt to queue the DPC. If it fails, it is already queued, and
  // that is fine.
  //
  // When the DPC executes, it will query the transition context through the provider, determine
  // if there is an actual pending transition and available means for the transition and post the
  // request.
  //
  // At this point, if there still is a pending request, we will proceed and generate a power level
  // transition request and wake the user space thread if they are waiting on the port.
  void NotifyPendingUpdate();

 private:
  // Arg is `this`.
  static void QueueDpc(Timer* t, zx_time_t now, void* arg);

  // `arg` is this.
  static void Invoke(Dpc* dpc);

  TransitionDetailsProvider provider_;
  Timer timer_;
  Dpc dpc_;
};

}  // namespace power_management

#endif  // ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_CONTROLLER_DPC_H_
