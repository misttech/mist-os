// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fuchsia_power_manager.h"

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <lib/driver/power/cpp/element-description-builder.h>
#include <lib/driver/power/cpp/power-support.h>
#include <lib/driver/power/cpp/types.h>
#include <lib/fit/defer.h>

FuchsiaPowerManager::FuchsiaPowerManager(Owner* owner) : owner_(owner) {}

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
          "Failed to get power dependency tokens: %u. Perhaps the product does not have Power "
          "Framework?",
          static_cast<uint8_t>(tokens.error_value()));
      return false;
    }

    fdf_power::ElementDesc description =
        fdf_power::ElementDescBuilder(config, std::move(tokens.value())).Build();
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
      hardware_power_current_level_client_ =
          fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel>(
              std::move(description.current_level_client.value()));
      hardware_power_required_level_client_ = fidl::WireClient<fuchsia_power_broker::RequiredLevel>(
          std::move(description.required_level_client.value()),
          fdf::Dispatcher::GetCurrent()->async_dispatcher());
      description.assertive_token.duplicate(ZX_RIGHT_SAME_RIGHTS,
                                            &hardware_element_assertive_token);

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
    if (result.is_error()) {
      MAGMA_LOG(ERROR, "Failed to add power element: %u",
                static_cast<uint8_t>(result.error_value()));
      return false;
    }
    on_ready_for_work_token_ = std::move(description.assertive_token);
    on_ready_for_work_control_client_end_ = std::move(description.element_control_client.value());
    on_ready_for_work_runner_.emplace(
        kOnReadyForWorkPowerElementName, std::move(description.required_level_client.value()),
        std::move(description.current_level_client.value()),
        [](uint8_t new_level) { return fit::ok(new_level); },
        [](fdf_power::ElementRunnerError error) {},
        fdf::Dispatcher::GetCurrent()->async_dispatcher());
    on_ready_for_work_runner_->RunPowerElement();
  }

  CheckRequiredLevel();
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

void FuchsiaPowerManager::CheckRequiredLevel() {
  fidl::Arena<> arena;
  hardware_power_required_level_client_.buffer(arena)->Watch().Then(
      [this](fidl::WireUnownedResult<fuchsia_power_broker::RequiredLevel::Watch>& result) {
        auto defer = fit::defer([&]() { CheckRequiredLevel(); });
        if (!result.ok()) {
          // TODO(https://fxbug.dev/340219979): Handle failures without spinning.
          MAGMA_LOG(ERROR, "Call to Watch failed %s", result.status_string());
          defer.cancel();
          return;
        }
        if (result->is_error()) {
          // TODO(https://fxbug.dev/340219979): Handle failures without spinning.
          MAGMA_LOG(ERROR, "Watch returned error %d", static_cast<uint32_t>(result->error_value()));
          defer.cancel();
          return;
        }
        uint8_t required_level = result->value()->required_level;
        required_power_level_.Set(required_level);

        bool enabled = required_level == kPoweredUpPowerLevel;
        owner_->SetPowerState(enabled, [this](bool powered_on) {
          uint8_t new_level = powered_on ? kPoweredUpPowerLevel : kPoweredDownPowerLevel;
          current_power_level_.Set(new_level);
          auto result = hardware_power_current_level_client_->Update(new_level);
          if (!result.ok()) {
            MAGMA_LOG(ERROR, "Call to Update failed: %s", result.status_string());
          } else if (result->is_error()) {
            MAGMA_LOG(ERROR, "Update returned failure %d",
                      static_cast<uint32_t>(result->error_value()));
          }
        });
      });
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
