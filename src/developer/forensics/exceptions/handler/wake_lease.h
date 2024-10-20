// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_EXCEPTIONS_HANDLER_WAKE_LEASE_H_
#define SRC_DEVELOPER_FORENSICS_EXCEPTIONS_HANDLER_WAKE_LEASE_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/fpromise/barrier.h>
#include <lib/fpromise/promise.h>
#include <lib/fpromise/scope.h>

#include <string>
#include <vector>

#include "src/developer/forensics/utils/errors.h"
#include "src/developer/forensics/utils/fidl_event_handler.h"

namespace forensics::exceptions::handler {

class WakeLeaseBase {
 public:
  virtual ~WakeLeaseBase() = default;

  virtual fpromise::promise<fidl::Client<::fuchsia_power_broker::LeaseControl>, Error> Acquire(
      zx::duration timeout) = 0;
};

// Adds a power element opportunistically dependent on (ExecutionState, Suspending) and takes a
// lease on that element.
class WakeLease : public WakeLeaseBase {
 public:
  WakeLease(async_dispatcher_t* dispatcher, const std::string& power_element_name,
            fidl::ClientEnd<fuchsia_power_system::ActivityGovernor> sag_client_end,
            fidl::ClientEnd<fuchsia_power_broker::Topology> topology_client_end);

  // Acquires a lease on a power element that opportunistically depends on (Execution State,
  // Suspending). Note, the power element is added automatically when Acquire is called for the
  // first time.
  //
  // The promise returned needs to be scheduled on an executor and will complete ok with the power
  // lease channel if successful. If there is an error, the promise will return an error indicating
  // why, e.g., the timeout has been hit and the lease couldn't be acquired.
  //
  // This function can be called many times. If the lease returned falls out of scope, the lease
  // will be dropped and can be later reacquired.
  fpromise::promise<fidl::Client<::fuchsia_power_broker::LeaseControl>, Error> Acquire(
      zx::duration timeout) override;

 private:
  // "Unsafe" because it does not have scoping to prevent |this| from being accessed in promise
  // continutations.
  fpromise::promise<fidl::Client<::fuchsia_power_broker::LeaseControl>, Error> UnsafeAcquire();

  // Adds a power element to the topology that opportunistically depends on ExecutionState. The
  // promise returned needs to be scheduled on an executor and will complete ok if the power element
  // is successfully added to the topology. If there is an error, the promise will return an error
  // indicating why.
  //
  // This function must only be called once.
  fpromise::promise<void, Error> AddPowerElement();

  fpromise::promise<fidl::Client<::fuchsia_power_broker::LeaseControl>, Error> DoAcquireLease();

  fpromise::promise<> WaitForRequiredLevelActive();

  void WatchRequiredLevel();

  async_dispatcher_t* dispatcher_;
  std::string power_element_name_;
  bool add_power_element_called_;
  fpromise::barrier add_power_element_barrier_;

  AsyncEventHandlerOpen<fuchsia_power_system::ActivityGovernor> sag_event_handler_;
  fidl::Client<fuchsia_power_system::ActivityGovernor> sag_;

  AsyncEventHandlerOpen<fuchsia_power_broker::Topology> topology_event_handler_;
  fidl::Client<fuchsia_power_broker::Topology> topology_;

  AsyncEventHandlerOpen<fuchsia_power_broker::LeaseControl> lease_control_event_handler_;

  fidl::Client<fuchsia_power_broker::CurrentLevel> current_level_client_;
  fidl::Client<fuchsia_power_broker::RequiredLevel> required_level_client_;
  uint8_t required_level_;
  std::vector<fpromise::suspended_task> waiting_for_required_level_;

  // Channels that will be valid and must be kept open once the element is added to the topology.
  fidl::Client<fuchsia_power_broker::ElementControl> element_control_;
  fidl::Client<fuchsia_power_broker::Lessor> lessor_;

  // Enables this to be safely captured in promises returned by Acquire. Any public method that
  // returns a promise must wrap it with |scope_|.
  fpromise::scope scope_;
};

}  // namespace forensics::exceptions::handler

#endif  // SRC_DEVELOPER_FORENSICS_EXCEPTIONS_HANDLER_WAKE_LEASE_H_
