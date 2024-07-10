// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_LESSOR_H_
#define SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_LESSOR_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/test_base.h>
#include <lib/async/dispatcher.h>
#include <lib/syslog/cpp/macros.h>

#include <vector>

#include "src/developer/forensics/testing/stubs/fidl_server.h"
#include "src/developer/forensics/testing/stubs/power_broker_lease_control.h"

namespace forensics::stubs {

class PowerBrokerLessorBase : public FidlServer<fuchsia_power_broker::Lessor> {
 public:
  PowerBrokerLessorBase(fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end,
                        async_dispatcher_t* dispatcher)
      : binding_(dispatcher, std::move(server_end), this, &PowerBrokerLessorBase::OnFidlClosed) {}

  virtual ~PowerBrokerLessorBase() = default;

  bool IsActive() const;

  static void OnFidlClosed(const fidl::UnbindInfo error) { FX_LOGS(ERROR) << error; }

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
                             std::function<void(uint8_t)> level_changed)
      : PowerBrokerLessorBase(std::move(server_end), dispatcher),
        dispatcher_(dispatcher),
        level_changed_(std::move(level_changed)) {}

  void Lease(LeaseRequest& request, LeaseCompleter::Sync& completer) override;

 private:
  async_dispatcher_t* dispatcher_;
  std::function<void(uint8_t)> level_changed_;
};

class PowerBrokerLessorDelaysRequiredLevel : public PowerBrokerLessorBase {
 public:
  explicit PowerBrokerLessorDelaysRequiredLevel(
      fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end, async_dispatcher_t* dispatcher,
      std::function<void(uint8_t)> level_changed)
      : PowerBrokerLessorBase(std::move(server_end), dispatcher),
        dispatcher_(dispatcher),
        level_changed_(std::move(level_changed)) {}

  void Lease(LeaseRequest& request, LeaseCompleter::Sync& completer) override;

 private:
  async_dispatcher_t* dispatcher_;
  std::function<void(uint8_t)> level_changed_;
};

class PowerBrokerLessorClosesConnection : public PowerBrokerLessorBase {
 public:
  explicit PowerBrokerLessorClosesConnection(
      fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end, async_dispatcher_t* dispatcher)
      : PowerBrokerLessorBase(std::move(server_end), dispatcher) {}

  void Lease(LeaseRequest& request, LeaseCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_PEER_CLOSED);
  }
};

}  // namespace forensics::stubs

#endif  // SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_LESSOR_H_
