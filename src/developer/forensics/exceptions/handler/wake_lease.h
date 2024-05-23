// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_EXCEPTIONS_HANDLER_WAKE_LEASE_H_
#define SRC_DEVELOPER_FORENSICS_EXCEPTIONS_HANDLER_WAKE_LEASE_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/fpromise/promise.h>

#include <memory>
#include <string>

#include "src/developer/forensics/utils/errors.h"

namespace forensics::exceptions::handler {

// Adds a power element passively dependent on (ExecutionState, WakeHandling).
//
// See https://fxbug.dev/333110044 for progress on acquiring a wake lease.
class WakeLease {
 public:
  WakeLease(async_dispatcher_t* dispatcher,
            fidl::ClientEnd<fuchsia_power_system::ActivityGovernor> sag_client_end,
            fidl::ClientEnd<fuchsia_power_broker::Topology> topology_client_end);

  // Adds a power element to the topology that passively depends on ExecutionState. The promise
  // returned needs scheduled on an executor and will complete ok if the power element is
  // successfully added to the topology. If there is an error, the promise will return an error
  // indicating why.
  //
  // This function must only be called once.
  fpromise::promise<void, Error> AddPowerElement(std::string power_element_name);

 private:
  async_dispatcher_t* dispatcher_;
  bool add_power_element_called_;

  std::unique_ptr<fidl::AsyncEventHandler<fuchsia_power_system::ActivityGovernor>>
      sag_event_handler_;
  fidl::Client<fuchsia_power_system::ActivityGovernor> sag_;

  std::unique_ptr<fidl::AsyncEventHandler<fuchsia_power_broker::Topology>> topology_event_handler_;
  fidl::Client<fuchsia_power_broker::Topology> topology_;

  // Channels that will be valid and must be kept open once the element is added to the topology.
  fidl::ClientEnd<fuchsia_power_broker::ElementControl> element_control_channel_;
  fidl::Client<fuchsia_power_broker::Lessor> lessor_;
};

}  // namespace forensics::exceptions::handler

#endif  // SRC_DEVELOPER_FORENSICS_EXCEPTIONS_HANDLER_WAKE_LEASE_H_
