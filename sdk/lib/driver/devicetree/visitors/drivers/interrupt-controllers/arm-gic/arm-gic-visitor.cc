// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "arm-gic-visitor.h"

#include <lib/driver/devicetree/visitors/interrupt-parser.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/stdcompat/array.h>

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "arm-gicv2.h"

namespace fpbus = fuchsia_hardware_platform_bus;
namespace {

constexpr auto kGicV1V2CompatibleDevices = cpp20::to_array<std::string_view>({
    // V1 and V2 compatible list
    "arm,arm11mp-gic",
    "arm,cortex-a15-gic",
    "arm,cortex-a7-gic",
    "arm,cortex-a5-gic",
    "arm,cortex-a9-gic",
    "arm,eb11mp-gic",
    "arm,gic-400",
    "arm,pl390",
    "arm,tc11mp-gic",
    "qcom,msm-8660-qgic",
    "qcom,msm-qgic2",
});

constexpr auto kGicV3CompatibleDevices = cpp20::to_array<std::string_view>({
    // V3 compatible list
    "arm,gic-v3",
});

std::vector<std::string> GetCompatibleList() {
  auto compatible_list =
      std::vector<std::string>(kGicV1V2CompatibleDevices.begin(), kGicV1V2CompatibleDevices.end());
  compatible_list.insert(compatible_list.end(), kGicV3CompatibleDevices.begin(),
                         kGicV3CompatibleDevices.end());
  return compatible_list;
}

bool IsArmGicV1V2(devicetree::StringList<> compatible_strings) {
  auto matched =
      std::find_first_of(compatible_strings.begin(), compatible_strings.end(),
                         kGicV1V2CompatibleDevices.begin(), kGicV1V2CompatibleDevices.end());
  return matched != compatible_strings.end();
}

bool IsArmGicV3(devicetree::StringList<> compatible_strings) {
  auto matched = std::find_first_of(compatible_strings.begin(), compatible_strings.end(),
                                    kGicV3CompatibleDevices.begin(), kGicV3CompatibleDevices.end());
  return matched != compatible_strings.end();
}

}  // namespace

namespace arm_gic_dt {

class InterruptPropertyV2 {
 public:
  static constexpr uint32_t kModeMask = 0x000F;

  explicit InterruptPropertyV2(fdf_devicetree::PropertyCells cells)
      : interrupt_cells_(cells, 1, 1, 1) {}

  // 1st cell contains the interrupt type; 0 for SPI interrupts, 1 for PPI interrupts.
  bool is_spi() { return *interrupt_cells_[0][0] == GIC_SPI; }

  // 2nd cell contains the interrupt number.
  // SPI interrupts are in the range [0-987].
  // PPI interrupts are in the range [0-15].
  uint32_t irq() {
    uint32_t irq = static_cast<uint32_t>(*interrupt_cells_[0][1]);
    if (is_spi()) {
      // SPI interrupts start at 32.
      // See https://developer.arm.com/documentation/101206/0003/Operation/Interrupt-types/SPIs.
      irq += 32;
    } else {
      // PPI interrupts start at 16.
      // See https://developer.arm.com/documentation/101206/0003/Operation/Interrupt-types/PPIs.
      irq += 16;
    }
    return irq;
  }

