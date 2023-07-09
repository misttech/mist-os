// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/boot-shim/devicetree.h"

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

std::optional<devicetree::RangesProperty> GetRanges(const devicetree::PropertyDecoder& decoder) {
  auto ranges = decoder.FindProperty("ranges");
  if (!ranges) {
    return std::nullopt;
  }

  return ranges->AsRanges(decoder);
}

std::optional<devicetree::RangesProperty> GetParentRanges(
    const devicetree::PropertyDecoder& decoder) {
  if (!decoder.parent()) {
    return std::nullopt;
  }

  return GetRanges(*decoder.parent());
}

}  // namespace

devicetree::ScanState ArmDevicetreePsciItem::HandlePsciNode(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  if (auto method = decoder.FindProperty("method")) {
    if (auto method_str = method->AsString()) {
      set_payload(zbi_dcfg_arm_psci_driver_t{
          .use_hvc = *method_str == "hvc",
      });
    }
  } else {
    OnError("\"method\" property missing.");
  }

  return devicetree::ScanState::kDone;
}

devicetree::ScanState ArmDevicetreePsciItem::OnNode(const devicetree::NodePath& path,
                                                    const devicetree::PropertyDecoder& decoder) {
  auto [compatibles] = decoder.FindProperties("compatible");
  if (!compatibles) {
    return devicetree::ScanState::kActive;
  }

  auto compatible_list = compatibles->AsStringList();
  if (!compatible_list) {
    return devicetree::ScanState::kActive;
  }

  for (auto dev : *compatible_list) {
    for (auto psci : kCompatibleDevices) {
      if (dev == psci) {
        return HandlePsciNode(path, decoder);
      }
    }
  }

  return devicetree::ScanState::kDone;
}

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

devicetree::ScanState DevicetreeBootstrapChosenNodeItemBase::HandleBootstrapStdout(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  auto resolved_path = decoder.ResolvePath(stdout_path_);
  if (resolved_path.is_error()) {
    if (resolved_path.error_value() == devicetree::PropertyDecoder::PathResolveError::kNoAliases) {
      return devicetree::ScanState::kNeedsPathResolution;
    }
    return devicetree::ScanState::kDone;
  }

  // for hand off.
  resolved_stdout_ = *resolved_path;

  switch (path.CompareWith(*resolved_path)) {
    case devicetree::NodePath::Comparison::kEqual:
      break;
    case devicetree::NodePath::Comparison::kParent:
    case devicetree::NodePath::Comparison::kIndirectAncestor:
      return devicetree::ScanState::kActive;
    default:
      return devicetree::ScanState::kDoneWithSubtree;
  }

  auto [compatible, interrupt, reg_property, interrupt_parent] =
      decoder.FindProperties("compatible", "interrupts", "reg", "interrupt-parent");

  // Without this we cant figure out what driver to use.
  if (!compatible) {
    return devicetree::ScanState::kDone;
  }

  // No MMIO region, we cant do anything.
  if (!reg_property) {
    return devicetree::ScanState::kDone;
  }

  auto reg = reg_property->AsReg(decoder);
  if (!reg) {
    return devicetree::ScanState::kDone;
  }

  auto addr = (*reg)[0].address();

  if (addr) {
    uart_dcfg_.mmio_phys = *addr;
    uart_dcfg_.irq = 0;

    if (!match_(decoder)) {
      // TODO(fxbug.dev/129729): Move this to after resolving interrupts.
      return devicetree::ScanState::kDone;
    }
    emplacer_(uart_dcfg_);
  }

  return devicetree::ScanState::kDone;
}

