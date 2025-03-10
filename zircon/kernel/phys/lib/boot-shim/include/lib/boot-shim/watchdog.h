// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/devicetree/devicetree.h>
#include <lib/stdcompat/array.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>

#include <optional>
#include <string_view>

#ifndef ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_WATCHDOG_H_
#define ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_WATCHDOG_H_

namespace boot_shim {

// A `Watchdog` Item must provide the following API:
//
//   * `Watchdog::kCompatibleDevices` as a collection of `std::string_view`.
//     This represents the list of device nodes compatible string the items is matching against.
//
//   * `std::optional<zbi_dcfg_generic_32_watchog_t> Watchdog::MaybeCreate(const
//          devicetree::PropertyDecoder& node_decoder);`
//     This function is provided with a decoder (references a node) whose compatible string contain
//     a device name in the `kCompatibleDevices` collection. If the node properties are enough to
//     generate a valid configuration then a `zbi_dcfg_generic_32_watchdog_t` instance is returned.
//     Otherwise, returns `std::nullopt`.

struct QualcomMsmWatchdog {
  // List of devicetree compatible strings that this `Watchdog` item should be matched to.
  static constexpr auto kCompatibleDevices =
      cpp20::to_array<std::string_view>({"qcom,msm-watchdog"});

  static constexpr uint32_t kResetOffset = 0;
  static constexpr uint32_t kEnableOffset = 0x8;

  // Returns true if decoder references a valid device node describing the watchdog device node and
  // `payload` is properly filled. Otherwise, returns false.
  static std::optional<zbi_dcfg_generic32_watchdog_t> MaybeCreate(
      const devicetree::PropertyDecoder& decoder,
      const DevicetreeBootShimMmioObserver* mmio_observer) {
    auto [reg_names_prop, reg_prop, pet_time_prop] =
        decoder.FindProperties("reg-names", "reg", "qcom,pet-time");

    if (!reg_names_prop || !reg_prop || !pet_time_prop) {
      return std::nullopt;
    }

    auto reg_names = reg_names_prop->AsStringList();
    auto reg = reg_prop->AsReg(decoder);
    auto pet_time = pet_time_prop->AsUint32();

    if (!reg_names || !pet_time || !reg) {
      return std::nullopt;
    }

    size_t base_index = 0;
    for (auto name : *reg_names) {
      if (name == "wdt-base") {
        break;
      }
      base_index++;
    }
    // Index refers to the reg property array.
    if (base_index >= reg->size()) {
      return std::nullopt;
    }

    auto base_reg = (*reg)[base_index];
    auto base_addr = base_reg.address();
    auto base_size = base_reg.size();

    if (!base_addr || !base_size) {
      return std::nullopt;
    }

    (*mmio_observer)(
        DevicetreeMmioRange{.address = *base_addr, .size = static_cast<size_t>(*base_size)});

    return zbi_dcfg_generic32_watchdog_t{
        .pet_action =
            {
                .addr = *base_addr + kResetOffset,
                .clr_mask = 0xffffffff,
                .set_mask = 0x1,
            },
        .enable_action =
            {
                .addr = *base_addr + kEnableOffset,
                .clr_mask = 0xffffffff,
                .set_mask = 0x1,
            },
        .disable_action =
            {
                .addr = *base_addr + kEnableOffset,
                .clr_mask = 0xffffffff,
                .set_mask = 0x0,
            },
        .watchdog_period_nsec = zx::msec(*pet_time).to_nsecs(),
        .flags = ZBI_KERNEL_DRIVER_GENERIC32_WATCHDOG_FLAGS_ENABLED,
    };
  }
};

template <template <typename...> class Template>
using WithAllWatchdogs = Template<QualcomMsmWatchdog>;

}  // namespace boot_shim

#endif  // ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_WATCHDOG_H_
