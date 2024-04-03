// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/aml-usb-phy/usb-phy2.h"

#include <lib/driver/logging/cpp/logger.h>

#include "src/devices/usb/drivers/aml-usb-phy/usb-phy-regs.h"
#include "src/devices/usb/drivers/aml-usb-phy/usb-phy2-regs.h"

namespace aml_usb_phy {

namespace {

class PHY2_R : public hwreg::RegisterBase<PHY2_R, uint32_t> {
 public:
  static auto Get(uint32_t reg, uint32_t i) { return hwreg::RegisterAddr<PHY2_R>(reg * 4); }

  static void dump(uint32_t reg, uint32_t i, const fdf::MmioBuffer& mmio, bool starred) {
    FDF_LOG(INFO, "%sPHY2_R%d: %x", (starred) ? "*" : " ", reg,
            PHY2_R::Get(reg, i).ReadFrom(&mmio).reg_value());
  }
};

void dump_phy2_regs(uint32_t idx, const fdf::MmioBuffer& mmio) {
  uint32_t used_regs[] = {0, 1, 3, 4, 13, 14, 16, 17, 18, 20, 21};
  uint32_t cur = 0;
  for (uint32_t i = 0; i < 24; i++) {
    PHY2_R::dump(i, idx, mmio, used_regs[cur] == i);
    if (used_regs[cur] == i) {
      cur++;
    }
  }
}

}  // namespace

void UsbPhy2::dump_regs() const {
  FDF_LOG(INFO, "    UsbPhy2[%d]", idx());
  dump_phy2_regs(idx(), mmio());
}

// Based on set_usb_pll() in phy-aml-new-usb2-v2.c
void UsbPhy2::InitPll(PhyType type, const std::array<uint32_t, 8>& pll_settings) {
  PHY2_R16::Get()
      .FromValue(0)
      .set_value(pll_settings[0])
      .set_enable(1)
      .set_reset(1)
      .WriteTo(&mmio());

  PHY2_R17::Get().FromValue(pll_settings[1]).WriteTo(&mmio());

  PHY2_R18::Get().FromValue(pll_settings[2]).WriteTo(&mmio());

  zx::nanosleep(zx::deadline_after(zx::usec(100)));

  PHY2_R16::Get()
      .FromValue(0)
      .set_value(pll_settings[0])
      .set_enable(1)
      .set_reset(0)
      .WriteTo(&mmio());

  // Phy tuning for G12B
  PHY2_R20::Get().FromValue(pll_settings[3]).WriteTo(&mmio());

  if (type == PhyType::kG12A) {
    PHY2_R4::Get().FromValue(pll_settings[4]).WriteTo(&mmio());

    // Recovery state
    PHY2_R14::Get().FromValue(0).WriteTo(&mmio());
    PHY2_R13::Get().FromValue(pll_settings[5]).WriteTo(&mmio());
  } else if (type == PhyType::kG12B) {
    PHY2_R21::Get().FromValue(0x2a).WriteTo(&mmio());
    PHY2_R13::Get().FromValue(0x70000).WriteTo(&mmio());
  }

  // Disconnect threshold
  PHY2_R3::Get().FromValue(type == PhyType::kG12A ? 0x3c : 0x34).WriteTo(&mmio());
}

void UsbPhy2::SetModeInternal(UsbMode mode, fdf::MmioBuffer& usbctrl_mmio,
                              const std::array<uint32_t, 8>& pll_settings) {
  ZX_DEBUG_ASSERT(mode == UsbMode::Host || mode == UsbMode::Peripheral);
  if ((dr_mode() == UsbMode::Host && mode != UsbMode::Host) ||
      (dr_mode() == UsbMode::Peripheral && mode != UsbMode::Peripheral)) {
    FDF_LOG(ERROR, "If dr_mode_ is not USB_MODE_OTG, dr_mode_ must match requested mode.");
    return;
  }

  FDF_LOG(INFO, "Entering USB %s Mode", mode == UsbMode::Host ? "Host" : "Peripheral");

  if (mode == phy_mode())
    return;

  if (is_otg_capable()) {
    auto r0 = USB_R0_V2::Get().ReadFrom(&usbctrl_mmio);
    if (mode == UsbMode::Host) {
      r0.set_u2d_act(0);
    } else {
      r0.set_u2d_act(1);
      r0.set_u2d_ss_scaledown_mode(0);
    }
    r0.WriteTo(&usbctrl_mmio);

    USB_R4_V2::Get()
        .ReadFrom(&usbctrl_mmio)
        .set_p21_sleepm0(mode == UsbMode::Peripheral)
        .WriteTo(&usbctrl_mmio);
  }

  U2P_R0_V2::Get(idx_)
      .ReadFrom(&usbctrl_mmio)
      .set_host_device(mode == UsbMode::Host)
      .set_por(0)
      .WriteTo(&usbctrl_mmio);
}

}  // namespace aml_usb_phy
