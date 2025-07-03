// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "magma_power_manager.h"

#include <lib/magma/util/dlog.h>
#include <lib/magma/util/macros.h>

namespace msd {
namespace {

constexpr int kPoweredUpPowerLevel = 1;

zx::result<fidl::ClientEnd<fuchsia_power_broker::LeaseControl>> AcquireLease(
    const fidl::WireSyncClient<fuchsia_power_broker::Lessor>& lessor_client) {
  const fidl::WireResult result = lessor_client->Lease(kPoweredUpPowerLevel);
  if (!result.ok()) {
    MAGMA_LOG(ERROR, "Call to Lease failed: %s", result.status_string());
    return zx::error(result.status());
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
    return zx::error(ZX_ERR_INTERNAL);
  }
  if (!result->value()->lease_control.is_valid()) {
    MAGMA_LOG(ERROR, "Lease returned invalid lease control client end.");
    return zx::error(ZX_ERR_BAD_STATE);
  }

  return zx::ok(std::move(result->value()->lease_control));
}

// TODO(b/358361345): Use //sdk/lib/driver/platform-device/cpp to retrieve power configuration
// once it supports it.
zx::result<std::vector<fdf_power::PowerElementConfiguration>> GetPowerConfiguration(
    fidl::ClientEnd<fuchsia_hardware_platform_device::Device>& pdev) {
  fidl::WireResult result = fidl::WireCall(pdev)->GetPowerConfiguration();
  if (!result.ok()) {
    MAGMA_LOG(ERROR, "Failed to send GetPowerConfiguration request: %s", result.status_string());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    MAGMA_LOG(ERROR, "Failed to get power configuration: %s",
              zx_status_get_string(result->error_value()));
    return result->take_error();
  }

  std::vector<fdf_power::PowerElementConfiguration> configs;
  for (const auto& wire : result.value()->config) {
    fuchsia_hardware_power::PowerElementConfiguration natural = fidl::ToNatural(wire);

    zx::result config = fdf_power::PowerElementConfiguration::FromFidl(natural);
    if (config.is_error()) {
      MAGMA_LOG(ERROR, "Failed to parse power configuration: %s", config.status_string());
      return config.take_error();
    }

    configs.push_back(std::move(config.value()));
  }

  return zx::ok(std::move(configs));
}

}  // namespace

void PowerElementRunner::SetLevel(fuchsia_power_broker::ElementRunnerSetLevelRequest& request,
                                  SetLevelCompleter::Sync& completer) {
  inspect_.required_power_level.Set(request.level());
  owner_.PostPowerStateChange(request.level(),
                              [this, new_level = request.level(),
                               completer = completer.ToAsync()](zx_status_t status) mutable {
                                if (status == ZX_OK) {
                                  inspect_.current_power_level.Set(new_level);
                                }
                                MAGMA_DLOG("Changing the power state to %d finished: %s", new_level,
                                           zx_status_get_string(status));
                                completer.Reply();
                              });
}

void PowerElementRunner::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_power_broker::ElementRunner> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  MAGMA_LOG(ERROR, "PowerElementRunner received unknown method %lu", metadata.method_ordinal);
}

zx_status_t PowerElementRunner::EnablePower() {
  if (lease_control_.is_valid()) {
    return ZX_OK;
  }

  zx::result lease = AcquireLease(lessor_);
  if (lease.is_error()) {
    MAGMA_LOG(ERROR, "Failed to acquire lease on hardware power: %s", lease.status_string());
    return lease.status_value();
  }

  lease_control_ = std::move(lease).value();
  inspect_.power_lease_active.Set(true);
  return ZX_OK;
}

zx_status_t PowerElementRunner::DisablePower() {
  if (!lease_control_.is_valid()) {
    return ZX_ERR_BAD_STATE;
  }
  lease_control_.reset();
  inspect_.power_lease_active.Set(false);
  return ZX_OK;
}

zx::result<std::unique_ptr<PowerElementRunner>> PowerElementRunner::Create(
    fidl::ClientEnd<fuchsia_hardware_platform_device::Device>& pdev, fdf::Namespace& incoming,
    const char* pdev_element_name, PowerElementRunner::Owner& owner) {
  zx::result power_broker = incoming.Connect<fuchsia_power_broker::Topology>();
  if (power_broker.is_error() || !power_broker->is_valid()) {
    MAGMA_LOG(ERROR, "Failed to connect to power broker: %s", power_broker.status_string());
    return zx::error(ZX_ERR_INTERNAL);
  }

  zx::result power_config = GetPowerConfiguration(pdev);
  if (power_config.is_error()) {
    return zx::error(ZX_ERR_INTERNAL);
  }

  zx::result power_info = CreatePowerInfo(incoming, power_broker.value(),
                                          std::move(power_config).value(), pdev_element_name);
  if (power_info.is_error()) {
    return zx::error(ZX_ERR_INTERNAL);
  }

  auto element_runner = std::make_unique<PowerElementRunner>(
      std::move(power_info->lessor), std::move(power_info->element_server),
      std::move(power_info->element_control), owner);
  return zx::ok(std::move(element_runner));
}

void PowerElementRunner::SetupInspect(inspect::Node& node) {
  inspect_.power_lease_active = node.CreateBool("power_lease_active", false);
  inspect_.required_power_level = node.CreateUint("required_power_level", 0);
  inspect_.current_power_level = node.CreateUint("current_power_level", 0);
}

zx::result<PowerInfo> CreatePowerInfo(
    fdf::Namespace& incoming, fidl::ClientEnd<fuchsia_power_broker::Topology>& power_broker,
    const std::vector<fdf_power::PowerElementConfiguration>& configs, const char* name) {
  for (const auto& config : configs) {
    fit::result tokens = fdf_power::GetDependencyTokens(incoming, config);
    if (tokens.is_error()) {
      MAGMA_DLOG(
          "Failed to get power dependency tokens: %s. Perhaps the product does not have Power "
          "Framework?",
          fdf_power::ErrorToString(tokens.error_value()));
      return zx::error(ZX_ERR_INTERNAL);
    }

    fidl::Endpoints<fuchsia_power_broker::ElementRunner> element_runner =
        fidl::Endpoints<fuchsia_power_broker::ElementRunner>::Create();
    fdf_power::ElementDesc description =
        fdf_power::ElementDescBuilder(config, std::move(tokens.value()))
            .SetElementRunner(std::move(element_runner.client))
            .Build();
    fit::result result = fdf_power::AddElement(power_broker, description);
    if (result.is_error()) {
      MAGMA_LOG(ERROR, "Failed to add power element: %u",
                static_cast<uint8_t>(result.error_value()));
      return zx::error(ZX_ERR_INTERNAL);
    }

    if (config.element.name != name) {
      MAGMA_LOG(WARNING, "Got unexpected power element %s", config.element.name.c_str());
    }
    return zx::ok(PowerInfo{
        .lessor = fidl::WireSyncClient<fuchsia_power_broker::Lessor>(
            std::move(description.lessor_client.value())),
        .element_control = std::move(description.element_control_client.value()),
        .element_server = std::move(element_runner.server),
    });
  }
  MAGMA_LOG(WARNING, "No element '%s', disabling power framework", name);
  return zx::error(ZX_ERR_INTERNAL);
}
}  // namespace msd
