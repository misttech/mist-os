// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/zircon-internal/align.h>

#include "lib/boot-shim/devicetree.h"

namespace boot_shim {

devicetree::ScanState DevicetreeMemoryMatcher::OnNode(const devicetree::NodePath& path,
                                                      const devicetree::PropertyDecoder& decoder) {
  if (path.IsDescendentOf("/reserved-memory")) {
    if (!AppendRangesFromReg(decoder, memalloc::Type::kReserved)) {
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

// |path.back()| must be 'memory'
// Each node may define N ranges in their reg property.
// Each node may have children defining subregions with special purpose (RESERVED ranges.)/
devicetree::ScanState DevicetreeMemoryMatcher::HandleMemoryNode(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  ZX_DEBUG_ASSERT(path.back().name() == "memory");

  if (!AppendRangesFromReg(decoder, memalloc::Type::kFreeRam)) {
    return devicetree::ScanState::kDone;
  }

  return devicetree::ScanState::kActive;
}

// |path.back()| must be 'reserved-memory'
// Each child node is a reserved region.
devicetree::ScanState DevicetreeMemoryMatcher::HandleReservedMemoryNode(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  ZX_DEBUG_ASSERT(path.back() == "reserved-memory");

  if (!AppendRangesFromReg(decoder, memalloc::Type::kReserved)) {
    return devicetree::ScanState::kDone;
  }

  return devicetree::ScanState::kActive;
}

bool DevicetreeMemoryMatcher::AppendRangesFromReg(const devicetree::PropertyDecoder& decoder,
                                                  memalloc::Type memrange_type) {
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

  for (size_t i = 0; i < reg.size(); ++i) {
    auto addr = reg[i].address();
    auto size = reg[i].size();
    if (!addr || !size) {
      continue;
    }

    auto translated_address = decoder.TranslateAddress(*addr);
    if (!translated_address) {
      continue;
    }

    size = ZX_PAGE_ALIGN(*translated_address + *size) - *translated_address;

    // Append each range as available memory.
    if (!AppendRange(memalloc::Range{
            .addr = *translated_address,
            .size = *size,
            .type = memrange_type,
        })) {
      return false;
    }
  }

  return true;
}

}  // namespace boot_shim
