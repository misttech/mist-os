// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_UART_H_
#define ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_UART_H_

#include <lib/uart/all.h>

#include <type_traits>

#include "item-base.h"

namespace boot_shim {

// This can supply a ZBI_TYPE_KERNEL_DRIVER item based on the UART driver configuration.
template <typename AllDrivers = uart::all::Driver>
class UartItem : public boot_shim::ItemBase {
 public:
  void Init(const uart::all::Driver& uart) { driver_ = uart; }

  constexpr size_t size_bytes() const { return ItemSize(zbi_dcfg_size()); }

  fit::result<DataZbi::Error> AppendItems(DataZbi& zbi) const {
    fit::result<UartItem::DataZbi::Error> result = fit::ok();
    uart::internal::Visit(
        [&zbi, &result](const auto& driver) {
          if constexpr (std::is_same_v<uart::null::Driver, std::decay_t<decltype(driver)>>) {
            result = fit::ok();
          } else {
            if (auto append_result = zbi.Append({
                    .type = driver.type(),
                    .length = static_cast<uint32_t>(driver.size()),
                    .extra = driver.extra(),
                });
                append_result.is_ok()) {
              auto& [header, payload] = *append_result.value();
              driver.FillItem(payload.data());
              result = fit::ok();
            } else {
              result = append_result.take_error();
            }
          }
        },
        driver_);
    return result;
  }

 private:
  constexpr size_t zbi_dcfg_size() const {
    size_t size = 0;
    uart::internal::Visit([&](const auto& driver) { size = driver.size(); }, driver_);
    return size;
  }

  AllDrivers driver_;
};

}  // namespace boot_shim

#endif  // ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_UART_H_
