// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_BOOT_SHIM_H_
#define ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_BOOT_SHIM_H_

#include <lib/devicetree/devicetree.h>
#include <lib/devicetree/matcher.h>
#include <lib/fit/result.h>

#include <type_traits>
#include <utility>

#include <fbl/macros.h>

#include "boot-shim.h"

namespace boot_shim {

// A DevicetreeBootShim represents a collection of items, which look into the devicetree itself
// to gather information to produce ZBI items.
//
// A devicetree item requires inspecting the devicetree. In addition to the API requirements from a
// BootShim's item it must fulfill a devicetree Matcher API. |DevicetreeItemBase| provides the
// expected API for the |DevicetreeBootShim|'s items.
//
// Prefer inheriting from |DevicetreeItemBase| when possible.
template <typename... Items>
class DevicetreeBootShim : public BootShim<Items...> {
 private:
  using Base = BootShim<Items...>;

 public:
  explicit DevicetreeBootShim(const char* name, devicetree::Devicetree dt, FILE* log = stdout)
      : Base(name, log), dt_(dt) {}

  // Initializes all devicetree boot shim items.
  // As part of the initialization each matcher's |Init(shim_name, log)| is called, followed by a
  // single invocation of |devicetree::Match| allowing each provided matcher to collect information
  // from the devicetree.
  bool Init() {
    auto match_with = [this](auto&... items) {
      auto init_item = [this](auto& item) {
        item.Init(*this);
        return 0;
      };
      std::ignore = (init_item(items) + ...);
      return devicetree::Match(dt_, items...);
    };
    return Base::template OnSelectItems<IsDevicetreeItem>(match_with);
  }

 private:
  DECLARE_HAS_MEMBER_FN_WITH_SIGNATURE(HasInit, Init, void (C::*)(const DevicetreeBootShim& shim));

  template <typename T>
  using IsDevicetreeItem = std::conditional_t<HasInit<T>::value && devicetree::kIsMatcher<T>,
                                              std::true_type, std::false_type>;

  devicetree::Devicetree dt_;
};

}  // namespace boot_shim

#endif  // ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_BOOT_SHIM_H_
