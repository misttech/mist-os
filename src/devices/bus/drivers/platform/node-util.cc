// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bus/drivers/platform/node-util.h"

namespace platform_bus {

// Most drivers only have a few of each resource, so we have just kept these as
// linear searches for simplicity. We can consider pre-constructing a map for
// each resource and storing it with the platform device if performance is an issue.

// Returns the index of the mmio that matches |mmio_name|.
std::optional<uint32_t> GetMmioIndex(const fuchsia_hardware_platform_bus::Node& node,
                                     std::string_view mmio_name) {
  if (node.mmio() == std::nullopt) {
    return std::nullopt;
  }
  for (size_t i = 0; i < node.mmio()->size(); i++) {
    if (node.mmio().value()[i].name().value().compare(mmio_name) == 0) {
      return i;
    }
  }
  return std::nullopt;
}

// Returns the index of the irq that matches |irq_name|.
std::optional<uint32_t> GetIrqIndex(const fuchsia_hardware_platform_bus::Node& node,
                                    std::string_view irq_name) {
  if (node.mmio() == std::nullopt) {
    return std::nullopt;
  }
  for (size_t i = 0; i < node.irq()->size(); i++) {
    if (node.irq().value()[i].name().value().compare(irq_name) == 0) {
      return i;
    }
  }
  return std::nullopt;
}

// Returns the index of the bti that matches |bti_name|.
std::optional<uint32_t> GetBtiIndex(const fuchsia_hardware_platform_bus::Node& node,
                                    std::string_view bti_name) {
  if (node.bti() == std::nullopt) {
    return std::nullopt;
  }
  for (size_t i = 0; i < node.bti()->size(); i++) {
    if (node.bti().value()[i].name().value().compare(bti_name) == 0) {
      return i;
    }
  }
  return std::nullopt;
}

// Returns the index of the smc that matches |smc_name|.
std::optional<uint32_t> GetSmcIndex(const fuchsia_hardware_platform_bus::Node& node,
                                    std::string_view smc_name) {
  if (node.smc() == std::nullopt) {
    return std::nullopt;
  }
  for (size_t i = 0; i < node.smc()->size(); i++) {
    if (node.smc().value()[i].name().value().compare(smc_name) == 0) {
      return i;
    }
  }
  return std::nullopt;
}

}  // namespace platform_bus
