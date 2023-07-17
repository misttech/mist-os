// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <algorithm>

#include "lib/boot-shim/devicetree.h"

namespace boot_shim {
namespace {

// Both Gic V2 and V3 have the same structure for interrupt cells.
std::optional<uint32_t> GetGicIrq(devicetree::PropertyValue interrupt_bytes,
                                  uint32_t interrupt_cells) {
  // GIC v2 requires 3 cells.
  // GIC v3 requires at least 3 cells, but cells 4 and beyond are reserved for future use.
  // Also GIC v3 states that cells 4 and beyond act as padding, and may be ignored. It is
  // recommended that padding cells have a value of 0.
  if (interrupt_cells < 3) {
    return std::nullopt;
  }

  auto interrupt = interrupt_bytes.AsBytes().substr(0, 3 * sizeof(uint32_t));

  // Now we can instantiate a prop encoded array.
  devicetree::PropEncodedArray<devicetree::PropEncodedArrayElement<3>> cells(interrupt, 1, 1, 1);

  // The 1st cell is the interrupt type; 0 for SPI interrupts, 1 for PPI interrupts.
  bool is_spi = *cells[0][0] == 0;

  //  The 2nd cell contains the interrupt number for the interrupt type.
  //  SPI interrupts are in the range [0-987].  PPI interrupts are in the
  //  range [0-15].
  uint32_t irq_offset = static_cast<uint32_t>(*cells[0][1]);

  // PPI:
  //    A PPI is unique to one core. However, the PPIs to other cores can have the same INTID. Up to
  //    16 PPIs can be recorded for each target core, where each PPI has a different INTID in the
  //    ID16-ID31 range.
  //
  // See https://developer.arm.com/documentation/101206/0003/Operation/Interrupt-types/PPIs
  //
  // SPI:
  //    The permitted values are ID32-ID991, in steps of 32. The first SPI has an ID number of 32.
  //
  // See https://developer.arm.com/documentation/101206/0003/Operation/Interrupt-types/SPIs
  //
  return (is_spi ? 32 : 16) + irq_offset;
}

std::optional<uint32_t> GetPlicIrq(devicetree::PropertyValue interrupt_bytes,
                                   uint32_t interrupt_cells) {
  // PLIC must have at least one cell.
  if (interrupt_cells < 1) {
    return std::nullopt;
  }

  // Only the first cell matters, which is the Interrupt ID or IRQ Number for our purposes.
  //
  // Global interrupt sources are assigned small unsigned integer identifiers, beginning at the
  // value 1. An interrupt ID of 0 is reserved to mean “no interrupt”.
  //
  // See Chapter 7 Section 5 of  Volume II: RISC-V Privileged Architectures V1.10
  auto interrupt = interrupt_bytes.AsBytes().substr(0, sizeof(uint32_t));
  devicetree::PropEncodedArray<devicetree::PropEncodedArrayElement<1>> cells(interrupt, 1);
  return static_cast<uint32_t>(*cells[0][0]);
}

std::optional<uint32_t> ResolveIrq(devicetree::StringList<> compatibles,
                                   devicetree::PropertyValue interrupt_bytes,
                                   uint32_t interrupt_cells) {
  auto is_compatible = [&compatibles](const auto& bindings) {
    return std::find_first_of(compatibles.begin(), compatibles.end(), bindings.begin(),
                              bindings.end()) != compatibles.end();
  };

  if (is_compatible(ArmDevicetreeGicItem::kGicV2CompatibleDevices) ||
      is_compatible(ArmDevicetreeGicItem::kGicV3CompatibleDevices)) {
    return GetGicIrq(interrupt_bytes, interrupt_cells);
  }

  if (is_compatible(RiscvDevicetreePlicItem::kCompatibleDevices)) {
    return GetPlicIrq(interrupt_bytes, interrupt_cells);
  }
  return 0;
}

}  // namespace

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

  auto [compatible, interrupts, reg_property] =
      decoder.FindProperties("compatible", "interrupts", "reg");

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
    if (!uart_matcher_(decoder)) {
      return devicetree::ScanState::kDone;
    }

    uart_dcfg_.mmio_phys = *addr;
    uart_dcfg_.irq = 0;

    if (!interrupts) {
      OnError("Uart Device does not provide interrupt cells.");
      return devicetree::ScanState::kDone;
    }

