// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/exceptions/handler/wake_lease.h"

#include <lib/async/dispatcher.h>
#include <lib/fidl/cpp/any_error_in.h>
#include <lib/fidl/cpp/channel.h>
#include <lib/fpromise/bridge.h>
#include <lib/fpromise/promise.h>
#include <lib/fpromise/result.h>
#include <lib/syslog/cpp/macros.h>

#include <tuple>
#include <utility>

#include "src/developer/forensics/exceptions/constants.h"
#include "src/developer/forensics/utils/errors.h"
#include "src/developer/forensics/utils/promise_timeout.h"
#include "src/lib/fidl/cpp/contrib/fpromise/client.h"

namespace forensics::exceptions::handler {
namespace {

namespace fpb = fuchsia_power_broker;
namespace fps = fuchsia_power_system;

fpb::ElementSchema BuildSchema(zx::event requires_token, fidl::ServerEnd<fpb::Lessor> lessor_server,
                               fidl::ServerEnd<fpb::ElementControl> element_control_server,
                               fpb::LevelControlChannels level_control_channels,
                               const std::string& element_name) {
  fpb::LevelDependency dependency(
      /*dependency_type=*/fpb::DependencyType::kOpportunistic,
      /*dependent_level=*/kPowerLevelActive,
      /*requires_token=*/std::move(requires_token),
      /*requires_level_by_preference=*/
      std::vector<uint8_t>(1, fidl::ToUnderlying(fps::ExecutionStateLevel::kSuspending)));

  fpb::ElementSchema schema;
  schema.element_name(element_name)
      .initial_current_level(kPowerLevelInactive)
      .lessor_channel(std::move(lessor_server))
      .element_control(std::move(element_control_server))
      .level_control_channels(std::move(level_control_channels))
      .valid_levels(std::vector<uint8_t>({
          kPowerLevelInactive,
          kPowerLevelActive,
      }));

  std::optional<std::vector<fpb::LevelDependency>>& dependencies = schema.dependencies();
  dependencies.emplace().push_back(std::move(dependency));

  return schema;
}

}  // namespace

WakeLease::WakeLease(async_dispatcher_t* dispatcher, const std::string& power_element_name,
                     fidl::ClientEnd<fps::ActivityGovernor> sag_client_end,
                     fidl::ClientEnd<fpb::Topology> topology_client_end)
    : dispatcher_(dispatcher),
      power_element_name_(power_element_name),
      add_power_element_called_(false),
      sag_(std::move(sag_client_end), dispatcher_, &sag_event_handler_),
      topology_(std::move(topology_client_end), dispatcher_, &topology_event_handler_),
      required_level_(kPowerLevelInactive) {}

fpromise::promise<fidl::Client<fpb::LeaseControl>, Error> WakeLease::Acquire(
    const zx::duration timeout) {
  return MakePromiseTimeout<fidl::Client<fpb::LeaseControl>>(UnsafeAcquire().wrap_with(scope_),
                                                             dispatcher_, timeout);
}

fpromise::promise<fidl::Client<fpb::LeaseControl>, Error> WakeLease::UnsafeAcquire() {
  if (add_power_element_called_) {
    return DoAcquireLease();
  }

  return AddPowerElement()
      .and_then([this]() { return DoAcquireLease(); })
      .or_else([](const Error& add_element_error) {
        return fpromise::make_result_promise<fidl::Client<fpb::LeaseControl>, Error>(
            fpromise::error(add_element_error));
      });
}

fpromise::promise<void, Error> WakeLease::AddPowerElement() {
  FX_CHECK(!add_power_element_called_);
  add_power_element_called_ = true;

  fpromise::bridge<void, Error> bridge;

  // Getting the SAG power elements is necessary because we need the execution state token.
  //
  // TODO(https://fxbug.dev/341104129): connect to SAG here instead of injecting the connection in
  // the constructor. Disconnect once no longer needed.
  sag_->GetPowerElements().Then(
      [this, completer = std::move(bridge.completer)](
          fidl::Result<fps::ActivityGovernor::GetPowerElements>& result) mutable {
        if (result.is_error()) {
          FX_LOGS(ERROR) << "Failed to retrieve power elements: "
                         << result.error_value().FormatDescription();
          completer.complete_error(Error::kBadValue);
          return;
        }

        if (!result->execution_state().has_value() ||
            !result->execution_state()->opportunistic_dependency_token().has_value()) {
          FX_LOGS(ERROR) << "Failed to get execution state opportunistic dependency token";
          completer.complete_error(Error::kBadValue);
          return;
        }

        zx::result<fidl::Endpoints<fpb::Lessor>> lessor_endpoints =
            fidl::CreateEndpoints<fpb::Lessor>();
        if (lessor_endpoints.is_error()) {
          FX_LOGS(ERROR) << "Couldn't create FIDL endpoints: " << lessor_endpoints.status_string();
          completer.complete_error(Error::kConnectionError);
          return;
        }

        zx::result<fidl::Endpoints<fpb::ElementControl>> element_control_endpoints =
            fidl::CreateEndpoints<fpb::ElementControl>();
        if (element_control_endpoints.is_error()) {
          FX_LOGS(ERROR) << "Couldn't create FIDL endpoints: "
                         << element_control_endpoints.status_string();
          completer.complete_error(Error::kConnectionError);
          return;
        }

        zx::result<fidl::Endpoints<fpb::CurrentLevel>> current_level_endpoints =
            fidl::CreateEndpoints<fpb::CurrentLevel>();
        if (!current_level_endpoints.is_ok()) {
          FX_LOGS(ERROR) << "Couldn't create FIDL endpoints: "
                         << current_level_endpoints.status_string();
          completer.complete_error(Error::kConnectionError);
          return;
        }

        zx::result<fidl::Endpoints<fpb::RequiredLevel>> required_level_endpoints =
            fidl::CreateEndpoints<fpb::RequiredLevel>();
        if (!required_level_endpoints.is_ok()) {
          FX_LOGS(ERROR) << "Couldn't create FIDL endpoints: "
                         << required_level_endpoints.status_string();
          completer.complete_error(Error::kConnectionError);
          return;
        }

        fpb::LevelControlChannels level_control_endpoints{{
            .current = std::move(current_level_endpoints->server),
            .required = std::move(required_level_endpoints->server),
        }};

        fpb::ElementSchema schema = BuildSchema(
            std::move(result->execution_state()->opportunistic_dependency_token()).value(),
            std::move(lessor_endpoints->server), std::move(element_control_endpoints->server),
            std::move(level_control_endpoints), power_element_name_);

        element_control_.Bind(std::move(element_control_endpoints->client), dispatcher_);
        lessor_.Bind(std::move(lessor_endpoints->client), dispatcher_);
        current_level_client_.Bind(std::move(current_level_endpoints->client), dispatcher_);
        required_level_client_.Bind(std::move(required_level_endpoints->client), dispatcher_);

        // TODO(https://fxbug.dev/341104129): connect to topology here instead of injecting the
        // connection in the constructor. Disconnect once no longer needed.
        topology_->AddElement(std::move(schema))
            .Then([this, completer = std::move(completer)](
                      fidl::Result<fpb::Topology::AddElement>& result) mutable {
              if (result.is_error()) {
                FX_LOGS(ERROR) << "Failed to add element to topology: "
                               << result.error_value().FormatDescription();

                std::ignore = element_control_.UnbindMaybeGetEndpoint();
                std::ignore = lessor_.UnbindMaybeGetEndpoint();
                std::ignore = current_level_client_.UnbindMaybeGetEndpoint();
                std::ignore = required_level_client_.UnbindMaybeGetEndpoint();

                completer.complete_error(Error::kBadValue);
                return;
              }

              WatchRequiredLevel();
              completer.complete_ok();
            });
      });

  return bridge.consumer.promise_or(fpromise::error(Error::kLogicError))
      .wrap_with(add_power_element_barrier_);
}

fpromise::promise<fidl::Client<fpb::LeaseControl>, Error> WakeLease::DoAcquireLease() {
  return add_power_element_barrier_.sync().then(
      [this](const fpromise::result<>& result) mutable
          -> fpromise::promise<fidl::Client<fpb::LeaseControl>, Error> {
        if (!lessor_.is_valid()) {
          // Power element addition must have failed.
          FX_LOGS(ERROR) << "Failed to acquire wake lease because Lessor client is not valid.";
          return fpromise::make_result_promise<fidl::Client<fpb::LeaseControl>, Error>(
              fpromise::error(Error::kBadValue));
        }

        return fidl_fpromise::as_promise(lessor_->Lease(kPowerLevelActive))
            .or_else([](const fidl::ErrorsIn<fpb::Lessor::Lease>& error) {
              FX_LOGS(ERROR) << "Failed to acquire wake lease: " << error.FormatDescription();
              return fpromise::make_result_promise<fpb::LessorLeaseResponse, Error>(
                  fpromise::error(Error::kBadValue));
            })
            .and_then([this](fpb::LessorLeaseResponse& result) {
              fidl::Client<fpb::LeaseControl> lease_control(
                  std::move(result.lease_control()), dispatcher_, &lease_control_event_handler_);

              return WaitForRequiredLevelActive().then(
                  [lease_control =
                       std::move(lease_control)](const fpromise::result<>& result) mutable {
                    return fpromise::make_result_promise<fidl::Client<fpb::LeaseControl>, Error>(
                        fpromise::ok(std::move(lease_control)));
                  });
            });
      });
}

void WakeLease::WatchRequiredLevel() {
  required_level_client_->Watch().Then(
      [this](const fidl::Result<fpb::RequiredLevel::Watch>& result) {
        if (result.is_error()) {
          FX_LOGS(ERROR) << "Failed to watch required level: "
                         << result.error_value().FormatDescription();
          return;
        }

        required_level_ = result->required_level();

        current_level_client_->Update(required_level_)
            .Then([this](const fidl::Result<fpb::CurrentLevel::Update>& result) {
              if (result.is_error()) {
                FX_LOGS(ERROR) << "Failed to update current level: "
                               << result.error_value().FormatDescription();
                return;
              }

              // Unblocks all tasks waiting for the required level to become active. Note, promises
              // will re-block themselves if the power level is not active.
              for (fpromise::suspended_task& task : waiting_for_required_level_) {
                task.resume_task();
              }
              waiting_for_required_level_.clear();

              WatchRequiredLevel();
            });
      });
}

fpromise::promise<> WakeLease::WaitForRequiredLevelActive() {
  return fpromise::make_promise([this](fpromise::context& context) -> fpromise::result<> {
    if (required_level_ == kPowerLevelActive) {
      return fpromise::ok();
    }

    waiting_for_required_level_.push_back(context.suspend_task());
    return fpromise::pending();
  });
}

}  // namespace forensics::exceptions::handler
