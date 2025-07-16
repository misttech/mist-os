// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fuchsia_power_manager.h"

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <lib/driver/power/cpp/element-description-builder.h>
#include <lib/driver/power/cpp/power-support.h>
#include <lib/driver/power/cpp/types.h>
#include <lib/fit/defer.h>

FuchsiaPowerManager::FuchsiaPowerManager(Owner* owner)
    : owner_(owner), hardware_power_element_runner_server_(*this) {}

bool FuchsiaPowerManager::Initialize(ParentDevice* parent_device, inspect::Node& node) {
  if (!parent_device->incoming()) {
    return false;
  }

  power_lease_active_ = node.CreateBool("power_lease_active", false);
  required_power_level_ = node.CreateUint("required_power_level", 0);
  current_power_level_ = node.CreateUint("current_power_level", 0);

  zx::result configs = parent_device->GetPowerConfiguration();
  if (configs.is_error()) {
    MAGMA_LOG(ERROR, "Failed to get power configuration: %s", configs.status_string());
    return false;
  }

  auto power_broker = parent_device->incoming()->Connect<fuchsia_power_broker::Topology>();
  if (power_broker.is_error() || !power_broker->is_valid()) {
    MAGMA_LOG(ERROR, "Failed to connect to power broker: %s", power_broker.status_string());
    return false;
  }

  zx::event hardware_element_assertive_token;

  for (const auto& config : configs.value()) {
    auto tokens = fdf_power::GetDependencyTokens(*parent_device->incoming(), config);
    if (tokens.is_error()) {
      MAGMA_LOG(
          ERROR,
          "Failed to get power dependency tokens: %s. Perhaps the product does not have Power "
          "Framework?",
          fdf_power::ErrorToString(tokens.error_value()));
      return false;
    }

    fidl::Endpoints<fuchsia_power_broker::ElementRunner> element_runner =
        fidl::Endpoints<fuchsia_power_broker::ElementRunner>::Create();
    fdf_power::ElementDesc description =
        fdf_power::ElementDescBuilder(config, std::move(tokens.value()))
            .SetElementRunner(std::move(element_runner.client))
            .Build();
    auto result = fdf_power::AddElement(power_broker.value(), description);
    if (result.is_error()) {
      MAGMA_LOG(ERROR, "Failed to add power element: %u",
                static_cast<uint8_t>(result.error_value()));
      return false;
    }

    if (config.element.name == kHardwarePowerElementName) {
      hardware_power_element_control_client_end_ =
          std::move(description.element_control_client.value());
      hardware_power_lessor_client_ = fidl::WireSyncClient<fuchsia_power_broker::Lessor>(
          std::move(description.lessor_client.value()));
      description.assertive_token.duplicate(ZX_RIGHT_SAME_RIGHTS,
                                            &hardware_element_assertive_token);
      hardware_power_element_runner_server_bindref_ = fidl::BindServer(
          fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(element_runner.server),
          &hardware_power_element_runner_server_);
    } else {
      MAGMA_LOG(INFO, "Got unexpected power element %s", config.element.name.c_str());
    }
    assertive_power_dep_tokens_.push_back(std::move(description.assertive_token));
    opportunistic_power_dep_tokens_.push_back(std::move(description.opportunistic_token));
  }
  if (!hardware_power_lessor_client_.is_valid()) {
    MAGMA_LOG(INFO, "No %s element, disabling power framework", kHardwarePowerElementName);
    return false;
  }

  {
    fdf_power::PowerElementConfiguration config = {
        .element = {.name = kOnReadyForWorkPowerElementName,
                    .levels =
                        {
                            {.level = 0,
                             .name = "off",
                             .transitions = {{.target_level = 1, .latency_us = 0}}},
                            {.level = 1,
                             .name = "on",
                             .transitions = {{.target_level = 0, .latency_us = 0}}},
                        }},
        .dependencies = {{
            .child = kOnReadyForWorkPowerElementName,
            .parent = fdf_power::ParentElement::WithInstanceName(kHardwarePowerElementName),
            .level_deps = {{.child_level = 1, .parent_level = kPoweredUpPowerLevel}},
            .strength = fdf_power::RequirementType::kAssertive,
        }}};

    fdf_power::TokenMap tokens;
    tokens[fdf_power::ParentElement::WithInstanceName(kHardwarePowerElementName)] =
        std::move(hardware_element_assertive_token);

    fdf_power::ElementDesc description =
        fdf_power::ElementDescBuilder(config, std::move(tokens)).Build();
    auto result = fdf_power::AddElement(power_broker.value(), description);
    fidl::Endpoints<fuchsia_power_broker::ElementRunner> element_runner =
        fidl::Endpoints<fuchsia_power_broker::ElementRunner>::Create();
    on_ready_for_work_element_runner_server_bindref_ = fidl::BindServer(
        fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(element_runner.server),
        &on_ready_for_work_element_runner_server_);
    if (result.is_error()) {
      MAGMA_LOG(ERROR, "Failed to add power element: %u",
                static_cast<uint8_t>(result.error_value()));
      return false;
    }
    on_ready_for_work_token_ = std::move(description.assertive_token);
    on_ready_for_work_control_client_end_ = std::move(description.element_control_client.value());
  }

  MAGMA_LOG(INFO, "Using power framework to manage GPU power");

  return true;
}

