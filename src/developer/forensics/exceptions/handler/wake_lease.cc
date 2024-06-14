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

#include <utility>

#include "src/developer/forensics/exceptions/constants.h"
#include "src/developer/forensics/utils/errors.h"
#include "src/developer/forensics/utils/promise_timeout.h"
#include "src/lib/fidl/cpp/contrib/fpromise/client.h"

namespace forensics::exceptions::handler {
namespace {

namespace fpb = fuchsia_power_broker;
namespace fps = fuchsia_power_system;

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

fpromise::promise<fidl::Client<fpb::LeaseControl>, Error> WaitForLeaseSatisfied(
    fidl::Client<fpb::LeaseControl> lease_control, fpb::LeaseStatus last_status) {
  return fidl_fpromise::as_promise(lease_control->WatchStatus(last_status))
      .then([lease_control = std::move(lease_control)](
                const fpromise::result<fuchsia_power_broker::LeaseControlWatchStatusResponse,
                                       fidl::Status>& result) mutable
            -> fpromise::promise<fidl::Client<fpb::LeaseControl>, Error> {
        if (result.is_error()) {
          FX_LOGS(ERROR) << "Failed to watch lease status: " << result.error().FormatDescription();

          // We failed to wait for the lease to become satisfied, but we'll optimistically hold onto
          // the lease and hope it becomes satisfied in time to prevent a possible system
          // suspension.
          return fpromise::make_result_promise<fidl::Client<fpb::LeaseControl>, Error>(
              fpromise::ok(std::move(lease_control)));
        }

        if (const fpb::LeaseStatus status = result.value().status();
            status != fpb::LeaseStatus::kSatisfied) {
          return WaitForLeaseSatisfied(std::move(lease_control), status);
        }
        return fpromise::make_result_promise<fidl::Client<fpb::LeaseControl>, Error>(
            fpromise::ok(std::move(lease_control)));
      });
}

}  // namespace

WakeLease::WakeLease(async_dispatcher_t* dispatcher, const std::string& power_element_name,
                     fidl::ClientEnd<fps::ActivityGovernor> sag_client_end,
                     fidl::ClientEnd<fpb::Topology> topology_client_end)
    : dispatcher_(dispatcher),
      power_element_name_(power_element_name),
      add_power_element_called_(false),
      sag_(std::move(sag_client_end), dispatcher_, &sag_event_handler_),
      topology_(std::move(topology_client_end), dispatcher_, &topology_event_handler_) {}

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
                        std::move(endpoints->server), power_element_name_);

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

              return WaitForLeaseSatisfied(std::move(lease_control), fpb::LeaseStatus::kUnknown);
            });
      });
}

}  // namespace forensics::exceptions::handler
