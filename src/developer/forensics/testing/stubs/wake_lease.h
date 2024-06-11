// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_TESTING_STUBS_WAKE_LEASE_H_
#define SRC_DEVELOPER_FORENSICS_TESTING_STUBS_WAKE_LEASE_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/fpromise/promise.h>

#include <vector>

#include "src/developer/forensics/exceptions/handler/wake_lease.h"
#include "src/developer/forensics/testing/stubs/power_broker_lease_control.h"
#include "src/developer/forensics/utils/errors.h"

namespace forensics::stubs {

class WakeLease : public exceptions::handler::WakeLeaseBase {
 public:
  explicit WakeLease(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  fpromise::promise<fidl::Client<fuchsia_power_broker::LeaseControl>, Error> Acquire(
      zx::duration timeout) override;

  bool LeaseHeld() const { return lease_controls_.size() > 0; }

 private:
  async_dispatcher_t* dispatcher_;
  std::vector<std::unique_ptr<PowerBrokerLeaseControl>> lease_controls_;
};

}  // namespace forensics::stubs

#endif  // SRC_DEVELOPER_FORENSICS_TESTING_STUBS_WAKE_LEASE_H_
