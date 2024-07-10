// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/testing/stubs/wake_lease.h"

#include <lib/stdcompat/vector.h>

namespace forensics::stubs {

fpromise::promise<fidl::Client<fuchsia_power_broker::LeaseControl>, Error> WakeLease::Acquire(
    const zx::duration timeout) {
  zx::result<fidl::Endpoints<fuchsia_power_broker::LeaseControl>> endpoints =
      fidl::CreateEndpoints<fuchsia_power_broker::LeaseControl>();
  FX_CHECK(endpoints.is_ok());

  auto lease_control = std::make_unique<PowerBrokerLeaseControl>(
      /*level=*/1, std::move(endpoints->server), dispatcher_,
      /*on_closure=*/[this](PowerBrokerLeaseControl* control) {
        cpp20::erase_if(lease_controls_,
                        [control](const std::unique_ptr<PowerBrokerLeaseControl>& item) {
                          return item.get() == control;
                        });
      });
  lease_controls_.push_back(std::move(lease_control));

  fidl::Client<fuchsia_power_broker::LeaseControl> lease_control_client(
      std::move(endpoints->client), dispatcher_);

  return fpromise::make_result_promise<fidl::Client<fuchsia_power_broker::LeaseControl>, Error>(
      fpromise::ok(std::move(lease_control_client)));
}

}  // namespace forensics::stubs
