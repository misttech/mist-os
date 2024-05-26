// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/testing/stubs/power_broker_topology.h"

namespace forensics::stubs {

void PowerBrokerTopology::AddElement(AddElementRequest& request,
                                     AddElementCompleter::Sync& completer) {
  FX_CHECK(request.element_name().has_value());
  FX_CHECK(request.lessor_channel().has_value());
  FX_CHECK(request.dependencies().has_value());
  FX_CHECK(request.valid_levels().has_value());

  // Remove the element from the topology if the element_control channel is dropped.
  auto element_control_endpoints = fidl::CreateEndpoints<fuchsia_power_broker::ElementControl>();
  auto element_control_server = std::make_unique<PowerBrokerElementControl>(
      std::move(element_control_endpoints->server), dispatcher_,
      [this, element_name = *request.element_name()]() { added_elements_.erase(element_name); });

  added_elements_.insert(
      {*request.element_name(), PowerElement{
                                    .dependencies = std::move(request.dependencies()).value(),
                                    .element_control_server = std::move(element_control_server),
                                }});

  fuchsia_power_broker::TopologyAddElementResponse response(
      std::move(element_control_endpoints->client));
  completer.Reply(
      fidl::Response<fuchsia_power_broker::Topology::AddElement>(fit::ok(std::move(response))));
}

const std::vector<fuchsia_power_broker::LevelDependency>& PowerBrokerTopology::Dependencies(
    const std::string& element_name) const {
  static const auto& kEmptyDependencies = *new std::vector<fuchsia_power_broker::LevelDependency>();

  const auto element = added_elements_.find(element_name);
  if (element == added_elements_.end()) {
    return kEmptyDependencies;
  }

  return element->second.dependencies;
}

}  // namespace forensics::stubs
