// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_USB_PHY3_H_
#define SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_USB_PHY3_H_

#include <lib/mmio/mmio.h>

#include "src/devices/usb/drivers/aml-usb-phy/usb-phy-base.h"

namespace aml_usb_phy {

class UsbPhy3 final : public UsbPhyBase {
 public:
  UsbPhy3(fdf::MmioBuffer mmio, bool is_otg_capable, UsbMode dr_mode)
      : UsbPhyBase(std::move(mmio), is_otg_capable, dr_mode) {}

  zx_status_t Init(fdf::MmioBuffer& usbctrl_mmio);

  void dump_regs() const override;

 private:
  void SetModeInternal(UsbMode mode, fdf::MmioBuffer& usbctrl_mmio,
                       const std::array<uint32_t, 8>& pll_settings) override {
    // UsbPhy3 only supports host mode as of right now.
    ZX_ASSERT(mode == UsbMode::Host);
  }

  zx_status_t CrBusAddr(uint32_t addr);
  uint32_t CrBusRead(uint32_t addr);
  zx_status_t CrBusWrite(uint32_t addr, uint32_t data);
};

}  // namespace aml_usb_phy

#endif  // SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_USB_PHY3_H_
