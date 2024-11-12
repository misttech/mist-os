// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdmmc-visitor.h"

#include <fidl/fuchsia.hardware.sdmmc/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <bind/fuchsia/cpp/bind.h>

#include "lib/driver/devicetree/manager/visitor.h"

namespace sdmmc_dt {

SdmmcVisitor::SdmmcVisitor() {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint32Property>(kMaxFrequency));
  properties.emplace_back(std::make_unique<fdf_devicetree::BoolProperty>(kNonRemovable));
  properties.emplace_back(std::make_unique<fdf_devicetree::BoolProperty>(kNoMmcHs400));
  properties.emplace_back(std::make_unique<fdf_devicetree::BoolProperty>(kNoMmcHs200));
  properties.emplace_back(std::make_unique<fdf_devicetree::BoolProperty>(kNoMmcHsDdr));
  properties.emplace_back(std::make_unique<fdf_devicetree::BoolProperty>(kUseFidl));
  sdmmc_parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(properties));
}

bool SdmmcVisitor::is_match(std::string_view name) {
  // Check that the name begins with mmc@.
  return name.find("mmc@") == 0;
}

zx::result<> SdmmcVisitor::Visit(fdf_devicetree::Node& node,
                                 const devicetree::PropertyDecoder& decoder) {
  if (!is_match(node.name())) {
    return zx::ok();
  }

  zx::result parser_output = sdmmc_parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "SDMMC visitor failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  fuchsia_hardware_sdmmc::SdmmcMetadata sdmmc_metadata = {};
  sdmmc_metadata.instance_identifier() = node.name();

  if (parser_output->find(kMaxFrequency) != parser_output->end()) {
    sdmmc_metadata.max_frequency() = parser_output->at(kMaxFrequency)[0].AsUint32();
  }

  if (parser_output->find(kNonRemovable) != parser_output->end()) {
    sdmmc_metadata.removable() = false;
  } else {
    sdmmc_metadata.removable() = true;
  }

  uint64_t host_prefs = 0;
  if (parser_output->find(kNoMmcHs400) != parser_output->end()) {
    host_prefs |= static_cast<uint64_t>(fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHs400);
  }
  if (parser_output->find(kNoMmcHs200) != parser_output->end()) {
    host_prefs |= static_cast<uint64_t>(fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHs200);
  }
  if (parser_output->find(kNoMmcHsDdr) != parser_output->end()) {
    host_prefs |= static_cast<uint64_t>(fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHsddr);
  }

  if (host_prefs) {
    sdmmc_metadata.speed_capabilities() =
        std::optional<fuchsia_hardware_sdmmc::SdmmcHostPrefs>(host_prefs);
  }

  if (parser_output->find(kUseFidl) != parser_output->end()) {
    sdmmc_metadata.use_fidl() = true;
  } else {
    sdmmc_metadata.use_fidl() = false;
  }

  fit::result encoded_metadata = fidl::Persist(sdmmc_metadata);
  if (!encoded_metadata.is_ok()) {
    FDF_LOG(ERROR, "Failed to encode SDMMC metadata for node %s: %s", node.name().c_str(),
            encoded_metadata.error_value().FormatDescription().c_str());
    return zx::error(encoded_metadata.error_value().status());
  }

  fuchsia_hardware_platform_bus::Metadata metadata = {
      {.id = std::to_string(DEVICE_METADATA_SDMMC), .data = encoded_metadata.value()}};
  node.AddMetadata(std::move(metadata));
  FDF_LOG(DEBUG, "SDMMC metadata added to node '%s'", node.name().c_str());

  return zx::ok();
}

}  // namespace sdmmc_dt

REGISTER_DEVICETREE_VISITOR(sdmmc_dt::SdmmcVisitor);
