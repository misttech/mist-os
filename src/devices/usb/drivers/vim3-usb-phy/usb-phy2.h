// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_USB_PHY2_H_
#define SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_USB_PHY2_H_

#include <lib/mmio/mmio.h>

#include <usb/usb.h>

#include "src/devices/usb/drivers/vim3-usb-phy/usb-phy-base.h"

namespace vim3_usb_phy {

class Vim3UsbPhy;

class UsbPhy2 final : public UsbPhyBase {
 public:
  UsbPhy2(uint8_t idx, fdf::MmioBuffer mmio, bool is_otg_capable, usb_mode_t dr_mode)
      : UsbPhyBase(std::move(mmio), is_otg_capable, dr_mode), idx_(idx) {}

  void InitPll(const std::array<uint32_t, 8>& pll_settings);

  uint8_t idx() const { return idx_; }

 private:
  void SetModeInternal(UsbMode mode, fdf::MmioBuffer& usbctrl_mmio,
                       const std::array<uint32_t, 8>& pll_settings) override;

  const uint8_t idx_;  // For indexing into usbctrl_mmio.
};

}  // namespace vim3_usb_phy

#endif  // SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_USB_PHY2_H_
