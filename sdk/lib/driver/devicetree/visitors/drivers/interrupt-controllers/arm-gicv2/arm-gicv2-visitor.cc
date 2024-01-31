// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "arm-gicv2-visitor.h"

#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include <cstddef>
#include <cstdint>
#include <string_view>
#include <vector>

#include "arm-gicv2.h"

namespace arm_gic_dt {

static constexpr auto kGicCompatibleDevices = cpp20::to_array<std::string_view>({
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
  zx::result<uint32_t> mode() {
    uint64_t mode = *interrupt_cells_[0][2];
    switch (mode & kModeMask) {
      case GIC_IRQ_MODE_EDGE_RISING:
        return zx::ok(ZX_INTERRUPT_MODE_EDGE_HIGH);
      case GIC_IRQ_MODE_EDGE_FALLING:
        if (is_spi()) {
          FDF_LOG(ERROR, "Edge low mode not supported for SPI interrupt");
          return zx::error(ZX_ERR_INVALID_ARGS);
        }
        return zx::ok(ZX_INTERRUPT_MODE_EDGE_LOW);
      case GIC_IRQ_MODE_LEVEL_HIGH:
        return zx::ok(ZX_INTERRUPT_MODE_LEVEL_HIGH);
      case GIC_IRQ_MODE_LEVEL_LOW:
        if (is_spi()) {
          FDF_LOG(ERROR, "Level low mode not supported for SPI interrupt");
          return zx::error(ZX_ERR_INVALID_ARGS);
        }
        return zx::ok(ZX_INTERRUPT_MODE_LEVEL_LOW);
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

ArmGicV2Visitor::ArmGicV2Visitor()
    : fdf_devicetree::DriverVisitor(
          std::vector<std::string>(kGicCompatibleDevices.begin(), kGicCompatibleDevices.end())) {}

zx::result<> ArmGicV2Visitor::Visit(fdf_devicetree::Node& node,
                                    const devicetree::PropertyDecoder& decoder) {
  auto parser_output = interrupt_parser_.Parse(node);
  if (parser_output.is_error()) {
    return parser_output.take_error();
  }

  // Interrupt parser converts all interrupts into kInterruptsExtended. No need to look for
  // kInterrupts property.
  if (parser_output->find(fdf_devicetree::InterruptParser::kInterruptsExtended) !=
      parser_output->end()) {
    return ParseInterrupts(node,
                           (*parser_output)[fdf_devicetree::InterruptParser::kInterruptsExtended]);
  }

  return zx::ok();
}

zx::result<> ArmGicV2Visitor::ParseInterrupts(
    fdf_devicetree::Node& node, std::vector<fdf_devicetree::PropertyValue>& interrupts) {
  for (uint32_t index = 0; index < interrupts.size(); index++) {
    auto reference = interrupts[index].AsReference();
    if (reference && is_match(reference->first.properties())) {
      auto result = ParseInterrupt(node, reference->first, reference->second);
      if (result.is_error()) {
        return result.take_error();
      }
    }
  }
  return zx::ok();
}

zx::result<> ArmGicV2Visitor::ParseInterrupt(fdf_devicetree::Node& child,
                                             fdf_devicetree::ReferenceNode& parent,
                                             fdf_devicetree::PropertyCells interrupt_cells) {
  if (interrupt_cells.size() != (3 * sizeof(uint32_t))) {
    FDF_LOG(
        ERROR,
        "Incorrect number of cells (expected %zu, found %zu) for arm,gic v2 interrupt in node '%s",
        3 * sizeof(uint32_t), interrupt_cells.size(), child.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto interrupt = InterruptPropertyV2(interrupt_cells);

  zx::result mode = interrupt.mode();
  if (mode.is_error()) {
    FDF_LOG(ERROR, "Failed to parse mode for interrupt %d of node '%s - %s", interrupt.irq(),
            child.name().c_str(), mode.status_string());
    return mode.take_error();
  }

  fuchsia_hardware_platform_bus::Irq irq = {{
      .irq = interrupt.irq(),
      .mode = *mode,
  }};
  FDF_LOG(DEBUG, "IRQ 0x%0x with mode 0x%0x added to node '%s'.", *irq.irq(), *irq.mode(),
          child.name().c_str());
  child.AddIrq(irq);
  return zx::ok();
}

}  // namespace arm_gic_dt

REGISTER_DEVICETREE_VISITOR(arm_gic_dt::ArmGicV2Visitor);
