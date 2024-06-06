// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_LEASE_CONTROL_H_
#define SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_LEASE_CONTROL_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/test_base.h>
#include <lib/async/dispatcher.h>
#include <lib/syslog/cpp/macros.h>

#include <optional>

namespace forensics::stubs {

class PowerBrokerLeaseControl : public fidl::testing::TestBase<fuchsia_power_broker::LeaseControl> {
 public:
  PowerBrokerLeaseControl(uint8_t level,
                          fidl::ServerEnd<fuchsia_power_broker::LeaseControl> server_end,
                          async_dispatcher_t* dispatcher,
                          fuchsia_power_broker::LeaseStatus initial_status,
                          std::function<void(PowerBrokerLeaseControl*)> on_closure)
      : level_(level),
        binding_(dispatcher, std::move(server_end), this,
                 std::mem_fn(&PowerBrokerLeaseControl::OnFidlClosed)),
        current_status_(initial_status),
        on_closure_(std::move(on_closure)) {}

  void WatchStatus(WatchStatusRequest& request, WatchStatusCompleter::Sync& completer) override;

  void SetStatus(fuchsia_power_broker::LeaseStatus status);

  void OnFidlClosed(const fidl::UnbindInfo error) {
    FX_LOGS(ERROR) << error;
    on_closure_(this);
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    FX_NOTIMPLEMENTED() << name << " is not implemented";
  }

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_power_broker::LeaseControl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    FX_NOTIMPLEMENTED() << "Method ordinal '" << metadata.method_ordinal << "' is not implemented";
  }

  uint8_t Level() const { return level_; }

 private:
  uint8_t level_;
  fidl::ServerBinding<fuchsia_power_broker::LeaseControl> binding_;
  fuchsia_power_broker::LeaseStatus current_status_;
  std::function<void(PowerBrokerLeaseControl*)> on_closure_;
  std::optional<WatchStatusCompleter::Async> queued_response_;
};

}  // namespace forensics::stubs

#endif  // SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_LEASE_CONTROL_H_
