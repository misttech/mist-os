// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_LEASE_CONTROL_H_
#define SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_LEASE_CONTROL_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/test_base.h>
#include <lib/async/dispatcher.h>
#include <lib/syslog/cpp/macros.h>

#include "src/developer/forensics/testing/stubs/fidl_server.h"

namespace forensics::stubs {

class PowerBrokerLeaseControl : public FidlServer<fuchsia_power_broker::LeaseControl> {
 public:
  PowerBrokerLeaseControl(uint8_t level,
                          fidl::ServerEnd<fuchsia_power_broker::LeaseControl> server_end,
                          async_dispatcher_t* dispatcher,
                          std::function<void(PowerBrokerLeaseControl*)> on_closure)
      : level_(level),
        binding_(dispatcher, std::move(server_end), this,
                 std::mem_fn(&PowerBrokerLeaseControl::OnFidlClosed)),
        on_closure_(std::move(on_closure)) {}

  void OnFidlClosed(const fidl::UnbindInfo error) {
    FX_LOGS(ERROR) << error;
    on_closure_(this);
  }

  uint8_t Level() const { return level_; }

 private:
  uint8_t level_;
  fidl::ServerBinding<fuchsia_power_broker::LeaseControl> binding_;
  std::function<void(PowerBrokerLeaseControl*)> on_closure_;
};

}  // namespace forensics::stubs

#endif  // SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_LEASE_CONTROL_H_
