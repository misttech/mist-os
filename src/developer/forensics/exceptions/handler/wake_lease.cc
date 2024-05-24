// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/exceptions/handler/wake_lease.h"

#include <lib/async/dispatcher.h>
#include <lib/fidl/cpp/channel.h>
#include <lib/fpromise/bridge.h>
#include <lib/fpromise/promise.h>
#include <lib/fpromise/result.h>
#include <lib/syslog/cpp/macros.h>

#include <utility>

#include "src/developer/forensics/exceptions/constants.h"
#include "src/developer/forensics/utils/errors.h"

namespace forensics::exceptions::handler {
namespace {

namespace fpb = fuchsia_power_broker;
namespace fps = fuchsia_power_system;

class SagEventHandler : public fidl::AsyncEventHandler<fps::ActivityGovernor> {
 public:
  void on_fidl_error(const fidl::UnbindInfo error) override { FX_LOGS(ERROR) << error; }

  void handle_unknown_event(
      const fidl::UnknownEventMetadata<fps::ActivityGovernor> metadata) override {
    FX_LOGS(ERROR) << "Unexpected event ordinal: " << metadata.event_ordinal;
  }
};

class TopologyEventHandler : public fidl::AsyncEventHandler<fpb::Topology> {
 public:
  void on_fidl_error(const fidl::UnbindInfo error) override { FX_LOGS(ERROR) << error; }

  void handle_unknown_event(const fidl::UnknownEventMetadata<fpb::Topology> metadata) override {
    FX_LOGS(ERROR) << "Unexpected event ordinal: " << metadata.event_ordinal;
  }
};

fpb::ElementSchema BuildSchema(zx::event requires_token, fidl::ServerEnd<fpb::Lessor> server_end,
                               const std::string& element_name) {
  fpb::LevelDependency dependency(
      /*dependency_type=*/fpb::DependencyType::kPassive,
      /*dependent_level=*/kPowerLevelActive,
      /*requires_token=*/std::move(requires_token),
      /*requires_level=*/
      fidl::ToUnderlying(fps::ExecutionStateLevel::kWakeHandling));

  fpb::ElementSchema schema;
  schema.element_name(element_name)
      .initial_current_level(kPowerLevelInactive)
      .lessor_channel(std::move(server_end))
      .valid_levels(std::vector<uint8_t>({
          kPowerLevelInactive,
          kPowerLevelActive,
      }));

  std::optional<std::vector<fpb::LevelDependency>>& dependencies = schema.dependencies();
  dependencies.emplace().push_back(std::move(dependency));

  return schema;
}

}  // namespace

WakeLease::WakeLease(async_dispatcher_t* dispatcher,
                     fidl::ClientEnd<fps::ActivityGovernor> sag_client_end,
                     fidl::ClientEnd<fpb::Topology> topology_client_end)
    : dispatcher_(dispatcher),
      add_power_element_called_(false),
      sag_event_handler_(std::make_unique<SagEventHandler>()),
      sag_(std::move(sag_client_end), dispatcher_, sag_event_handler_.get()),
      topology_event_handler_(std::make_unique<TopologyEventHandler>()),
      topology_(std::move(topology_client_end), dispatcher_, topology_event_handler_.get()) {}

fpromise::promise<void, Error> WakeLease::AddPowerElement(std::string power_element_name) {
  FX_CHECK(!add_power_element_called_);
  add_power_element_called_ = true;

  fpromise::bridge<void, Error> bridge;

  // Getting the SAG power elements is necessary because we need the execution state token.
  //
  // TODO(https://fxbug.dev/341104129): connect to SAG here instead of injecting the connection in
  // the constructor. Disconnect once no longer needed.
  sag_->GetPowerElements().Then(
      [this, completer = std::move(bridge.completer),
       power_element_name = std::move(power_element_name)](
          fidl::Result<fps::ActivityGovernor::GetPowerElements>& result) mutable {
        if (result.is_error()) {
          FX_LOGS(ERROR) << "Failed to retrieve power elements: "
                         << result.error_value().FormatDescription();
          completer.complete_error(Error::kBadValue);
          return;
        }

        if (!result->execution_state().has_value() ||
            !result->execution_state()->passive_dependency_token().has_value()) {
          FX_LOGS(ERROR) << "Failed to get execution state passive dependency token";
          completer.complete_error(Error::kBadValue);
          return;
        }

        zx::result<fidl::Endpoints<fpb::Lessor>> endpoints = fidl::CreateEndpoints<fpb::Lessor>();
        if (endpoints.is_error()) {
          FX_LOGS(ERROR) << "Couldn't create FIDL endpoints: " << endpoints.status_string();
          completer.complete_error(Error::kConnectionError);
          return;
        }

        fpb::ElementSchema schema =
            BuildSchema(std::move(result->execution_state()->passive_dependency_token()).value(),
                        std::move(endpoints->server), power_element_name);

        // TODO(https://fxbug.dev/341104129): connect to topology here instead of injecting the
        // connection in the constructor. Disconnect once no longer needed.
        topology_->AddElement(std::move(schema))
            .Then(
                [this, completer = std::move(completer), client_end = std::move(endpoints->client)](
                    fidl::Result<fpb::Topology::AddElement>& result) mutable {
                  if (result.is_error()) {
                    FX_LOGS(ERROR) << "Failed to add element to topology: "
                                   << result.error_value().FormatDescription();
                    completer.complete_error(Error::kBadValue);
                    return;
                  }

                  element_control_channel_ = std::move(result.value().element_control_channel());
                  lessor_ = fidl::Client<fpb::Lessor>(std::move(client_end), dispatcher_);
                  completer.complete_ok();
                });
      });

  return bridge.consumer.promise_or(fpromise::error(Error::kLogicError));
}

}  // namespace forensics::exceptions::handler
