// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/boot-shim/devicetree.h"

namespace boot_shim {

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
    if (!match_(decoder)) {
      return devicetree::ScanState::kDone;
    }

    uart_dcfg_.mmio_phys = *addr;
    uart_dcfg_.irq = 0;
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

}  // namespace boot_shim
