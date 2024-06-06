// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_LESSOR_H_
#define SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_LESSOR_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/test_base.h>
#include <lib/async/dispatcher.h>
#include <lib/syslog/cpp/macros.h>

#include <string>
#include <vector>

#include "src/developer/forensics/testing/stubs/power_broker_lease_control.h"

namespace forensics::stubs {

class PowerBrokerLessorBase : public fidl::testing::TestBase<fuchsia_power_broker::Lessor> {
 public:
  PowerBrokerLessorBase(fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end,
                        async_dispatcher_t* dispatcher)
      : binding_(dispatcher, std::move(server_end), this, &PowerBrokerLessorBase::OnFidlClosed) {}

  virtual ~PowerBrokerLessorBase() = default;

  virtual bool IsActive() const = 0;

  void SetLeaseStatus(fuchsia_power_broker::LeaseStatus status);

  static void OnFidlClosed(const fidl::UnbindInfo error) { FX_LOGS(ERROR) << error; }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    FX_NOTIMPLEMENTED() << name << " is not implemented";
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Lessor> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    FX_NOTIMPLEMENTED() << "Method ordinal '" << metadata.method_ordinal << "' is not implemented";
  }

 protected:
  std::vector<std::unique_ptr<PowerBrokerLeaseControl>>& LeaseControls() { return lease_controls_; }

  const std::vector<std::unique_ptr<PowerBrokerLeaseControl>>& LeaseControls() const {
    return lease_controls_;
  }

 private:
  fidl::ServerBinding<fuchsia_power_broker::Lessor> binding_;
  std::vector<std::unique_ptr<PowerBrokerLeaseControl>> lease_controls_;
};

class PowerBrokerLessor : public PowerBrokerLessorBase {
 public:
  explicit PowerBrokerLessor(fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end,
                             async_dispatcher_t* dispatcher,
                             fuchsia_power_broker::LeaseStatus initial_status)
      : PowerBrokerLessorBase(std::move(server_end), dispatcher),
        dispatcher_(dispatcher),
        initial_status_(initial_status) {}

  void Lease(LeaseRequest& request, LeaseCompleter::Sync& completer) override;

  bool IsActive() const override;

 private:
  async_dispatcher_t* dispatcher_;
  fuchsia_power_broker::LeaseStatus initial_status_;
};

class PowerBrokerLessorClosesConnection : public PowerBrokerLessorBase {
 public:
  explicit PowerBrokerLessorClosesConnection(
      fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end, async_dispatcher_t* dispatcher)
      : PowerBrokerLessorBase(std::move(server_end), dispatcher) {}

  void Lease(LeaseRequest& request, LeaseCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_PEER_CLOSED);
  }

  bool IsActive() const override { return false; }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    FX_NOTIMPLEMENTED() << name << " is not implemented";
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Lessor> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    FX_NOTIMPLEMENTED() << "Method ordinal '" << metadata.method_ordinal << "' is not implemented";
  }
};

}  // namespace forensics::stubs

#endif  // SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_LESSOR_H_