devicetree::ScanState DevicetreeBootstrapChosenNodeItemBase::OnNode(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  if (found_chosen_) {
    if (!stdout_path_.empty()) {
      return HandleBootstrapStdout(path, decoder);
    }
    return devicetree::ScanState::kDone;
  }

  switch (path.CompareWith("/chosen")) {
    case devicetree::NodePath::Comparison::kParent:
    case devicetree::NodePath::Comparison::kIndirectAncestor:
      return devicetree::ScanState::kActive;
    case devicetree::NodePath::Comparison::kMismatch:
    case devicetree::NodePath::Comparison::kChild:
    case devicetree::NodePath::Comparison::kIndirectDescendent:
      return devicetree::ScanState::kDoneWithSubtree;
    case devicetree::NodePath::Comparison::kEqual:
      found_chosen_ = true;
      break;
  };

  // We are on /chosen, pull the cmdline, zbi and uart device path.
  auto [bootargs, stdout_path, legacy_stdout_path, ramdisk_start, ramdisk_end] =
      decoder.FindProperties("bootargs", "stdout-path", "linux,stdout-path", "linux,initrd-start",
                             "linux,initrd-end");
  if (bootargs) {
    if (auto cmdline = bootargs->AsString()) {
      cmdline_ = *cmdline;
    }
  }

  if (stdout_path) {
    stdout_path_ = stdout_path->AsString().value_or("");
  } else if (legacy_stdout_path) {
    stdout_path_ = legacy_stdout_path->AsString().value_or("");
  }

  if (ramdisk_start && ramdisk_end) {
    auto addr_start = ramdisk_start->AsUint32();
    auto addr_end = ramdisk_end->AsUint32();
    if (addr_start && addr_end) {
      zbi_ = cpp20::span<const std::byte>(reinterpret_cast<const std::byte*>(*addr_start),
                                          *addr_end - *addr_start);
    }
  }

  return devicetree::ScanState::kActive;
}

devicetree::ScanState DevicetreeMemoryItem::OnNode(const devicetree::NodePath& path,
                                                   const devicetree::PropertyDecoder& decoder) {
  // root reserved-memory child
  if (reserved_memory_root_ != nullptr) {
    if (!AppendRangesFromReg(decoder, reserved_memory_ranges_, memalloc::Type::kReserved)) {
      return devicetree::ScanState::kDone;
    }
    return devicetree::ScanState::kActive;
  }

  if (path == "/") {
    return devicetree::ScanState::kActive;
  }

  // Only look for direct children of the root node.
  if (path.IsChildOf("/")) {
    auto name = path.back().name();
    if (name == "memory") {
      return HandleMemoryNode(path, decoder);
    }

    if (name == "reserved-memory") {
      return HandleReservedMemoryNode(path, decoder);
    }
  }

  // No need to look at other things.
  return devicetree::ScanState::kDoneWithSubtree;
}

devicetree::ScanState DevicetreeMemoryItem::OnSubtree(const devicetree::NodePath& path) {
  if (&path.back() == reserved_memory_root_) {
    reserved_memory_root_ = nullptr;
  }

  return devicetree::ScanState::kActive;
}

fit::result<DevicetreeMemoryItem::DataZbi::Error> DevicetreeMemoryItem::AppendItems(
    DataZbi& zbi) const {
  if (ranges_count_ == 0) {
    return fit::ok();
  }

  auto result = zbi.Append({
      .type = ZBI_TYPE_MEM_CONFIG,
      .length = static_cast<uint32_t>(size_bytes() - sizeof(zbi_header_t)),
  });
  if (result.is_error()) {
    return result.take_error();
  }
  auto [header, payload] = **result;
  for (uint32_t i = 0, j = 0; i < memory_ranges().size(); i++) {
    const auto& mem_range = memory_ranges()[i];

    zbi_mem_range_t zbi_mem_range = {
        .paddr = mem_range.addr,
        .length = mem_range.size,
        .type = static_cast<uint32_t>(
            memalloc::IsExtendedType(mem_range.type) ? memalloc::Type::kFreeRam : mem_range.type),
        .reserved = 0,
    };
    memcpy(payload.data() + (i - j) * sizeof(zbi_mem_range_t), &zbi_mem_range,
           sizeof(zbi_mem_range_t));
  }

  return fit::ok();
}

// |path.back()| must be 'memory'
// Each node may define N ranges in their reg property.
// Each node may have children defining subregions with special purpose (RESERVED ranges.)/
devicetree::ScanState DevicetreeMemoryItem::HandleMemoryNode(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  ZX_DEBUG_ASSERT(path.back().name() == "memory");

  // see for ranges in parent.
  if (!root_ranges_) {
    root_ranges_ = GetParentRanges(decoder);
  }

  if (!AppendRangesFromReg(decoder, root_ranges_, memalloc::Type::kFreeRam)) {
    return devicetree::ScanState::kDone;
  }

  return devicetree::ScanState::kActive;
}

