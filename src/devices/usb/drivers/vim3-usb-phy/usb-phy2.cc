// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/vim3-usb-phy/usb-phy2.h"

#include <lib/driver/logging/cpp/logger.h>

#include "src/devices/usb/drivers/vim3-usb-phy/usb-phy-regs.h"

namespace vim3_usb_phy {

// Based on set_usb_pll() in phy-aml-new-usb2-v2.c
void UsbPhy2::InitPll(const std::array<uint32_t, 8>& pll_settings) {
  PLL_REGISTER_40::Get()
      .FromValue(0)
      .set_value(pll_settings[0])
      .set_enable(1)
      .set_reset(1)
      .WriteTo(&mmio());

  PLL_REGISTER::Get(0x44).FromValue(pll_settings[1]).WriteTo(&mmio());

  PLL_REGISTER::Get(0x48).FromValue(pll_settings[2]).WriteTo(&mmio());

  zx::nanosleep(zx::deadline_after(zx::usec(100)));

  PLL_REGISTER_40::Get()
      .FromValue(0)
      .set_value(pll_settings[0])
      .set_enable(1)
      .set_reset(0)
      .WriteTo(&mmio());

  // Phy tuning for G12B
  PLL_REGISTER::Get(0x50).FromValue(pll_settings[3]).WriteTo(&mmio());

  PLL_REGISTER::Get(0x54).FromValue(0x2a).WriteTo(&mmio());

  PLL_REGISTER::Get(0x34).FromValue(0x70000).WriteTo(&mmio());

  // Disconnect threshold
  PLL_REGISTER::Get(0xc).FromValue(0x34).WriteTo(&mmio());
}

void UsbPhy2::SetModeInternal(UsbMode mode, fdf::MmioBuffer& usbctrl_mmio,
                              const std::array<uint32_t, 8>& pll_settings) {
  ZX_DEBUG_ASSERT(mode == UsbMode::HOST || mode == UsbMode::PERIPHERAL);
  if ((dr_mode() == USB_MODE_HOST && mode != UsbMode::HOST) ||
      (dr_mode() == USB_MODE_PERIPHERAL && mode != UsbMode::PERIPHERAL)) {
    FDF_LOG(ERROR, "If dr_mode_ is not USB_MODE_OTG, dr_mode_ must match requested mode.");
    return;
  }

  FDF_LOG(INFO, "Entering USB %s Mode", mode == UsbMode::HOST ? "Host" : "Peripheral");

  if (mode == phy_mode())
    return;

  if (is_otg_capable()) {
    auto r0 = USB_R0_V2::Get().ReadFrom(&usbctrl_mmio);
    if (mode == UsbMode::HOST) {
      r0.set_u2d_act(0);
    } else {
      r0.set_u2d_act(1);
      r0.set_u2d_ss_scaledown_mode(0);
    }
    r0.WriteTo(&usbctrl_mmio);

    USB_R4_V2::Get()
        .ReadFrom(&usbctrl_mmio)
        .set_p21_sleepm0(mode == UsbMode::PERIPHERAL)
        .WriteTo(&usbctrl_mmio);
  }

  U2P_R0_V2::Get(idx_)
      .ReadFrom(&usbctrl_mmio)
      .set_host_device(mode == UsbMode::HOST)
      .set_por(0)
      .WriteTo(&usbctrl_mmio);

  zx::nanosleep(zx::deadline_after(zx::usec(500)));

  if (is_otg_capable() && phy_mode() != UsbMode::UNKNOWN) {
    PLL_REGISTER::Get(0x38).FromValue(mode == UsbMode::HOST ? pll_settings[6] : 0).WriteTo(&mmio());
    PLL_REGISTER::Get(0x34).FromValue(pll_settings[5]).WriteTo(&mmio());
  }
}

}  // namespace vim3_usb_phy
