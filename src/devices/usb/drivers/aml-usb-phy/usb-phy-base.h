// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_USB_PHY_BASE_H_
#define SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_USB_PHY_BASE_H_

#include <lib/mmio/mmio.h>

#include <soc/aml-common/aml-usb-phy.h>

namespace aml_usb_phy {

class UsbPhyBase {
 public:
  const fdf::MmioBuffer& mmio() const { return mmio_; }
  bool is_otg_capable() const { return is_otg_capable_; }
  UsbMode dr_mode() const { return dr_mode_; }

  UsbMode phy_mode() { return phy_mode_; }
  void SetMode(UsbMode mode, fdf::MmioBuffer& usbctrl_mmio,
               const std::array<uint32_t, 8>& pll_settings) {
    SetModeInternal(mode, usbctrl_mmio, pll_settings);
    phy_mode_ = mode;
  }

  // Used for debugging.
  virtual void dump_regs() const = 0;

 protected:
  UsbPhyBase(fdf::MmioBuffer mmio, bool is_otg_capable, UsbMode dr_mode)
      : mmio_(std::move(mmio)), is_otg_capable_(is_otg_capable), dr_mode_(dr_mode) {}

 private:
  virtual void SetModeInternal(UsbMode mode, fdf::MmioBuffer& usbctrl_mmio,
                               const std::array<uint32_t, 8>& pll_settings) = 0;

  fdf::MmioBuffer mmio_;
  const bool is_otg_capable_;
  const UsbMode dr_mode_;  // USB Controller Mode. Internal to Driver.

  UsbMode phy_mode_ =
      UsbMode::Unknown;  // Physical USB mode. Must hold parent's lock_ while accessing.
};

}  // namespace aml_usb_phy

#endif  // SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_USB_PHY_BASE_H_
