// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-shim/devicetree.h>
#include <lib/devicetree/devicetree.h>
#include <lib/devicetree/matcher.h>

#include <algorithm>

namespace boot_shim {
namespace {

constexpr std::string_view kRamOops = "ramoops";

}

devicetree::ScanState RamoopsMatcher::OnNode(const devicetree::NodePath& path,
                                             const devicetree::PropertyDecoder& decoder) {
  if (path.IsAncestorOf("/reserved-memory/ramoops")) {
    return devicetree::ScanState::kActive;
  }

  auto is_compatible = [](const devicetree::PropertyDecoder& decoder) {
    auto compatible_list =
        decoder.FindAndDecodeProperty<&devicetree::PropertyValue::AsStringList>("compatible");
    if (!compatible_list) {
      return false;
    }

    return std::find(compatible_list->begin(), compatible_list->end(), kRamOops) !=
           compatible_list->end();
  };

  if (path.back().name() == kRamOops || is_compatible(decoder)) {
    auto reg = decoder.FindAndDecodeProperty<&devicetree::PropertyValue::AsReg>("reg", decoder);
    if (reg && reg->size() >= 1) {
      auto address = (*reg)[0].address();
      auto size = (*reg)[0].size();
      if (address && size) {
        range_ = {
            .base = *address,
            .length = *size,
        };
      }
    }
    return devicetree::ScanState::kDone;
  }

  return devicetree::ScanState::kDoneWithSubtree;
}

}  // namespace boot_shim