zx_status_t FuchsiaPowerManager::AcquireLease(
    const fidl::WireSyncClient<fuchsia_power_broker::Lessor>& lessor_client,
    fidl::ClientEnd<fuchsia_power_broker::LeaseControl>& lease_control_client_end) {
  if (lease_control_client_end.is_valid()) {
    return ZX_ERR_ALREADY_BOUND;
  }

  const fidl::WireResult result = lessor_client->Lease(kPoweredUpPowerLevel);
  if (!result.ok()) {
    MAGMA_LOG(ERROR, "Call to Lease failed: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    switch (result->error_value()) {
      case fuchsia_power_broker::LeaseError::kInternal:
        MAGMA_LOG(ERROR, "Lease returned internal error.");
        break;
      case fuchsia_power_broker::LeaseError::kNotAuthorized:
        MAGMA_LOG(ERROR, "Lease returned not authorized error.");
        break;
      default:
        MAGMA_LOG(ERROR, "Lease returned unknown error.");
        break;
    }
    return ZX_ERR_INTERNAL;
  }
  if (!result->value()->lease_control.is_valid()) {
    MAGMA_LOG(ERROR, "Lease returned invalid lease control client end.");
    return ZX_ERR_BAD_STATE;
  }
  lease_control_client_end = std::move(result->value()->lease_control);
  return ZX_OK;
}

void FuchsiaPowerManager::HardwareElementRunner::SetLevel(
    fuchsia_power_broker::ElementRunnerSetLevelRequest& request,
    SetLevelCompleter::Sync& set_level_completer) {
  uint8_t required_level = request.level();
  parent_.required_power_level_.Set(required_level);

  bool enabled = required_level == kPoweredUpPowerLevel;
  parent_.owner_->PostPowerStateChange(
      enabled, [this, completer = set_level_completer.ToAsync()](bool powered_on) mutable {
        uint8_t new_level = powered_on ? kPoweredUpPowerLevel : kPoweredDownPowerLevel;
        parent_.current_power_level_.Set(new_level);
        completer.Reply();
      });
}

void FuchsiaPowerManager::HardwareElementRunner::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_power_broker::ElementRunner> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  MAGMA_LOG(ERROR, "ElementRunner received unknown method %lu", metadata.method_ordinal);
}

void FuchsiaPowerManager::OnReadyForWorkElementRunner::SetLevel(
    fuchsia_power_broker::ElementRunnerSetLevelRequest& request,
    SetLevelCompleter::Sync& set_level_completer) {
  set_level_completer.Reply();
}

void FuchsiaPowerManager::OnReadyForWorkElementRunner::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_power_broker::ElementRunner> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  MAGMA_LOG(ERROR, "ElementRunner received unknown method %lu", metadata.method_ordinal);
}

TimeoutSource::Clock::time_point FuchsiaPowerManager::GetCurrentTimeoutPoint() {
  if (!LeaseIsRequested()) {
    return Clock::time_point::max();
  }
  return owner_->GetPowerManager()->GetGpuPowerdownTimeout();
}

bool FuchsiaPowerManager::EnablePower() {
  if (lease_control_client_end_.is_valid()) {
    return true;
  }

  fidl::ClientEnd<fuchsia_power_broker::LeaseControl> lease_control_client_end;
  zx_status_t status = AcquireLease(hardware_power_lessor_client_, lease_control_client_end);
  if (status != ZX_OK) {
    MAGMA_LOG(ERROR, "Failed to acquire lease on hardware power: %s", zx_status_get_string(status));
    return false;
  }

  lease_control_client_end_ = std::move(lease_control_client_end);
  power_lease_active_.Set(true);
  return true;
}

bool FuchsiaPowerManager::DisablePower() {
  lease_control_client_end_.reset();
  power_lease_active_.Set(false);
  return true;
}

bool FuchsiaPowerManager::LeaseIsRequested() { return lease_control_client_end_.is_valid(); }

FuchsiaPowerManager::PowerGoals FuchsiaPowerManager::GetPowerGoals() {
  PowerGoals goals;
  on_ready_for_work_token_.duplicate(ZX_RIGHT_SAME_RIGHTS, &goals.on_ready_for_work_token);

  return goals;
}
