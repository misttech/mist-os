// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_USB_PHY_REGS_H_
#define SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_USB_PHY_REGS_H_

#include <zircon/types.h>

#include <hwreg/bitfields.h>

namespace vim3_usb_phy {

constexpr uint32_t RESET1_REGISTER_OFFSET = 0x8;
constexpr uint32_t RESET1_LEVEL_OFFSET = 0x84;
constexpr uint32_t USB2PHY_PORTCOUNT = 2;

// PHY register offsets
constexpr uint32_t U2P_REGISTER_OFFSET = 32;
constexpr uint32_t U2P_R0_OFFSET = 0x0;
constexpr uint32_t U2P_R1_OFFSET = 0x4;

constexpr uint32_t USB_R0_OFFSET = 0x80;
constexpr uint32_t USB_R1_OFFSET = 0x84;
constexpr uint32_t USB_R2_OFFSET = 0x88;
constexpr uint32_t USB_R3_OFFSET = 0x8c;
constexpr uint32_t USB_R4_OFFSET = 0x90;
constexpr uint32_t USB_R5_OFFSET = 0x94;

constexpr uint32_t USB_PHY_REG21_OFFSET = 0x54;

constexpr uint32_t PHY3_R1_OFFSET = 0x4;
constexpr uint32_t PHY3_R2_OFFSET = 0x8;
constexpr uint32_t PHY3_R4_OFFSET = 0x10;
constexpr uint32_t PHY3_R5_OFFSET = 0x14;

class U2P_R0_V2 : public hwreg::RegisterBase<U2P_R0_V2, uint32_t> {
 public:
  DEF_BIT(0, host_device);
  DEF_BIT(1, power_ok);
  DEF_BIT(2, hast_mode);
  DEF_BIT(3, por);
  DEF_BIT(4, idpullup0);
  DEF_BIT(5, drvvbus0);
  static auto Get(uint32_t i) {
    return hwreg::RegisterAddr<U2P_R0_V2>(i * U2P_REGISTER_OFFSET + U2P_R0_OFFSET);
  }
};

class U2P_R1_V2 : public hwreg::RegisterBase<U2P_R1_V2, uint32_t> {
 public:
  DEF_BIT(0, phy_rdy);
  DEF_BIT(1, iddig0);
  DEF_BIT(2, otgsessvld0);
  DEF_BIT(3, vbusvalid0);
  static auto Get(uint32_t i) {
    return hwreg::RegisterAddr<U2P_R1_V2>(i * U2P_REGISTER_OFFSET + U2P_R1_OFFSET);
  }
};

class USB_R0_V2 : public hwreg::RegisterBase<USB_R0_V2, uint32_t> {
 public:
  DEF_BIT(17, p30_lane0_tx2rx_loopback);
  DEF_BIT(18, p30_lane0_ext_pclk_reg);
  DEF_FIELD(28, 19, p30_pcs_rx_los_mask_val);
  DEF_FIELD(30, 29, u2d_ss_scaledown_mode);
  DEF_BIT(31, u2d_act);
  static auto Get() { return hwreg::RegisterAddr<USB_R0_V2>(USB_R0_OFFSET); }
};

class USB_R1_V2 : public hwreg::RegisterBase<USB_R1_V2, uint32_t> {
 public:
  DEF_BIT(0, u3h_bigendian_gs);
  DEF_BIT(1, u3h_pme_en);
  DEF_FIELD(3, 2, u3h_hub_port_overcurrent);
  DEF_FIELD(8, 6, u3h_hub_port_perm_attach);
  DEF_FIELD(12, 11, u3h_host_u2_port_disable);
  DEF_BIT(16, u3h_host_u3_port_disable);
  DEF_BIT(17, u3h_host_port_power_control_present);
  DEF_BIT(18, u3h_host_msi_enable);
  DEF_FIELD(24, 19, u3h_fladj_30mhz_reg);
  DEF_FIELD(31, 25, p30_pcs_tx_swing_full);
  static auto Get() { return hwreg::RegisterAddr<USB_R1_V2>(USB_R1_OFFSET); }
};

class USB_R2_V2 : public hwreg::RegisterBase<USB_R2_V2, uint32_t> {
 public:
  DEF_FIELD(25, 20, p30_pcs_tx_deemph_3p5db);
  DEF_FIELD(31, 26, p30_pcs_tx_deemph_6db);
  static auto Get() { return hwreg::RegisterAddr<USB_R2_V2>(USB_R2_OFFSET); }
};

class USB_R3_V2 : public hwreg::RegisterBase<USB_R3_V2, uint32_t> {
 public:
  DEF_BIT(0, p30_ssc_en);
  DEF_FIELD(3, 1, p30_ssc_range);
  DEF_FIELD(12, 4, p30_ssc_ref_clk_sel);
  DEF_BIT(13, p30_ref_ssp_en);
  static auto Get() { return hwreg::RegisterAddr<USB_R3_V2>(USB_R3_OFFSET); }
};

class USB_R4_V2 : public hwreg::RegisterBase<USB_R4_V2, uint32_t> {
 public:
  DEF_BIT(0, p21_portreset0);
  DEF_BIT(1, p21_sleepm0);
  DEF_FIELD(3, 2, mem_pd);
  DEF_BIT(4, p21_only);
  static auto Get() { return hwreg::RegisterAddr<USB_R4_V2>(USB_R4_OFFSET); }
};

class USB_R5_V2 : public hwreg::RegisterBase<USB_R5_V2, uint32_t> {
 public:
  DEF_BIT(0, iddig_sync);
  DEF_BIT(1, iddig_reg);
  DEF_FIELD(3, 2, iddig_cfg);
  DEF_BIT(4, iddig_en0);
  DEF_BIT(5, iddig_en1);
  DEF_BIT(6, iddig_curr);
  DEF_BIT(7, usb_iddig_irq);
  DEF_FIELD(15, 8, iddig_th);
  DEF_FIELD(23, 16, iddig_cnt);
  static auto Get() { return hwreg::RegisterAddr<USB_R5_V2>(USB_R5_OFFSET); }
};

class USB_PHY_REG21 : public hwreg::RegisterBase<USB_PHY_REG21, uint32_t> {
 public:
  DEF_FIELD(31, 26, reg32_21_31_26_reserved);
  DEF_FIELD(25, 20, bypass_utmi_reg);
  DEF_FIELD(19, 16, bypass_utmi_cntr);
  DEF_FIELD(15, 6, reg32_21_15_6_reserved);
  DEF_FIELD(5, 4, usb2_otg_aca_trim_1_0);
  DEF_BIT(3, usb2_tx_strg_pd);
  DEF_BIT(2, usb2_otg_aca_en);
  DEF_BIT(1, usb2_cal_ack_en);
  DEF_BIT(0, usb2_bgr_force);
  static auto Get() { return hwreg::RegisterAddr<USB_PHY_REG21>(USB_PHY_REG21_OFFSET); }
};

// Undocumented PLL registers used for PHY tuning.
class PLL_REGISTER : public hwreg::RegisterBase<PLL_REGISTER, uint32_t> {
 public:
  static auto Get(uint32_t i) { return hwreg::RegisterAddr<PLL_REGISTER>(i); }
};

class PLL_REGISTER_40 : public hwreg::RegisterBase<PLL_REGISTER_40, uint32_t> {
 public:
  DEF_FIELD(27, 0, value);
  DEF_BIT(28, enable);
  DEF_BIT(29, reset);
  static auto Get() { return hwreg::RegisterAddr<PLL_REGISTER_40>(0x40); }
};

// USB3 PHY Registers

class PHY3_R1 : public hwreg::RegisterBase<PHY3_R1, uint32_t> {
 public:
  DEF_FIELD(4, 0, phy_tx1_term_offset);
  DEF_FIELD(9, 5, phy_tx0_term_offset);
  DEF_FIELD(12, 10, phy_rx1_eq);
  DEF_FIELD(15, 13, phy_rx0_eq);
  DEF_FIELD(20, 16, phy_los_level);
  DEF_FIELD(23, 21, phy_los_bias);
  DEF_BIT(24, phy_ref_clkdiv2);
  DEF_FIELD(31, 25, phy_mpll_multiplier);
  static auto Get() { return hwreg::RegisterAddr<PHY3_R1>(PHY3_R1_OFFSET); }
};

class PHY3_R2 : public hwreg::RegisterBase<PHY3_R2, uint32_t> {
 public:
  DEF_FIELD(5, 0, pcs_tx_deemph_gen2_6db);
  DEF_FIELD(11, 6, pcs_tx_deemph_gen2_3p5db);
  DEF_FIELD(17, 12, pcs_tx_deemph_gen1);
  DEF_FIELD(20, 18, phy_tx_vboost_lvl);
  DEF_FIELD(31, 21, reserved);
  static auto Get() { return hwreg::RegisterAddr<PHY3_R2>(PHY3_R2_OFFSET); }
};

class PHY3_R4 : public hwreg::RegisterBase<PHY3_R4, uint32_t> {
 public:
  DEF_BIT(0, phy_cr_write);
  DEF_BIT(1, phy_cr_read);
  DEF_FIELD(17, 2, phy_cr_data_in);
  DEF_BIT(18, phy_cr_cap_data);
  DEF_BIT(19, phy_cr_cap_addr);
  DEF_FIELD(31, 20, reserved);
  static auto Get() { return hwreg::RegisterAddr<PHY3_R4>(PHY3_R4_OFFSET); }
};

class PHY3_R5 : public hwreg::RegisterBase<PHY3_R5, uint32_t> {
 public:
  DEF_FIELD(15, 0, phy_cr_data_out);
  DEF_BIT(16, phy_cr_ack);
  DEF_BIT(17, phy_bs_out);
  DEF_FIELD(31, 18, reserved);
  static auto Get() { return hwreg::RegisterAddr<PHY3_R5>(PHY3_R5_OFFSET); }
};

}  // namespace vim3_usb_phy

#endif  // SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_USB_PHY_REGS_H_
