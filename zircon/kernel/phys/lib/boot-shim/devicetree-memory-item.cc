// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/boot-shim/devicetree.h"

namespace boot_shim {

devicetree::ScanState DevicetreeMemoryMatcher::OnNode(const devicetree::NodePath& path,
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

devicetree::ScanState DevicetreeMemoryMatcher::OnSubtree(const devicetree::NodePath& path) {
  if (&path.back() == reserved_memory_root_) {
    reserved_memory_root_ = nullptr;
  }

  return devicetree::ScanState::kActive;
}

// |path.back()| must be 'memory'
// Each node may define N ranges in their reg property.
// Each node may have children defining subregions with special purpose (RESERVED ranges.)/
devicetree::ScanState DevicetreeMemoryMatcher::HandleMemoryNode(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  ZX_DEBUG_ASSERT(path.back().name() == "memory");

  // see for ranges in parent.
  if (!root_ranges_) {
    root_ranges_ =
        decoder.parent()
            ? decoder.parent()->FindAndDecodeProperty<&devicetree::PropertyValue::AsRanges>(
                  "ranges", *decoder.parent())
            : std::nullopt;
  }

  if (!AppendRangesFromReg(decoder, root_ranges_, memalloc::Type::kFreeRam)) {
    return devicetree::ScanState::kDone;
  }

  return devicetree::ScanState::kActive;
}

// |path.back()| must be 'reserved-memory'
// Each child node is a reserved region.
devicetree::ScanState DevicetreeMemoryMatcher::HandleReservedMemoryNode(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  ZX_DEBUG_ASSERT(path.back() == "reserved-memory");
  ZX_DEBUG_ASSERT(reserved_memory_root_ == nullptr);

  reserved_memory_root_ = &path.back();

  // see for ranges in parent.
  if (!root_ranges_) {
    root_ranges_ =
        decoder.parent()
            ? decoder.parent()->FindAndDecodeProperty<&devicetree::PropertyValue::AsRanges>(
                  "ranges", *decoder.parent())
            : std::nullopt;
  }

  if (!reserved_memory_ranges_) {
    reserved_memory_ranges_ =
        decoder.FindAndDecodeProperty<&devicetree::PropertyValue::AsRanges>("ranges", decoder);
  }

  if (!AppendRangesFromReg(decoder, root_ranges_, memalloc::Type::kReserved)) {
    return devicetree::ScanState::kDone;
  }

  return devicetree::ScanState::kActive;
}

bool DevicetreeMemoryMatcher::AppendRangesFromReg(
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

}  // namespace boot_shim