// |path.back()| must be 'reserved-memory'
// Each child node is a reserved region.
devicetree::ScanState DevicetreeMemoryItem::HandleReservedMemoryNode(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  ZX_DEBUG_ASSERT(path.back() == "reserved-memory");
  ZX_DEBUG_ASSERT(reserved_memory_root_ == nullptr);

  reserved_memory_root_ = &path.back();

  // see for ranges in parent.
  if (!root_ranges_) {
    root_ranges_ = GetParentRanges(decoder);
  }

  if (!reserved_memory_ranges_) {
    reserved_memory_ranges_ = GetRanges(decoder);
  }

  if (!AppendRangesFromReg(decoder, root_ranges_, memalloc::Type::kReserved)) {
    return devicetree::ScanState::kDone;
  }

  return devicetree::ScanState::kActive;
}

bool DevicetreeMemoryItem::AppendRangesFromReg(
    const devicetree::PropertyDecoder& decoder,
    const std::optional<devicetree::RangesProperty>& parent_range, memalloc::Type memrange_type) {
  // Look at the reg property for possible memory banks.
  const auto& [reg_property] = decoder.FindProperties("reg");

  if (!reg_property) {
    return true;
  }

  auto reg_ptr = reg_property->AsReg(decoder);
  if (!reg_ptr) {
    OnError("Memory: Failed to decode 'reg'.");
    return true;
  }

  auto& reg = *reg_ptr;

  auto translate_address = [&](uint64_t addr) {
    if (!parent_range) {
      return addr;
    }
    return parent_range->TranslateChildAddress(addr).value_or(addr);
  };

  for (size_t i = 0; i < reg.size(); ++i) {
    auto addr = reg[i].address();
    auto size = reg[i].size();
    if (!addr || !size) {
      continue;
    }

    // Append each range as available memory.
    if (!AppendRange(memalloc::Range{
            .addr = translate_address(*addr),
            .size = *size,
            .type = memrange_type,
        })) {
      return false;
    }
  }

  return true;
}

devicetree::ScanState RiscvDevicetreeTimerItem::OnNode(const devicetree::NodePath& path,
                                                       const devicetree::PropertyDecoder& decoder) {
  if (path == "/") {
    return devicetree::ScanState::kActive;
  }

  if (path == "/cpus") {
    auto freq = decoder.FindProperty("timebase-frequency");
    if (freq) {
      if (auto freq_val = freq->AsUint32()) {
        set_payload(zbi_dcfg_riscv_generic_timer_driver_t{
            .freq_hz = *freq_val,
        });
      }
    }
    return devicetree::ScanState::kDone;
  }

  return devicetree::ScanState::kDoneWithSubtree;
}

devicetree::ScanState RiscvDevicetreePlicItem::OnNode(const devicetree::NodePath& path,
                                                      const devicetree::PropertyDecoder& decoder) {
  auto compatibles = decoder.FindProperty("compatible");

  if (!compatibles) {
    return devicetree::ScanState::kActive;
  }

  auto supported_devices = compatibles->AsStringList();
  if (!supported_devices) {
    return devicetree::ScanState::kActive;
  }

  for (auto compatible_device : kCompatibleDevices) {
    for (auto supported_device : *supported_devices) {
      if (compatible_device == supported_device) {
        return HandlePlicNode(path, decoder);
      }
    }
  }

  return devicetree::ScanState::kActive;
}

devicetree::ScanState RiscvDevicetreePlicItem::HandlePlicNode(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  auto [reg, num_irqs] = decoder.FindProperties("reg", "riscv,ndev");
  if (!reg) {
    OnError("PLIC Node did not contain a 'reg' property.");
    return devicetree::ScanState::kDone;
  }

  bool base_address_ok = false;
  zbi_dcfg_riscv_plic_driver_t dcfg{};
  if (auto reg_val = reg->AsReg(decoder); reg_val) {
    if (auto address = (*reg_val)[0].address()) {
      if (auto root_address = decoder.TranslateAddress(*address)) {
        dcfg.mmio_phys = *root_address;
        base_address_ok = true;
      }
    }
  }

  if (!base_address_ok) {
    OnError("Error parsing PLIC node's reg address.");
    return devicetree::ScanState::kDone;
  }

  if (!num_irqs) {
    OnError("PLIC Node did not contain 'riscv,ndev' property.");
    return devicetree::ScanState::kDone;
  }

  if (auto num_irqs_val = num_irqs->AsUint32()) {
    dcfg.num_irqs = *num_irqs_val;
  } else {
    OnError("Error parsing PLIC node's 'riscv,ndev' property.");
    return devicetree::ScanState::kDone;
  }

  set_payload(dcfg);

  return devicetree::ScanState::kDone;
}

}  // namespace boot_shim
