// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_USB_PHY2_REGS_H_
#define SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_USB_PHY2_REGS_H_

#include <lib/mmio/mmio.h>
#include <zircon/types.h>

#include <hwreg/bitfields.h>

#define DECLARE_PHY2_R(idx)                                                              \
  constexpr uint32_t PHY2_R##idx##_OFFSET = 0x4 * (idx);                                 \
  class PHY2_R##idx : public hwreg::RegisterBase<PHY2_R##idx, uint32_t> {                \
   public:                                                                               \
    static auto Get() { return hwreg::RegisterAddr<PHY2_R##idx>(PHY2_R##idx##_OFFSET); } \
  };

namespace aml_usb_phy {

constexpr uint32_t PHY2_R16_OFFSET = 0x40;
constexpr uint32_t PHY2_R21_OFFSET = 0x54;

DECLARE_PHY2_R(3)
DECLARE_PHY2_R(4)
DECLARE_PHY2_R(13)
DECLARE_PHY2_R(14)

class PHY2_R16 : public hwreg::RegisterBase<PHY2_R16, uint32_t> {
 public:
  DEF_FIELD(27, 0, value);
  DEF_BIT(28, enable);
  DEF_BIT(29, reset);
  static auto Get() { return hwreg::RegisterAddr<PHY2_R16>(PHY2_R16_OFFSET); }
};

DECLARE_PHY2_R(17)
DECLARE_PHY2_R(18)
DECLARE_PHY2_R(20)

class PHY2_R21 : public hwreg::RegisterBase<PHY2_R21, uint32_t> {
 public:
  DEF_BIT(0, usb2_bgr_force);
  DEF_BIT(1, usb2_cal_ack_en);
  DEF_BIT(2, usb2_otg_aca_en);
  DEF_BIT(3, usb2_tx_strg_pd);
  DEF_FIELD(5, 4, usb2_otg_aca_trim_1_0);
  DEF_FIELD(19, 16, bypass_utmi_cntr);
  DEF_FIELD(25, 20, bypass_utmi_reg);
  static auto Get() { return hwreg::RegisterAddr<PHY2_R21>(PHY2_R21_OFFSET); }
};

}  // namespace aml_usb_phy

#endif  // SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_USB_PHY2_REGS_H_