    uart_interrupts_ = *interrupts;
    // Look in the path to the root for interrupt parent or interrupt-controller.
    auto* current = &decoder;
    while (current) {
      auto [compatibles, interrupt_parent, interrupt_controller] =
          current->FindProperties("compatible", "interrupt-parent", "interrupt-controller");
      if (!compatibles || !compatibles->AsStringList()) {
        OnError("Missing compatible property in interrupt controller.");
        return devicetree::ScanState::kDone;
      }

      if (interrupt_controller) {
        // We found the interrupt controller, we need to resolve the interrupt cells based on the
        // compatible string.
        if (!current->num_interrupt_cells()) {
          OnError("UART's interrupt controller did not provide #interrupt-cells property.");
          return devicetree::ScanState::kDone;
        }

        auto irq_num = ResolveIrq(*compatibles->AsStringList(), *uart_interrupts_,
                                  *current->num_interrupt_cells());
        if (!irq_num) {
          OnError("UART's interrupt property does not meet interrupt-controller's specification.");
          return devicetree::ScanState::kDone;
        }
        uart_dcfg_.irq = *irq_num;
        uart_emplacer_(uart_dcfg_);
        return devicetree::ScanState::kDone;
      }

      // Uart cannot be resolved yet.
      if (interrupt_parent) {
        uart_interrupt_parent_ = interrupt_parent->AsUint32();
        return devicetree::ScanState::kActive;
      }

      current = current->parent();
    }

    return devicetree::ScanState::kActive;
  }

  return devicetree::ScanState::kDone;
}

devicetree::ScanState DevicetreeBootstrapChosenNodeItemBase::OnNode(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  if (found_chosen_) {
    if (uart_interrupt_parent_) {
      return HandleUartInterruptParent(decoder);
    }

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

  // Make it just contain the prefix. This string can be formatted as 'path:UART_ARGS'.
  // Where UART_ARGS can be things such as baud rate, parity, etc. We only need |path| from the
  // format.
  if (!stdout_path_.empty()) {
    stdout_path_ = stdout_path_.substr(0, stdout_path_.find(':'));
  }

  if (ramdisk_start && ramdisk_end) {
    // RISC V and ARM disagree on what the type of these fields are. In both cases its an integer,
    // but sometimes is u32 and sometimes is u64. So based on the number of bytes, guess.
    std::optional<uint64_t> address_start =
        ramdisk_start->AsBytes().size() > sizeof(uint32_t)
            ? ramdisk_start->AsUint64()
            : static_cast<std::optional<uint64_t>>(ramdisk_start->AsUint32());

    std::optional<uint64_t> address_end =
        ramdisk_end->AsBytes().size() > sizeof(uint32_t)
            ? ramdisk_end->AsUint64()
            : static_cast<std::optional<uint64_t>>(ramdisk_end->AsUint32());
    if (!address_start) {
      OnError("Failed to parse chosen node's \"linux,initrd-start\" property.");
      return devicetree::ScanState::kActive;
    }

    if (!address_end) {
      OnError("Failed to parse chosen node's \"linux,initrd-end\" property.");
      return devicetree::ScanState::kActive;
    }

    zbi_ = cpp20::span<const std::byte>(reinterpret_cast<const std::byte*>(*address_start),
                                        static_cast<size_t>(*address_end - *address_start));
  }

  return devicetree::ScanState::kActive;
}

devicetree::ScanState DevicetreeBootstrapChosenNodeItemBase::HandleUartInterruptParent(
    const devicetree::PropertyDecoder& decoder) {
  // We are doing lookup and we need to check if this |decoder| points to the uart interrupt
  // phandle.
  if (uart_interrupt_parent_) {
    auto [compatible, phandle] = decoder.FindProperties("compatible", "phandle");
    if (!phandle || phandle->AsUint32() != uart_interrupt_parent_) {
      return devicetree::ScanState::kActive;
    }

    if (!decoder.num_interrupt_cells()) {
      OnError("UART's interrupt controller did not provide #interrupt-cells property.");
      return devicetree::ScanState::kDone;
    }

    auto irq_num =
        ResolveIrq(*compatible->AsStringList(), *uart_interrupts_, *decoder.num_interrupt_cells());
    if (!irq_num) {
      OnError("UART's interrupt property does not meet interrupt-controller's specification.");
      return devicetree::ScanState::kDone;
    }
    uart_dcfg_.irq = *irq_num;

    // This the interrupt controller and we look at compatibles and translate IRQ.
    uart_emplacer_(uart_dcfg_);
    return devicetree::ScanState::kDone;
  }

  return devicetree::ScanState::kActive;
}

}  // namespace boot_shim
