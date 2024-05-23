// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_TOPOLOGY_H_
#define SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_TOPOLOGY_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/test_base.h>
#include <lib/async/dispatcher.h>
#include <lib/syslog/cpp/macros.h>

#include <memory>
#include <string>
#include <unordered_map>
#include <vector>

#include "src/developer/forensics/testing/stubs/power_broker_element_control.h"

namespace forensics::stubs {

class PowerBrokerTopology : public fidl::testing::TestBase<fuchsia_power_broker::Topology> {
 public:
  explicit PowerBrokerTopology(fidl::ServerEnd<fuchsia_power_broker::Topology> server_end,
                               async_dispatcher_t* dispatcher)
      : dispatcher_(dispatcher),
        binding_(dispatcher_, std::move(server_end), this, &PowerBrokerTopology::OnFidlClosed) {}

  // Adds an element to the topology. |request| must have a valid element_name, lessor_channel,
  // dependencies, and valid_levels.
  void AddElement(AddElementRequest& request, AddElementCompleter::Sync& completer) override;

  static void OnFidlClosed(const fidl::UnbindInfo error) { FX_LOGS(ERROR) << error; }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    FX_NOTIMPLEMENTED() << name << " is not implemented";
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Topology> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    FX_NOTIMPLEMENTED() << "Method ordinal '" << metadata.method_ordinal << "' is not implemented";
  }

  bool ElementInTopology(const std::string& element_name) const {
    return added_elements_.count(element_name) > 0;
  }

  const std::vector<fuchsia_power_broker::LevelDependency>& Dependencies(
      const std::string& element_name) const;

 private:
  struct PowerElement {
    std::vector<fuchsia_power_broker::LevelDependency> dependencies;
    std::unique_ptr<PowerBrokerElementControl> element_control_server;
  };

  async_dispatcher_t* dispatcher_;
  std::unordered_map<std::string, PowerElement> added_elements_;
  fidl::ServerBinding<fuchsia_power_broker::Topology> binding_;
};

class PowerBrokerTopologyClosesConnection
    : public fidl::testing::TestBase<fuchsia_power_broker::Topology> {
 public:
  PowerBrokerTopologyClosesConnection(fidl::ServerEnd<fuchsia_power_broker::Topology> server_end,
                                      async_dispatcher_t* dispatcher)
      : binding_(dispatcher, std::move(server_end), this,
                 &PowerBrokerTopologyClosesConnection::OnFidlClosed) {}

  void AddElement(AddElementRequest& request, AddElementCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_PEER_CLOSED);
  }

  static void OnFidlClosed(const fidl::UnbindInfo error) { FX_LOGS(ERROR) << error; }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    FX_NOTIMPLEMENTED() << name << " is not implemented";
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Topology> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    FX_NOTIMPLEMENTED() << "Method ordinal '" << metadata.method_ordinal << "' is not implemented";
  }

  fidl::ServerBinding<fuchsia_power_broker::Topology> binding_;
};

}  // namespace forensics::stubs

#endif  // SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_TOPOLOGY_H_
