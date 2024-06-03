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

#include <memory>
#include <string>

#include "src/developer/forensics/utils/errors.h"
#include "src/lib/fxl/memory/weak_ptr.h"

namespace forensics::exceptions::handler {

// Adds a power element passively dependent on (ExecutionState, WakeHandling) and takes a lease on
// that element.
class WakeLease {
 public:
  WakeLease(async_dispatcher_t* dispatcher, const std::string& power_element_name,
            fidl::ClientEnd<fuchsia_power_system::ActivityGovernor> sag_client_end,
            fidl::ClientEnd<fuchsia_power_broker::Topology> topology_client_end);

  // Acquires a lease on a power element that passively depends on (Execution State, Wake Handling).
  // Note, the power element is added automatically when Acquire is called for the first time.
  //
  // The promise returned needs scheduled on an executor and will complete ok with the power lease
  // channel if successful. If there is an error, the promise will return an error indicating why.
  //
  // This function can be called many times. If the lease returned falls out of scope, the lease
  // will be dropped and can be later reacquired.
  fpromise::promise<fidl::ClientEnd<::fuchsia_power_broker::LeaseControl>, Error> Acquire();

 private:
  // Adds a power element to the topology that passively depends on ExecutionState. The promise
  // returned needs scheduled on an executor and will complete ok if the power element is
  // successfully added to the topology. If there is an error, the promise will return an error
  // indicating why.
  //
  // This function must only be called once.
  fpromise::promise<void, Error> AddPowerElement();

  fpromise::promise<fidl::ClientEnd<::fuchsia_power_broker::LeaseControl>, Error> DoAcquireLease();

  async_dispatcher_t* dispatcher_;
  std::string power_element_name_;
  bool add_power_element_called_;
  fpromise::barrier add_power_element_barrier_;

  std::unique_ptr<fidl::AsyncEventHandler<fuchsia_power_system::ActivityGovernor>>
      sag_event_handler_;
  fidl::Client<fuchsia_power_system::ActivityGovernor> sag_;

  std::unique_ptr<fidl::AsyncEventHandler<fuchsia_power_broker::Topology>> topology_event_handler_;
  fidl::Client<fuchsia_power_broker::Topology> topology_;

  // Channels that will be valid and must be kept open once the element is added to the topology.
  fidl::ClientEnd<fuchsia_power_broker::ElementControl> element_control_channel_;
  fidl::Client<fuchsia_power_broker::Lessor> lessor_;

  fxl::WeakPtrFactory<WakeLease> ptr_factory_{this};
};

}  // namespace forensics::exceptions::handler

#endif  // SRC_DEVELOPER_FORENSICS_EXCEPTIONS_HANDLER_WAKE_LEASE_H_
