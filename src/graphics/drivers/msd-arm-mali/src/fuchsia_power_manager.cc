// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fuchsia_power_manager.h"

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <lib/driver/power/cpp/element-description-builder.h>
#include <lib/driver/power/cpp/power-support.h>
#include <lib/fit/defer.h>

FuchsiaPowerManager::FuchsiaPowerManager(Owner* owner) : owner_(owner) {}

bool FuchsiaPowerManager::Initialize(ParentDevice* parent_device) {
  if (!parent_device->incoming()) {
    return false;
  }

  auto result = parent_device->GetPowerConfiguration();
  if (!result.ok()) {
    MAGMA_LOG(ERROR, "Call to GetPowerConfiguration failed");
    return false;
  }

  if (result->is_error()) {
    MAGMA_LOG(ERROR, "Call to GetPowerConfiguration returned error %d", result->error_value());
    return false;
  }

  auto power_broker = parent_device->incoming()->Connect<fuchsia_power_broker::Topology>();
  if (power_broker.is_error() || !power_broker->is_valid()) {
    MAGMA_LOG(ERROR, "Failed to connect to power broker: %s", power_broker.status_string());
    return false;
  }

  for (auto& config : result->value()->config) {
    if (!config.has_element()) {
      continue;
    }
    auto& element = config.element();
    if (!element.has_name()) {
      continue;
    }
    std::string name{element.name().get()};
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

    active_power_dep_tokens_.push_back(std::move(description.active_token_));
    passive_power_dep_tokens_.push_back(std::move(description.passive_token_));

    if (config.element().name().get() == kHardwarePowerElementName) {
      hardware_power_element_control_client_end_ =
          std::move(result.value().element_control_channel());
      hardware_power_lessor_client_ = fidl::WireSyncClient<fuchsia_power_broker::Lessor>(
          std::move(description.lessor_client_.value()));
      hardware_power_current_level_client_ =
          fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel>(
              std::move(description.current_level_client_.value()));
      hardware_power_required_level_client_ = fidl::WireClient<fuchsia_power_broker::RequiredLevel>(
          std::move(description.required_level_client_.value()),
          fdf::Dispatcher::GetCurrent()->async_dispatcher());

    } else {
      MAGMA_LOG(INFO, "Got unexpected power element %s", name.c_str());
    }
  }
  if (!hardware_power_lessor_client_.is_valid()) {
    MAGMA_LOG(INFO, "No %s element, disabling power framework", kHardwarePowerElementName);
    return false;
  }
  fidl::ClientEnd<fuchsia_power_broker::LeaseControl> lease_control_client_end;
  zx_status_t status = AcquireLease(hardware_power_lessor_client_, lease_control_client_end);
  if (status != ZX_OK) {
    MAGMA_LOG(ERROR, "Failed to acquire lease on hardware power: %s", zx_status_get_string(status));
    return false;
  }
  hardware_power_lease_control_client_ = fidl::WireClient<fuchsia_power_broker::LeaseControl>(
      std::move(lease_control_client_end), fdf::Dispatcher::GetCurrent()->async_dispatcher());
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
        MAGMA_LOG(INFO, "Starting transition to power level %d", required_level);

        bool enabled = required_level == kPoweredUpPowerLevel;
        owner_->SetPowerState(enabled, [this](bool powered_on) {
          uint8_t new_level = powered_on ? kPoweredUpPowerLevel : kPoweredDownPowerLevel;
          MAGMA_LOG(INFO, "Finished transition to power level %d", new_level);
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
