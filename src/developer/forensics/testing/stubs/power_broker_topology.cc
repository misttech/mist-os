// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/testing/stubs/power_broker_topology.h"

#include <lib/stdcompat/functional.h>

#include "src/developer/forensics/testing/stubs/power_broker_required_level.h"

namespace forensics::stubs {

void PowerBrokerTopology::AddElement(AddElementRequest& request,
                                     AddElementCompleter::Sync& completer) {
  FX_CHECK(request.element_name().has_value());
  FX_CHECK(request.lessor_channel().has_value());
  FX_CHECK(request.dependencies().has_value());
  FX_CHECK(request.valid_levels().has_value());
  FX_CHECK(request.level_control_channels().has_value());

  FX_CHECK(AddedElements().count(*request.element_name()) == 0)
      << "Duplicate element added to topology";

  // Remove the element from the topology if the element_control channel is dropped.
  auto element_control_endpoints = fidl::CreateEndpoints<fuchsia_power_broker::ElementControl>();
  auto element_control_server = std::make_unique<PowerBrokerElementControl>(
      std::move(element_control_endpoints->server), Dispatcher(),
      [this, element_name = *request.element_name()]() { AddedElements().erase(element_name); });

  auto required_level_server = std::make_unique<PowerBrokerRequiredLevel>(
      std::move(request.level_control_channels()->required()), Dispatcher(),
      InitialRequiredLevel());

  std::unique_ptr<PowerBrokerLessorBase> lessor_server =
      ConstructLessor()(std::move(request.lessor_channel()).value(),
                        /*level_changed=*/cpp20::bind_front(&PowerBrokerRequiredLevel::SetLevel,
                                                            required_level_server.get()));

  AddedElements().insert(
      {*request.element_name(), PowerElement{
                                    .dependencies = std::move(request.dependencies()).value(),
                                    .element_control_server = std::move(element_control_server),
                                    .lessor_server = std::move(lessor_server),
                                    .required_level_server = std::move(required_level_server),
                                }});

  fuchsia_power_broker::TopologyAddElementResponse response(
      std::move(element_control_endpoints->client));

  completer.Reply(
      fidl::Response<fuchsia_power_broker::Topology::AddElement>(fit::ok(std::move(response))));
}

void PowerBrokerTopologyDelaysResponse::AddElement(AddElementRequest& request,
                                                   AddElementCompleter::Sync& completer) {
  FX_CHECK(request.element_name().has_value());
  FX_CHECK(request.lessor_channel().has_value());
  FX_CHECK(request.dependencies().has_value());
  FX_CHECK(request.valid_levels().has_value());
  FX_CHECK(request.level_control_channels().has_value());

  FX_CHECK(AddedElements().count(*request.element_name()) == 0)
      << "Duplicate element added to topology";

  // Remove the element from the topology if the element_control channel is dropped.
  auto element_control_endpoints = fidl::CreateEndpoints<fuchsia_power_broker::ElementControl>();
  auto element_control_server = std::make_unique<PowerBrokerElementControl>(
      std::move(element_control_endpoints->server), Dispatcher(),
      [this, element_name = *request.element_name()]() { AddedElements().erase(element_name); });

  auto required_level_server = std::make_unique<PowerBrokerRequiredLevel>(
      std::move(request.level_control_channels()->required()), Dispatcher(),
      InitialRequiredLevel());

  std::unique_ptr<PowerBrokerLessorBase> lessor_server =
      ConstructLessor()(std::move(request.lessor_channel()).value(),
                        /*level_changed=*/cpp20::bind_front(&PowerBrokerRequiredLevel::SetLevel,
                                                            required_level_server.get()));

  AddedElements().insert(
      {*request.element_name(), PowerElement{
                                    .dependencies = std::move(request.dependencies()).value(),
                                    .element_control_server = std::move(element_control_server),
                                    .lessor_server = std::move(lessor_server),
                                    .required_level_server = std::move(required_level_server),
                                }});

  fuchsia_power_broker::TopologyAddElementResponse response(
      std::move(element_control_endpoints->client));

  queued_responses_.push(QueuedResponse{
      .completer = completer.ToAsync(),
      .response = std::move(response),
  });
}

void PowerBrokerTopologyDelaysResponse::PopResponse() {
  FX_CHECK(!queued_responses_.empty());

  queued_responses_.front().completer.Reply(
      fidl::Response<fuchsia_power_broker::Topology::AddElement>(
          fit::ok(std::move(queued_responses_.front().response))));
  queued_responses_.pop();
}

const std::vector<fuchsia_power_broker::LevelDependency>& PowerBrokerTopologyBase::Dependencies(
    const std::string& element_name) const {
  static const auto& kEmptyDependencies = *new std::vector<fuchsia_power_broker::LevelDependency>();

  const auto element = added_elements_.find(element_name);
  if (element == added_elements_.end()) {
    return kEmptyDependencies;
  }

  return element->second.dependencies;
}

bool PowerBrokerTopologyBase::IsLeaseActive(const std::string& element_name) const {
  if (added_elements_.count(element_name) == 0) {
    return false;
  }

  return added_elements_.find(element_name)->second.lessor_server->IsActive();
}

void PowerBrokerTopologyBase::SetRequiredLevel(const std::string& element_name,
                                               const uint8_t level) {
  const auto element = added_elements_.find(element_name);
  FX_CHECK(element != added_elements_.end());

  element->second.required_level_server->SetLevel(level);
}

}  // namespace forensics::stubs
