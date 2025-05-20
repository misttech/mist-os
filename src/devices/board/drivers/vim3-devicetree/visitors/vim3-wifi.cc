// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vim3-wifi.h"

#include <fidl/fuchsia.hardware.gpio/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/logger.h>

#include <bind/fuchsia/broadcom/platform/sdio/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/sdio/cpp/bind.h>
#include <bind/fuchsia/sdio/cpp/bind.h>
#include <ddk/metadata/buttons.h>
#include <wifi/wifi-config.h>

namespace vim3_dt {

zx::result<> Vim3WifiVisitor::DriverVisit(fdf_devicetree::Node& node,
                                          const devicetree::PropertyDecoder& decoder) {
  const wifi_config_t wifi_config = {
      .oob_irq_mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      .clm_needed = false,
      .iovar_table =
          {
              {IOVAR_CMD_TYPE, {.iovar_cmd = BRCMF_C_SET_PM}, 0},
              {IOVAR_LIST_END_TYPE, {{0}}, 0},
          },
      .cc_table =
          {
              {"WW", 0}, {"AU", 0}, {"CA", 0}, {"US", 0}, {"GB", 0}, {"BE", 0}, {"BG", 0},
              {"CZ", 0}, {"DK", 0}, {"DE", 0}, {"EE", 0}, {"IE", 0}, {"GR", 0}, {"ES", 0},
              {"FR", 0}, {"HR", 0}, {"IT", 0}, {"CY", 0}, {"LV", 0}, {"LT", 0}, {"LU", 0},
              {"HU", 0}, {"MT", 0}, {"NL", 0}, {"AT", 0}, {"PL", 0}, {"PT", 0}, {"RO", 0},
              {"SI", 0}, {"SK", 0}, {"FI", 0}, {"SE", 0}, {"EL", 0}, {"IS", 0}, {"LI", 0},
              {"TR", 0}, {"CH", 0}, {"NO", 0}, {"JP", 0}, {"", 0},
          },
  };

  fuchsia_hardware_platform_bus::Metadata wifi_config_metadata = {{
      .id = std::to_string(DEVICE_METADATA_WIFI_CONFIG),
      .data = std::vector<uint8_t>(
          reinterpret_cast<const uint8_t*>(&wifi_config),
          reinterpret_cast<const uint8_t*>(&wifi_config) + sizeof(wifi_config)),
  }};

  node.AddMetadata(wifi_config_metadata);

  constexpr uint32_t kSdioFunctionCount = 2;
  for (uint32_t i = 1; i <= kSdioFunctionCount; i++) {
    auto sdio_node = fuchsia_driver_framework::ParentSpec2{
        {.bind_rules =
             {
                 fdf::MakeAcceptBindRule2(bind_fuchsia::PROTOCOL,
                                          bind_fuchsia_sdio::BIND_PROTOCOL_DEVICE),
                 fdf::MakeAcceptBindRule2(
                     bind_fuchsia::SDIO_VID,
                     bind_fuchsia_broadcom_platform_sdio::BIND_SDIO_VID_BROADCOM),
                 fdf::MakeAcceptBindRule2(
                     bind_fuchsia::SDIO_PID,
                     bind_fuchsia_broadcom_platform_sdio::BIND_SDIO_PID_BCM4359),
                 fdf::MakeAcceptBindRule2(bind_fuchsia::SDIO_FUNCTION, i),
             },
         .properties = {
             fdf::MakeProperty2(bind_fuchsia_hardware_sdio::SERVICE,
                                bind_fuchsia_hardware_sdio::SERVICE_ZIRCONTRANSPORT),
             fdf::MakeProperty2(bind_fuchsia::SDIO_FUNCTION, i),
         }}};
    node.AddNodeSpec(sdio_node);
  }

  return zx::ok();
}

}  // namespace vim3_dt
