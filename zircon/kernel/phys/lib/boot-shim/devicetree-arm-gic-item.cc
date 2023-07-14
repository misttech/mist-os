// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-shim/debugdata.h>
#include <lib/devicetree/devicetree.h>
#include <lib/devicetree/matcher.h>
#include <lib/fit/result.h>
#include <lib/memalloc/range.h>
#include <lib/stdcompat/array.h>
#include <lib/uart/all.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/memory.h>
#include <lib/zbi-format/zbi.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <zircon/assert.h>

#include <array>
#include <cstdint>
#include <limits>
#include <optional>
#include <string>
#include <string_view>
#include <type_traits>

#include "lib/boot-shim/devicetree.h"

namespace boot_shim {
namespace {
struct GicV3Regs {
  enum Regs : size_t {
    kGicd = 0,
    kGicr = 1,
    kGicc = 2,
    kGich = 3,
    kGicv = 4,
    kReserved = 5,
  };
};

struct GicV2Regs {
  enum Regs : size_t {
    kGicd = 0,
    kGicc = 1,

    // Virtualization Extension
    kGich = 2,
    kGicv = 3,
    kReserved = 4,
  };
};

// See:
// https://android.googlesource.com/kernel/msm/+/android-msm-hammerhead-3.4-kk-r1/Documentation/devicetree/bindings/memory.txt
// Which is a special case of the memory node. This specific incantation may never happen in the
// real world, if ever needed then this
// constexpr std::string_view kMemoryRegion = "region";

}  // namespace

devicetree::ScanState ArmDevicetreeGicItem::HandleGicChildNode(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  VisitPayload([&](auto& dcfg) -> void {
    using dtype = std::decay_t<decltype(dcfg)>;
    if constexpr (std::is_same_v<dtype, zbi_dcfg_arm_gic_v2_driver_t>) {
      // If subnode is defined, then msi is enabled.
      if (path.back().name() == "v2m") {
        auto [reg_property] = decoder.FindProperties("reg");
        if (!reg_property) {
          return;
        }

        auto reg = reg_property->AsReg(decoder);
        if (!reg) {
          return;
        }

        uint64_t base_address = *(*reg)[0].address();
        if (auto root_address = decoder.TranslateAddress(base_address)) {
          dcfg.use_msi = true;
          dcfg.msi_frame_phys = *root_address;
        }
      }
    } else if constexpr (std::is_same_v<dtype, zbi_dcfg_arm_gic_v3_driver_t>) {
      // TODO(fxbug.dev/128235) : no support yet.
    } else {
      // No Driver set, but we have seen the GIC, so this is an error.
      ZX_PANIC("GIC Item should have been initialized before looking into its child.");
    }
  });
  return devicetree::ScanState::kActive;
}

devicetree::ScanState ArmDevicetreeGicItem::HandleGicV3(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  const auto& [reg_property] = decoder.FindProperties("reg");
  if (!reg_property) {
    OnError("GIC v3: No 'reg' found.");
    return devicetree::ScanState::kDoneWithSubtree;
  }

  auto reg_ptr = reg_property->AsReg(decoder);
  if (!reg_ptr) {
    OnError("GIC v3: Failed to decode 'reg'.");
    return devicetree::ScanState::kDoneWithSubtree;
  }

  auto& reg = *reg_ptr;

  auto [redistributor_stride] = decoder.FindProperties("redistributor-stride");

  zbi_dcfg_arm_gic_v3_driver_t dcfg{};

  if (reg.size() > 1) {
    dcfg.mmio_phys = *reg[GicV3Regs::kGicd].address();
    dcfg.gicd_offset = 0;

    dcfg.gicr_offset = *reg[GicV3Regs::kGicr].address() - dcfg.mmio_phys;
    if (redistributor_stride) {
      if (auto stride = redistributor_stride->AsUint32()) {
        dcfg.gicr_stride = *stride;
      }
    } else {
      // See:
      //   "Arm Generic Interrupt Controller Architecture Specification GIC architecture version 3
      //   and version 4" 12.10 The GIC Redistributor register map
      // Specifically:
      //    - Each Redistributor defines two 64KB frames in the physical address map
      //
      // Note for GicV4: VLPI (Virtual Local Peripherial Interrupt), which would require an extra
      // two pages.
      dcfg.gicr_stride = 2 * 64 << 10;
    }
  }

  dcfg.ipi_base = 0;
  dcfg.optional = false;
  set_payload(dcfg);

  return devicetree::ScanState::kDone;
}

devicetree::ScanState ArmDevicetreeGicItem::HandleGicV2(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  const auto& [reg_property] = decoder.FindProperties("reg");

  if (!reg_property) {
    OnError("GIC v2: No 'reg' found.");
    return devicetree::ScanState::kDone;
  }

  auto reg_ptr = reg_property->AsReg(decoder);
  if (!reg_ptr) {
    OnError("GIC v2: Failed to decode 'reg'.");
    return devicetree::ScanState::kDone;
  }

  auto& reg = *reg_ptr;

  zbi_dcfg_arm_gic_v2_driver_t dcfg{};

  if (reg.size() > 1) {
    dcfg.mmio_phys = *reg[GicV2Regs::kGicd].address();
    dcfg.gicd_offset = 0;
    dcfg.gicc_offset = reg[GicV2Regs::kGicc].address().value_or(dcfg.mmio_phys) - dcfg.mmio_phys;
  }

  // If there are more than 2, then the virtualization extension is provided.
  if (reg.size() > 2) {
    dcfg.gich_offset = reg[GicV2Regs::kGich].address().value_or(dcfg.mmio_phys) - dcfg.mmio_phys;
    dcfg.gicv_offset = reg[GicV2Regs::kGicv].address().value_or(dcfg.mmio_phys) - dcfg.mmio_phys;
  }

  dcfg.ipi_base = 0;
  dcfg.optional = false;
  // Default values when msi is not enabled. This is determined by proper handlers.
  // The MSI Frame registers are contigous, so we will pick the lowest base address from all the
  // frames.
  dcfg.use_msi = false;
  dcfg.msi_frame_phys = std::numeric_limits<decltype(dcfg.msi_frame_phys)>::max();
  set_payload(dcfg);
  gic_ = &path.back();

  return devicetree::ScanState::kActive;
}

devicetree::ScanState ArmDevicetreeGicItem::OnNode(const devicetree::NodePath& path,
                                                   const devicetree::PropertyDecoder& decoder) {
  if (IsGicChildNode()) {
    return HandleGicChildNode(path, decoder);
  }

  // Figure out which interrupt controller this is.
  const auto& [compatible, interrupt_controller, msi_controller] =
      decoder.FindProperties("compatible", "interrupt-controller", "msi-controller");

  if (!interrupt_controller || !compatible) {
    return devicetree::ScanState::kActive;
  }

  auto compatible_list = compatible->AsStringList();
  if (!compatible_list) {
    return devicetree::ScanState::kActive;
  }

  // Check for gic version.
  for (auto comp : *compatible_list) {
    for (auto v3 : kGicV3CompatibleDevices) {
      if (v3 == comp) {
        return HandleGicV3(path, decoder);
      }
    }

    for (auto v2 : kGicV2CompatibleDevices) {
      if (v2 == comp) {
        return HandleGicV2(path, decoder);
      }
    }
  }

  // Keep looking.
  return devicetree::ScanState::kActive;
}

devicetree::ScanState ArmDevicetreeGicItem::OnSubtree(const devicetree::NodePath& path) {
  if (gic_ == nullptr) {
    return matched_ ? devicetree::ScanState::kDone : devicetree::ScanState::kActive;
  }

  if (gic_ != &path.back()) {
    return devicetree::ScanState::kActive;
  }

  gic_ = nullptr;
  matched_ = true;
  return devicetree::ScanState::kDone;
}

}  // namespace boot_shim
