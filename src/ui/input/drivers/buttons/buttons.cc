// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "buttons.h"

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_export.h>

#include <fbl/alloc_checker.h>

namespace buttons {

namespace {

struct MetadataConfig {
  fbl::Array<buttons_button_config_t> buttons;
  fbl::Array<buttons_gpio_config_t> gpios;
};

zx::result<MetadataConfig> ParseMetadata(
    const fidl::VectorView<::fuchsia_driver_compat::wire::Metadata>& metadata) {
  MetadataConfig metadata_config;
  fbl::AllocChecker ac;

  for (const auto& m : metadata) {
    if (m.type == DEVICE_METADATA_BUTTONS_BUTTONS) {
      size_t size;
      auto status = m.data.get_prop_content_size(&size);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to get_prop_content_size: %s", zx_status_get_string(status));
        return zx::error(status);
      }
      if (size % sizeof(buttons_button_config_t)) {
        FDF_LOG(ERROR, "Bad button metadata size: %zu", size);
        return zx::error(ZX_ERR_BAD_STATE);
      }
      size_t n_buttons = size / sizeof(buttons_button_config_t);
      auto buttons = fbl::MakeArray<buttons_button_config_t>(&ac, n_buttons);
      if (!ac.check()) {
        return zx::error(ZX_ERR_NO_MEMORY);
      }
      status = m.data.read(&buttons[0], 0, size);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to read button metadata: %s", zx_status_get_string(status));
        return zx::error(status);
      }
      metadata_config.buttons = std::move(buttons);
    } else if (m.type == DEVICE_METADATA_BUTTONS_GPIOS) {
      size_t size;
      auto status = m.data.get_prop_content_size(&size);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to get_prop_content_size: %s", zx_status_get_string(status));
        return zx::error(status);
      }
      if (size % sizeof(buttons_gpio_config_t)) {
        FDF_LOG(ERROR, "Bad GPIO metadata size: %zu", size);
        return zx::error(ZX_ERR_BAD_STATE);
      }
      size_t n_gpios = size / sizeof(buttons_gpio_config_t);
      auto gpios = fbl::MakeArray<buttons_gpio_config_t>(&ac, n_gpios);
      if (!ac.check()) {
        return zx::error(ZX_ERR_NO_MEMORY);
      }
      status = m.data.read(&gpios[0], 0, size);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to read GPIO metadata: %s", zx_status_get_string(status));
        return zx::error(status);
      }
      metadata_config.gpios = std::move(gpios);
    }
  }
  if (metadata_config.buttons.empty() || metadata_config.gpios.empty()) {
    FDF_LOG(ERROR, "Failed to get metadata, %zu buttons and %zu GPIO found",
            metadata_config.buttons.size(), metadata_config.gpios.size());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(std::move(metadata_config));
}

}  // namespace