  // 3rd cell contains the flags.
  //     bits[3:0] contains trigger type and level.
  //        1 = low-to-high edge triggered
  //        2 = high-to-low edge triggered (invalid for SPI)
  //        4 = active high level-sensitive
  //        8 = active low level-sensitive (invalid for SPI).
  zx::result<fuchsia_hardware_platform_bus::ZirconInterruptMode> mode() {
    uint64_t mode = *interrupt_cells_[0][2];
    switch (mode & kModeMask) {
      case GIC_IRQ_MODE_EDGE_RISING:
        return zx::ok(fuchsia_hardware_platform_bus::ZirconInterruptMode::kEdgeHigh);
      case GIC_IRQ_MODE_EDGE_FALLING:
        if (is_spi()) {
          FDF_LOG(ERROR, "Edge low mode not supported for SPI interrupt");
          return zx::error(ZX_ERR_INVALID_ARGS);
        }
        return zx::ok(fuchsia_hardware_platform_bus::ZirconInterruptMode::kEdgeLow);
      case GIC_IRQ_MODE_LEVEL_HIGH:
        return zx::ok(fuchsia_hardware_platform_bus::ZirconInterruptMode::kLevelHigh);
      case GIC_IRQ_MODE_LEVEL_LOW:
        if (is_spi()) {
          FDF_LOG(ERROR, "Level low mode not supported for SPI interrupt");
          return zx::error(ZX_ERR_INVALID_ARGS);
        }
        return zx::ok(fuchsia_hardware_platform_bus::ZirconInterruptMode::kLevelLow);
      default:
        break;
    }

    FDF_LOG(ERROR, "Invalid mode %lu", mode & kModeMask);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

 private:
  using InterruptElement = devicetree::PropEncodedArrayElement<3>;
  devicetree::PropEncodedArray<InterruptElement> interrupt_cells_;
};

ArmGicVisitor::ArmGicVisitor() : fdf_devicetree::DriverVisitor(GetCompatibleList()) {}

zx::result<> ArmGicVisitor::Visit(fdf_devicetree::Node& node,
                                  const devicetree::PropertyDecoder& decoder) {
  zx::result<fdf_devicetree::PropertyValues> properties = interrupt_parser_.Parse(node);
  if (properties.is_error()) {
    return properties.take_error();
  }

  // Interrupt parser converts all interrupts into kInterruptsExtended. No need to look for
  // kInterrupts property.
  auto interrupts_it = properties->find(fdf_devicetree::InterruptParser::kInterruptsExtended);
  if (interrupts_it == properties->end()) {
    return zx::ok();
  }

  std::vector<fdf_devicetree::PropertyValue> interrupts(std::move(interrupts_it->second));
  std::vector<fdf_devicetree::PropertyValue> interrupt_names{};
  std::vector<fdf_devicetree::PropertyValue> wake_vectors{};
  if (const auto names_it = properties->find(fdf_devicetree::InterruptParser::kInterruptNames);
      names_it != properties->end()) {
    interrupt_names.swap(names_it->second);
    // If `interrupt-names` property is present in the dts then we require that
    // it be the same size as the number of interrupts specified in the
    // `interrupts` property.
    ZX_DEBUG_ASSERT(interrupts.size() == interrupt_names.size());

    // Wake vectors are only valid if accompanying an `interrupt-names` property.
    if (const auto wake_it =
            properties->find(fdf_devicetree::InterruptParser::kFuchsiaInterruptWakeVectors);
        wake_it != properties->end()) {
      wake_vectors.swap(wake_it->second);
    }
  }

  // Verify and then add any GIC interrupts we've parsed.
  for (uint32_t i = 0; i < interrupts.size(); i++) {
    std::optional references = interrupts[i].AsReference();
    if (!references) {
      continue;
    }

    auto& [parent, cells] = references.value();
    if (!is_match(parent.properties())) {
      continue;
    }

    std::optional<std::string> name = std::nullopt;
    if (!interrupt_names.empty()) {
      name = interrupt_names[i].AsString();
    }

    zx::result result = ParseInterrupt(node.name(), parent, cells, name);
    if (result.is_error()) {
      return result.take_error();
    }

    fpbus::Irq irq = std::move(result.value());
    // If the node has a property for specifying a fuchsia interrupt wake vector then set an
    // interrupts wake capability based on whether or not we find their name referenced.
    if (name.has_value() && !wake_vectors.empty()) {
      irq.wake_vector() =
          std::ranges::find_if(wake_vectors, [&name](fdf_devicetree::PropertyValue v) {
            return name == v.AsString().value();
          }) != wake_vectors.end();
    }

    node.AddIrq(irq);
  }

  return zx::ok();
}

zx::result<fpbus::Irq> ArmGicVisitor::ParseInterrupt(const std::string& node_name,
                                                     fdf_devicetree::ReferenceNode& parent,
                                                     fdf_devicetree::PropertyCells interrupt_cells,
                                                     std::optional<std::string> interrupt_name) {
  auto compatible_strings = parent.properties().at("compatible").AsStringList().value();
  if (IsArmGicV1V2(compatible_strings) && (interrupt_cells.size() != (3 * sizeof(uint32_t)))) {
    // For GIC v2 3 cells are expected.
    FDF_LOG(ERROR, "Incorrect number of cells (expected %zu, found %zu) for interrupt in node '%s",
            3 * sizeof(uint32_t), interrupt_cells.size(), node_name.c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  if (IsArmGicV3(compatible_strings) && (interrupt_cells.size() < (3 * sizeof(uint32_t)))) {
    // For GIC v3 at least 3 cells are expected. 4th cell if present represents the phandle of a
    // node that defines the CPU affinity for PPIs. This is not used in Fuchsia currently to
    // configure interrupts. 5th and above cells if present are reserved for future use and should
    // be ignored.
    FDF_LOG(
        ERROR,
        "Incorrect number of cells (expected at least %zu, found %zu) for interrupt in node '%s",
        3 * sizeof(uint32_t), interrupt_cells.size(), node_name.c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // Both GIC V2 and V3 share the same cell specifiers for the first 3 cells.
  auto interrupt = InterruptPropertyV2(interrupt_cells.first(3 * sizeof(uint32_t)));

  zx::result mode = interrupt.mode();
  if (mode.is_error()) {
    FDF_LOG(ERROR, "Failed to parse mode for interrupt %d of node '%s - %s", interrupt.irq(),
            node_name.c_str(), mode.status_string());
    return mode.take_error();
  }

  fpbus::Irq irq = {{
      .irq = interrupt.irq(),
      .mode = *mode,
      .name = std::move(interrupt_name),
      .wake_vector = false,
  }};
  FDF_LOG(DEBUG, "IRQ 0x%0x named '%s' with mode 0x%0x added to node '%s'.", *irq.irq(),
          irq.name().has_value() ? irq.name()->c_str() : "(no name)", *irq.mode(),
          node_name.c_str());
  return zx::ok(irq);
}

}  // namespace arm_gic_dt

REGISTER_DEVICETREE_VISITOR(arm_gic_dt::ArmGicVisitor);
