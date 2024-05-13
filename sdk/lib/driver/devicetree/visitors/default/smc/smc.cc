// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "lib/driver/devicetree/visitors/default/smc/smc.h"

#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/devicetree/devicetree.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include <cstdint>
#include <optional>

namespace fdf {
using namespace fuchsia_driver_framework;
}

namespace fdf_devicetree {

constexpr const char KSmcsProp[] = "smcs";
constexpr const char KSmcNamesProp[] = "smc-names";
constexpr const uint32_t kSmcCellSize = 3;
constexpr const uint32_t kServiceCallBaseCellSize = 1;
constexpr const uint32_t kServiceCallCountCellSize = 1;
constexpr const uint32_t kSmcFlagsCellSize = 1;

class SmcElement : public devicetree::PropEncodedArrayElement<kSmcCellSize> {
 public:
  using PropEncodedArrayElement<kSmcCellSize>::PropEncodedArrayElement;

  // 1st cell represents the service call number base.
  uint32_t service_call_num_base() {
    std::optional<uint64_t> cell = (*this)[0];
    return static_cast<uint32_t>(*cell);
  }

  // 2nd cell represents the service call count.
  uint32_t service_call_count() {
    std::optional<uint64_t> cell = (*this)[1];
    return static_cast<uint32_t>(*cell);
  }

  // 3rc cell represents the SMC flags.
  uint32_t smc_flags() {
    std::optional<uint64_t> cell = (*this)[2];
    return static_cast<uint32_t>(*cell);
  }
};

SmcVisitor::SmcVisitor() {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(std::make_unique<fdf_devicetree::StringListProperty>(KSmcNamesProp));
  parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(properties));
}

zx::result<> SmcVisitor::Visit(Node& node, const devicetree::PropertyDecoder& decoder) {
  auto smcs_prop = node.properties().find(KSmcsProp);
  if (smcs_prop == node.properties().end()) {
    return zx::ok();
  }

  auto smc_array = devicetree::PropEncodedArray<SmcElement>(
      smcs_prop->second.AsBytes(), kServiceCallBaseCellSize, kServiceCallCountCellSize,
      kSmcFlagsCellSize);

  size_t count = smc_array.size();
  std::vector<std::optional<std::string>> smc_names(count);

  zx::result parser_output = parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "Smc visitor parse failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  if (parser_output->find(KSmcNamesProp) != parser_output->end()) {
    if ((*parser_output)[KSmcNamesProp].size() != count) {
      FDF_LOG(ERROR, "Smc names count mismatch for node '%s'. Expected %zu, got %zu.",
              node.name().c_str(), count, (*parser_output)[KSmcNamesProp].size());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    size_t name_idx = 0;
    for (auto& names : (*parser_output)[KSmcNamesProp]) {
      smc_names[name_idx++] = names.AsString();
    }
  }

  for (uint32_t index = 0; index < smc_array.size(); index++) {
    fuchsia_hardware_platform_bus::Smc metadata;
    metadata.service_call_num_base(smc_array[index].service_call_num_base());
    metadata.count(smc_array[index].service_call_count());

    if (smc_array[index].smc_flags() == 0x1) {
      metadata.exclusive() = true;
    } else {
      metadata.exclusive() = false;
    }

    if (smc_names[index]) {
      metadata.name(smc_names[index]);
    }

    FDF_LOG(DEBUG, "SMC (0x%0x, 0x%0x) added to node '%s'.", *metadata.service_call_num_base(),
            *metadata.count(), node.name().c_str());
    node.AddSmc(metadata);
  }

  return zx::ok();
}

}  // namespace fdf_devicetree