zx::result<> Buttons::Start() {
  // Get metadata.
  zx::result compat_result = incoming()->Connect<fuchsia_driver_compat::Service::Device>();
  if (compat_result.is_error()) {
    FDF_LOG(ERROR, "Failed to open compat service: %s", compat_result.status_string());
    return compat_result.take_error();
  }
  auto compat = fidl::WireSyncClient(std::move(compat_result.value()));
  if (!compat.is_valid()) {
    FDF_LOG(ERROR, "Failed to get compat acccess");
    return zx::error(ZX_ERR_NO_RESOURCES);
  }
  auto metadata = compat->GetMetadata();
  if (!metadata.ok()) {
    FDF_LOG(ERROR, "Failed to send GetMetadata request: %s", metadata.FormatDescription().data());
    return zx::error(metadata.status());
  }
  if (metadata->is_error()) {
    FDF_LOG(ERROR, "Failed to GetMetadata: %s", zx_status_get_string(metadata->error_value()));
    return metadata->take_error();
  }
  auto metadata_config = ParseMetadata(metadata.value()->metadata);
  if (metadata_config.is_error()) {
    FDF_LOG(ERROR, "Failed to ParseMetadata: %s",
            zx_status_get_string(metadata_config.error_value()));
    return metadata_config.take_error();
  }

  // Prepare gpios array.
  fbl::AllocChecker ac;
  auto gpios = fbl::Array(new (&ac) ButtonsDevice::Gpio[metadata_config->gpios.size()],
                          metadata_config->gpios.size());
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  for (uint32_t i = 0; i < metadata_config->gpios.size(); ++i) {
    const char* name;
    switch (metadata_config->buttons[i].id) {
      case BUTTONS_ID_VOLUME_UP:
        name = "volume-up";
        break;
      case BUTTONS_ID_VOLUME_DOWN:
        name = "volume-down";
        break;
      case BUTTONS_ID_FDR:
        name = "volume-both";
        break;
      case BUTTONS_ID_MIC_MUTE:
      case BUTTONS_ID_MIC_AND_CAM_MUTE:
        name = "mic-privacy";
        break;
      case BUTTONS_ID_CAM_MUTE:
        name = "cam-mute";
        break;
      case BUTTONS_ID_POWER:
        name = "power";
        break;
      case BUTTONS_ID_PLAY_PAUSE:
        name = "play-pause";
        break;
      case BUTTONS_ID_KEY_A:
        name = "key-a";
        break;
      case BUTTONS_ID_KEY_M:
        name = "key-m";
        break;
      case BUTTONS_ID_FUNCTION:
        name = "function";
        break;
      default:
        FDF_LOG(ERROR, "Unknown Button id: %d", metadata_config->buttons[i].id);
        return zx::error(ZX_ERR_NOT_SUPPORTED);
    };
    zx::result gpio_client = incoming()->Connect<fuchsia_hardware_gpio::Service::Device>(name);
    if (gpio_client.is_error() || !gpio_client->is_valid()) {
      FDF_LOG(ERROR, "Connect to GPIO %s failed: %s", name, gpio_client.status_string());
      return gpio_client.take_error();
    }
    gpios[i].client.Bind(std::move(gpio_client.value()));
    gpios[i].config = metadata_config->gpios[i];
  }

  fidl::ClientEnd<fuchsia_power_system::ActivityGovernor> sag_client;
  if (config_.suspend_enabled()) {
    auto sag_result = incoming()->Connect<fuchsia_power_system::ActivityGovernor>();
    if (sag_result.is_ok() && sag_result->is_valid()) {
      sag_client = std::move(sag_result.value());
    } else {
      FDF_LOG(
          ERROR,
          "Failed to connect to fuchsia.power.system.ActivityGovernor: %s; system may incorrectly enter suspend.",
          sag_result.status_string());
    }
  }

  device_ = std::make_unique<buttons::ButtonsDevice>(
      dispatcher(), std::move(metadata_config->buttons), std::move(gpios), std::move(sag_client));

  auto result = outgoing()->component().AddUnmanagedProtocol<fuchsia_input_report::InputDevice>(
      input_report_bindings_.CreateHandler(device_.get(), dispatcher(),
                                           fidl::kIgnoreBindingClosure),
      kDeviceName);
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add input report service: %s", result.status_string());
    return result.take_error();
  }

  if (zx::result result = CreateDevfsNode(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to export to devfs: %s", result.status_string());
    return result.take_error();
  }

  return zx::ok();
}

void Buttons::PrepareStop(fdf::PrepareStopCompleter completer) {
  device_->ShutDown();
  completer(zx::ok());
}

zx::result<> Buttons::CreateDevfsNode() {
  fidl::Arena arena;
  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    return connector.take_error();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .class_name("input-report");

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, kDeviceName)
                  .devfs_args(devfs.Build())
                  .Build();

  auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed to create node endpoints: %s",
                node_endpoints.status_string());

  fidl::WireResult result = fidl::WireCall(node())->AddChild(
      args, std::move(controller_endpoints.server), std::move(node_endpoints->server));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child %s", result.status_string());
    return zx::error(result.status());
  }
  controller_.Bind(std::move(controller_endpoints.client));
  node_.Bind(std::move(node_endpoints->client));
  return zx::ok();
}

}  // namespace buttons

FUCHSIA_DRIVER_EXPORT(buttons::Buttons);
