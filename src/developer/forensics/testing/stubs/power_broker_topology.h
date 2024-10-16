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
#include <queue>
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

#include "src/developer/forensics/testing/stubs/fidl_server.h"
#include "src/developer/forensics/testing/stubs/power_broker_current_level.h"
#include "src/developer/forensics/testing/stubs/power_broker_element_control.h"
#include "src/developer/forensics/testing/stubs/power_broker_lessor.h"
#include "src/developer/forensics/testing/stubs/power_broker_required_level.h"

namespace forensics::stubs {

// Stores added elements until the ElementControl channel is dropped.
class PowerBrokerTopologyBase : public FidlServer<fuchsia_power_broker::Topology> {
 public:
  using ConstructLessorFn = std::function<std::unique_ptr<PowerBrokerLessorBase>(
      fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end,
      std::function<void(uint8_t)> level_changed)>;

  virtual ~PowerBrokerTopologyBase() = default;

  static void OnFidlClosed(const fidl::UnbindInfo error) { FX_LOGS(ERROR) << error; }

  bool ElementInTopology(const std::string& element_name) const {
    return added_elements_.count(element_name) > 0;
  }

  const std::vector<fuchsia_power_broker::LevelDependency>& Dependencies(
      const std::string& element_name) const;

  bool IsLeaseActive(const std::string& element_name) const;

  void SetRequiredLevel(const std::string& element_name, uint8_t level);

  uint8_t GetCurrentLevel(const std::string& element_name) const;

 protected:
  explicit PowerBrokerTopologyBase(fidl::ServerEnd<fuchsia_power_broker::Topology> server_end,
                                   async_dispatcher_t* dispatcher, uint8_t initial_required_level,
                                   ConstructLessorFn construct_lessor)
      : dispatcher_(dispatcher),
        initial_required_level_(initial_required_level),
        construct_lessor_(std::move(construct_lessor)),
        binding_(dispatcher_, std::move(server_end), this, &PowerBrokerTopologyBase::OnFidlClosed) {
  }

  struct PowerElement {
    std::vector<fuchsia_power_broker::LevelDependency> dependencies;
    std::unique_ptr<PowerBrokerElementControl> element_control_server;
    std::unique_ptr<PowerBrokerLessorBase> lessor_server;
    std::unique_ptr<PowerBrokerRequiredLevel> required_level_server;
    std::unique_ptr<PowerBrokerCurrentLevel> current_level_server;
  };

  async_dispatcher_t* Dispatcher() { return dispatcher_; }

  std::unordered_map<std::string, PowerElement>& AddedElements() { return added_elements_; }

  uint8_t InitialRequiredLevel() const { return initial_required_level_; }

  ConstructLessorFn& ConstructLessor() { return construct_lessor_; }

 private:
  async_dispatcher_t* dispatcher_;
  uint8_t initial_required_level_;
  ConstructLessorFn construct_lessor_;
  std::unordered_map<std::string, PowerElement> added_elements_;
  fidl::ServerBinding<fuchsia_power_broker::Topology> binding_;
};

class PowerBrokerTopology : public PowerBrokerTopologyBase {
 public:
  explicit PowerBrokerTopology(fidl::ServerEnd<fuchsia_power_broker::Topology> server_end,
                               async_dispatcher_t* dispatcher, uint8_t initial_required_level,
                               ConstructLessorFn construct_lessor)
      : PowerBrokerTopologyBase(std::move(server_end), dispatcher, initial_required_level,
                                std::move(construct_lessor)) {}

  // Adds an element to the topology. |request| must have a valid element_name, lessor_channel,
  // dependencies, and valid_levels. Check-fails if elements with duplicate names are added.
  void AddElement(AddElementRequest& request, AddElementCompleter::Sync& completer) override;
};

// Will not respond with the ElementControl channel until PopResponse is called. Responses are
// popped in FIFO fashion.
class PowerBrokerTopologyDelaysResponse : public PowerBrokerTopologyBase {
 public:
  explicit PowerBrokerTopologyDelaysResponse(
      fidl::ServerEnd<fuchsia_power_broker::Topology> server_end, async_dispatcher_t* dispatcher,
      uint8_t initial_required_level, ConstructLessorFn construct_lessor)
      : PowerBrokerTopologyBase(std::move(server_end), dispatcher, initial_required_level,
                                std::move(construct_lessor)) {}

  // Adds an element to the topology. |request| must have a valid element_name, lessor_channel,
  // dependencies, and valid_levels. Check-fails if elements with duplicate names are added.
  void AddElement(AddElementRequest& request, AddElementCompleter::Sync& completer) override;
  void PopResponse();

 private:
  std::queue<AddElementCompleter::Async> queued_completers_;
};

class PowerBrokerTopologyClosesConnection : public FidlServer<fuchsia_power_broker::Topology> {
 public:
  PowerBrokerTopologyClosesConnection(fidl::ServerEnd<fuchsia_power_broker::Topology> server_end,
                                      async_dispatcher_t* dispatcher)
      : binding_(dispatcher, std::move(server_end), this,
                 &PowerBrokerTopologyClosesConnection::OnFidlClosed) {}

  void AddElement(AddElementRequest& request, AddElementCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_PEER_CLOSED);
  }

  static void OnFidlClosed(const fidl::UnbindInfo error) { FX_LOGS(ERROR) << error; }

  fidl::ServerBinding<fuchsia_power_broker::Topology> binding_;
};

}  // namespace forensics::stubs

#endif  // SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_TOPOLOGY_H_
