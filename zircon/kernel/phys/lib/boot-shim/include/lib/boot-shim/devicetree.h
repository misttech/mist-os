// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_H_
#define ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_H_

#include <lib/boot-shim/item-base.h>
#include <lib/devicetree/devicetree.h>
#include <lib/devicetree/matcher.h>
#include <lib/stdcompat/array.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/zbi.h>
#include <stdio.h>

#include <cstdarg>

#include <fbl/type_info.h>

namespace boot_shim {

// Base class for DevicetreeItems, providing default implementations for the Matcher API.
// Derived classes MUST implement OnNode.
template <typename T, size_t MaxScans>
class DevicetreeItemBase {
 public:
  static constexpr size_t kMaxScans = MaxScans;

  devicetree::ScanState OnNode(const devicetree::NodePath&, const devicetree::PropertyDecoder&) {
    static_assert(kMaxScans != MaxScans, "Must implement OnNode.");
    return devicetree::ScanState::kActive;
  }

  devicetree::ScanState OnWalk() { return devicetree::ScanState::kActive; }

  void OnError(std::string_view error) {
    Log("Error on %s, %*s\n", fbl::TypeInfo<T>::Name(), static_cast<int>(error.length()),
        error.data());
  }

  devicetree::ScanState OnSubtree(const devicetree::NodePath&) {
    return devicetree::ScanState::kActive;
  }

  template <typename Shim>
  void Init(const Shim& shim) {
    static_assert(devicetree::kIsMatcher<T>);
    shim_name_ = shim.shim_name();
    log_ = shim.log();
  }

 protected:
  // Helper for logging in to |log_|.
  void Log(const char* fmt, ...) __PRINTFLIKE(2, 3) {
    fprintf(log_, "%s: ", shim_name_);
    va_list ap;
    va_start(ap, fmt);
    vfprintf(log_, fmt, ap);
    va_end(ap);
  }

 private:
  FILE* log_;
  const char* shim_name_;
};

// Decodes PSCI information from a devicetree and synthesizes a
// DRIVER_CONFIG ZBI item for it.
//
// A PSCI device is encoded within a node called "psci" with a "compatible" property
// giving its compatible PSCI revisions (i.e., values of `kCompatibleDevices` below).
//
// For example,
//
// psci {
//      compatible  = "arm,psci-0.2";
//      method      = "smc";
// };
//
// For more details please see
// https://www.kernel.org/doc/Documentation/devicetree/bindings/arm/psci.txt
class ArmDevicetreePsciItem
    : public DevicetreeItemBase<ArmDevicetreePsciItem, 1>,
      public SingleOptionalItem<zbi_dcfg_arm_psci_driver_t, ZBI_TYPE_KERNEL_DRIVER,
                                ZBI_KERNEL_DRIVER_ARM_PSCI> {
 public:
  static constexpr auto kCompatibleDevices = cpp20::to_array<std::string_view>({
      // PSCI 0.1 : Not Supported.
      // "arm,psci",
      "arm,psci-0.2",
      "arm,psci-1.0",
  });

  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);

 private:
  devicetree::ScanState HandlePsciNode(const devicetree::NodePath& path,
                                       const devicetree::PropertyDecoder& decoder);
};

}  // namespace boot_shim

#endif  // ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_H_
