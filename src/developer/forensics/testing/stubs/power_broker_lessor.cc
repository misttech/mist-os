// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/testing/stubs/power_broker_lessor.h"

#include <lib/stdcompat/vector.h>

#include <algorithm>
#include <utility>

#include "src/developer/forensics/exceptions/constants.h"

namespace forensics::stubs {

void PowerBrokerLessor::Lease(LeaseRequest& request, LeaseCompleter::Sync& completer) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_power_broker::LeaseControl>();

  auto lease_control = std::make_unique<PowerBrokerLeaseControl>(
      request.level(), std::move(endpoints->server), dispatcher_,
      [this](PowerBrokerLeaseControl* control) {
        cpp20::erase_if(lease_controls_,
                        [control](const std::unique_ptr<PowerBrokerLeaseControl>& item) {
                          return item.get() == control;
                        });
      });

  lease_controls_.push_back(std::move(lease_control));

  fuchsia_power_broker::LessorLeaseResponse response;
  response.lease_control(std::move(endpoints->client));
  completer.Reply(
      fidl::Response<fuchsia_power_broker::Lessor::Lease>(fit::ok(std::move(response))));
}

bool PowerBrokerLessor::IsActive() const {
  return std::any_of(lease_controls_.begin(), lease_controls_.end(),
                     [](const std::unique_ptr<PowerBrokerLeaseControl>& control) {
                       return control->Level() == exceptions::kPowerLevelActive;
                     });
}

}  // namespace forensics::stubs
