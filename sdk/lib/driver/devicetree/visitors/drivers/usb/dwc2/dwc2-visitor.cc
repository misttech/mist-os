// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dwc2-visitor.h"

#include <lib/ddk/metadata.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include <cstdint>

#include <usb/dwc2/metadata.h>

namespace dwc2_visitor_dt {

Dwc2Visitor::Dwc2Visitor() : fdf_devicetree::DriverVisitor({"snps,dwc2"}) {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint32Property>(kGRxFifoSize));
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint32Property>(kGNpTxFifoSize));
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint32ArrayProperty>(kGTxFifoSize));
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint32Property>(kGTurnaroundTime));
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint32Property>(kDmaBurstLen));
  parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(properties));
}

zx::result<> Dwc2Visitor::DriverVisit(fdf_devicetree::Node& node,
                                      const devicetree::PropertyDecoder& decoder) {
  zx::result parser_output = parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "dwc2 visitor parse failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  if (parser_output->size() != 0) {
    dwc2_metadata_t dwc2_metadata = {};

    if (parser_output->find(kGRxFifoSize) != parser_output->end()) {
      auto value = parser_output->at(kGRxFifoSize)[0].AsUint32();
      if (!value) {
        FDF_LOG(ERROR, "Node '%s' has invalid '%s'.", node.name().c_str(), kGRxFifoSize);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      dwc2_metadata.rx_fifo_size = *value;
    }

    if (parser_output->find(kGNpTxFifoSize) != parser_output->end()) {
      auto value = parser_output->at(kGNpTxFifoSize)[0].AsUint32();
      if (!value) {
        FDF_LOG(ERROR, "Node '%s' has invalid '%s'.", node.name().c_str(), kGNpTxFifoSize);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      dwc2_metadata.nptx_fifo_size = *value;
    }

    if (parser_output->find(kGTurnaroundTime) != parser_output->end()) {
      auto value = parser_output->at(kGTurnaroundTime)[0].AsUint32();
      if (!value) {
        FDF_LOG(ERROR, "Node '%s' has invalid '%s'.", node.name().c_str(), kGTurnaroundTime);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      dwc2_metadata.usb_turnaround_time = *value;
    }

    if (parser_output->find(kDmaBurstLen) != parser_output->end()) {
      auto value = parser_output->at(kDmaBurstLen)[0].AsUint32();
      if (!value) {
        FDF_LOG(ERROR, "Node '%s' has invalid '%s'.", node.name().c_str(), kDmaBurstLen);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      dwc2_metadata.dma_burst_len = *value;
    }

    if (parser_output->find(kGTxFifoSize) != parser_output->end()) {
      if (parser_output->at(kGTxFifoSize).size() >
          (sizeof(dwc2_metadata.tx_fifo_sizes) / sizeof(uint32_t))) {
        FDF_LOG(ERROR, "Node '%s' has invalid '%s'. Expected size to be <= %lu, actual: %zu.",
                node.name().c_str(), kGTxFifoSize,
                (sizeof(dwc2_metadata.tx_fifo_sizes) / sizeof(uint32_t)),
                parser_output->at(kGTxFifoSize).size());
        return zx::error(ZX_ERR_INVALID_ARGS);
      }

      auto index = 0;
      for (auto tx_fifo : parser_output->at(kGTxFifoSize)) {
        auto value = tx_fifo.AsUint32();
        if (!value) {
          FDF_LOG(ERROR, "Node '%s' has invalid '%s' at index %d.", node.name().c_str(),
                  kGTxFifoSize, index);
          return zx::error(ZX_ERR_INVALID_ARGS);
        }
        dwc2_metadata.tx_fifo_sizes[index++] = *value;
      }
    }

    fuchsia_hardware_platform_bus::Metadata metadata = {{
        .type = DEVICE_METADATA_PRIVATE,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&dwc2_metadata),
            reinterpret_cast<const uint8_t*>(&dwc2_metadata) + sizeof(dwc2_metadata)),
    }};
    node.AddMetadata(std::move(metadata));
    FDF_LOG(DEBUG, "Added dwc2 metadata to node '%s'.", node.name().c_str());
  }

  return zx::ok();
}

}  // namespace dwc2_visitor_dt

REGISTER_DEVICETREE_VISITOR(dwc2_visitor_dt::Dwc2Visitor);
