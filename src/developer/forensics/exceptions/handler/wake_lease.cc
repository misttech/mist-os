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
#include "src/lib/fidl/cpp/contrib/fpromise/client.h"

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

WakeLease::WakeLease(async_dispatcher_t* dispatcher, const std::string& power_element_name,
                     fidl::ClientEnd<fps::ActivityGovernor> sag_client_end,
                     fidl::ClientEnd<fpb::Topology> topology_client_end)
    : dispatcher_(dispatcher),
      power_element_name_(power_element_name),
      add_power_element_called_(false),
      sag_event_handler_(std::make_unique<SagEventHandler>()),
      sag_(std::move(sag_client_end), dispatcher_, sag_event_handler_.get()),
      topology_event_handler_(std::make_unique<TopologyEventHandler>()),
      topology_(std::move(topology_client_end), dispatcher_, topology_event_handler_.get()) {}

fpromise::promise<fidl::ClientEnd<fpb::LeaseControl>, Error> WakeLease::Acquire() {
  if (add_power_element_called_) {
    return DoAcquireLease();
  }

  auto self = ptr_factory_.GetWeakPtr();
  return AddPowerElement()
      .and_then([self]() -> fpromise::promise<fidl::ClientEnd<fpb::LeaseControl>, Error> {
        if (!self) {
          return fpromise::make_result_promise<fidl::ClientEnd<fpb::LeaseControl>, Error>(
              fpromise::error(Error::kLogicError));
        }

        return self->DoAcquireLease();
      })
      .or_else([](const Error& add_element_error) {
        return fpromise::make_result_promise<fidl::ClientEnd<fpb::LeaseControl>, Error>(
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
  auto self = ptr_factory_.GetWeakPtr();
  sag_->GetPowerElements().Then(
      [self, completer = std::move(bridge.completer)](
          fidl::Result<fps::ActivityGovernor::GetPowerElements>& result) mutable {
        if (!self) {
          return;
        }

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
                        std::move(endpoints->server), self->power_element_name_);

        // TODO(https://fxbug.dev/341104129): connect to topology here instead of injecting the
        // connection in the constructor. Disconnect once no longer needed.
        self->topology_->AddElement(std::move(schema))
            .Then([self, completer = std::move(completer),
                   client_end = std::move(endpoints->client)](
                      fidl::Result<fpb::Topology::AddElement>& result) mutable {
              if (!self) {
                return;
              }

              if (result.is_error()) {
                FX_LOGS(ERROR) << "Failed to add element to topology: "
                               << result.error_value().FormatDescription();
                completer.complete_error(Error::kBadValue);
                return;
              }

              self->element_control_channel_ = std::move(result.value().element_control_channel());
              self->lessor_ = fidl::Client<fpb::Lessor>(std::move(client_end), self->dispatcher_);
              completer.complete_ok();
            });
      });

  return bridge.consumer.promise_or(fpromise::error(Error::kLogicError))
      .wrap_with(add_power_element_barrier_);
}

fpromise::promise<fidl::ClientEnd<fpb::LeaseControl>, Error> WakeLease::DoAcquireLease() {
  auto self = ptr_factory_.GetWeakPtr();
  return add_power_element_barrier_.sync().then(
      [self](const fpromise::result<>& result) mutable
      -> fpromise::promise<fidl::ClientEnd<fpb::LeaseControl>, Error> {
        if (!self) {
          return fpromise::make_result_promise<fidl::ClientEnd<fpb::LeaseControl>, Error>(
              fpromise::error(Error::kLogicError));
        }

        if (!self->lessor_.is_valid()) {
          // Power element addition must have failed.
          FX_LOGS(ERROR) << "Failed to acquire wake lease because Lessor client is not valid.";
          return fpromise::make_result_promise<fidl::ClientEnd<fpb::LeaseControl>, Error>(
              fpromise::error(Error::kBadValue));
        }

        return fidl_fpromise::as_promise(self->lessor_->Lease(kPowerLevelActive))
            .and_then([](fpb::LessorLeaseResponse& result) {
              // TODO(https://fxbug.dev/341104129): Call LeaseControl::WatchStatus to wait until
              // LeaseStatus is SATISFIED.
              return fpromise::make_result_promise<fidl::ClientEnd<fpb::LeaseControl>,
                                                   fidl::ErrorsIn<fpb::Lessor::Lease>>(
                  fpromise::ok(std::move(result.lease_control())));
            })
            .or_else([](const fidl::ErrorsIn<fpb::Lessor::Lease>& error) {
              FX_LOGS(ERROR) << "Failed to acquire wake lease: " << error.FormatDescription();
              return fpromise::make_result_promise<fidl::ClientEnd<fpb::LeaseControl>, Error>(
                  fpromise::error(Error::kBadValue));
            });
      });
}

}  // namespace forensics::exceptions::handler
