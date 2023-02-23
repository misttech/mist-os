// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_A1_USB_PHY_USB_PHY_REGS_H_
#define SRC_DEVICES_USB_DRIVERS_A1_USB_PHY_USB_PHY_REGS_H_

#include <zircon/types.h>

#include <hwreg/bitfields.h>

namespace a1_usb_phy {

constexpr uint32_t RESET1_REGISTER_OFFSET = 0x4;
constexpr uint32_t RESET1_LEVEL_OFFSET = 0x44;

// PHY register offsets
constexpr uint32_t U2P_REGISTER_OFFSET = 32;
constexpr uint32_t U2P_R0_OFFSET = 0x20;
constexpr uint32_t U2P_R1_OFFSET = 0x24;

constexpr uint32_t USB_R0_OFFSET = 0x80;
constexpr uint32_t USB_R1_OFFSET = 0x84;
constexpr uint32_t USB_R2_OFFSET = 0x88;
constexpr uint32_t USB_R3_OFFSET = 0x8c;
constexpr uint32_t USB_R4_OFFSET = 0x90;
constexpr uint32_t USB_R5_OFFSET = 0x94;
constexpr uint32_t USB_BUSCLK_OFFSET = 0xdc;

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
  DEF_FIELD(4, 2, u3h_hub_port_overcurrent);
  DEF_FIELD(6, 5, reserved_1);
  DEF_FIELD(9, 7, u3h_hub_port_perm_attach);
  DEF_FIELD(11, 10, reserved_2);
  DEF_FIELD(13, 12, u3h_host_u2_port_disable);
  DEF_FIELD(15, 14, reserved_3);
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

class USB_BUSCLK_CTRL : public hwreg::RegisterBase<USB_BUSCLK_CTRL, uint32_t> {
 public:
  DEF_FIELD(7, 0, clock_div);
  DEF_BIT(8, clock_gate_en);
  DEF_FIELD(10, 9, clock_source_select);
  static auto Get() { return hwreg::RegisterAddr<USB_BUSCLK_CTRL>(USB_BUSCLK_OFFSET); }
};

// Undocumented PLL registers used for PHY tuning.
class PLL_REGISTER_C : public hwreg::RegisterBase<PLL_REGISTER_C, uint32_t> {
 public:
  DEF_FIELD(2, 0, squelch_ref);
  DEF_FIELD(4, 3, hsdic_ref);
  DEF_FIELD(7, 5, TBD_4);
  DEF_FIELD(15, 8, TBD_5);
  DEF_FIELD(23, 16, TBD_6);
  DEF_FIELD(31, 24, TBD_7);
  static auto Get() { return hwreg::RegisterAddr<PLL_REGISTER_C>(0xc); }
};

class PLL_REGISTER_34 : public hwreg::RegisterBase<PLL_REGISTER_34, uint32_t> {
 public:
  DEF_FIELD(7, 0, Custom_Pattern_19);
  DEF_FIELD(13, 8, reserved0);
  DEF_BIT(14, load_stat);
  DEF_BIT(15, Update_PMA_signals);
  DEF_FIELD(20, 16, minimum_count_for_sync_detection);
  DEF_BIT(21, Clear_Hold_HS_disconnect);
  DEF_BIT(22, Bypass_Host_Disconnect_Value);
  DEF_BIT(23, Bypass_Host_Disconnect_Enable);
  DEF_BIT(24, bypass_reg_0_i_c2l_hs_en);
  DEF_BIT(25, bypass_reg_1_i_c2l_fs_en);
  DEF_BIT(26, bypass_reg_2_i_c2l_ls_en);
  DEF_BIT(27, bypass_reg_3_i_c2l_hs_oe);
  DEF_BIT(28, bypass_reg_4_i_c2l_fs_oe);
  DEF_BIT(29, bypass_reg_5_i_c2l_hs_rx_en);
  DEF_BIT(30, bypass_reg_6_i_c2l_fsls_rx_en);
  static auto Get() { return hwreg::RegisterAddr<PLL_REGISTER_34>(0x34); }
};

class PLL_REGISTER_38 : public hwreg::RegisterBase<PLL_REGISTER_38, uint32_t> {
 public:
  DEF_BIT(0, i_rpd_en);
  DEF_BIT(1, bypass_reg_8_i_rpd_en);
  DEF_FIELD(3, 2, bypass_reg_9_i_rpu_sw2_en);
  DEF_BIT(4, bypass_reg_11_10_i_rpu_sw1_en);
  DEF_BIT(5, pg_rstn);
  DEF_BIT(6, i_c2l_data_16_8);
  DEF_BIT(7, i_c2l_assert_single_enable_zero);
  DEF_FIELD(15, 8, reserved0);
  DEF_BIT(16, bypass_ctrl_0_hs);
  DEF_BIT(17, bypass_ctrl_1_fs);
  DEF_BIT(18, bypass_ctrl_2_ls);
  DEF_BIT(19, bypass_ctrl_3_hs_out_en);
  DEF_BIT(20, bypass_ctrl_4_fsls_out_en);
  DEF_BIT(21, bypass_ctrl_5_hs_rx_en);
  DEF_BIT(22, bypass_ctrl_6_hls_rx_en);
  DEF_BIT(23, reserved1);
  DEF_BIT(24, bypass_ctrl_8_i_rpd_en);
  DEF_BIT(25, bypass_ctrl_9_i_rpu_sw2_en);
  DEF_BIT(26, bypass_ctrl_10_i_rpu_sw1_en);
  static auto Get() { return hwreg::RegisterAddr<PLL_REGISTER_38>(0x38); }
};

class PLL_REGISTER_40 : public hwreg::RegisterBase<PLL_REGISTER_40, uint32_t> {
 public:
  DEF_FIELD(27, 0, value);
  DEF_BIT(28, enable);
  DEF_BIT(29, reset);
  static auto Get() { return hwreg::RegisterAddr<PLL_REGISTER_40>(0x40); }
};

class PLL_REGISTER_44 : public hwreg::RegisterBase<PLL_REGISTER_44, uint32_t> {
 public:
  DEF_FIELD(13, 0, usb2_mppll_frac_in);
  DEF_FIELD(15, 14, reserved0);
  DEF_BIT(16, usb2_mppll_fix_en);
  DEF_FIELD(19, 17, usb2_mppll_lambda1);
  DEF_FIELD(22, 20, usb2_mppll_lambda0);
  DEF_BIT(23, usb2_mppll_filter_mode);
  DEF_FIELD(27, 24, usb2_mppll_filter_pvt2);
  DEF_FIELD(31, 28, usb2_mppll_filter_pvt1);
  static auto Get() { return hwreg::RegisterAddr<PLL_REGISTER_44>(0x44); }
};

class PLL_REGISTER_48 : public hwreg::RegisterBase<PLL_REGISTER_48, uint32_t> {
 public:
  DEF_FIELD(1, 0, usb2_mppll_lkw_sel);
  DEF_FIELD(5, 2, usb2_mppll_lk_w);
  DEF_FIELD(11, 6, usb2_mppll_lk_s);
  DEF_BIT(12, usb2_mppll_dco_m_en);
  DEF_BIT(13, usb2_mppll_dco_clk_sel);
  DEF_FIELD(15, 14, usb2_mppll_pfd_gain);
  DEF_FIELD(18, 16, usb2_mppll_rou);
  DEF_FIELD(21, 19, usb2_mppll_data_sel);
  DEF_FIELD(23, 22, usb2_mppll_bias_adj);
  DEF_FIELD(25, 24, usb2_mppll_bb_mode);
  DEF_FIELD(28, 26, usb2_mppll_alpha);
  DEF_FIELD(30, 29, usb2_mppll_adj_ldo);
  DEF_BIT(31, usb2_mppll_acg_range);
  static auto Get() { return hwreg::RegisterAddr<PLL_REGISTER_48>(0x48); }
};

class PLL_REGISTER_50 : public hwreg::RegisterBase<PLL_REGISTER_50, uint32_t> {
 public:
  DEF_BIT(0, usb2_otg_iddet_en);
  DEF_FIELD(3, 1, usb2_otg_vbus_trim_2_0);
  DEF_BIT(4, usb2_otg_vbusdet_en);
  DEF_BIT(5, usb2_amon_en);
  DEF_BIT(6, usb2_cal_code_r5);
  DEF_BIT(7, bypass_otg_det);
  DEF_BIT(8, usb2_dmon_en);
  DEF_FIELD(12, 9, usb2_dmon_sel_3_0);
  DEF_BIT(13, usb2_edgedrv_en);
  DEF_FIELD(15, 14, usb2_edgedrv_trim_1_0);
  DEF_FIELD(20, 16, usb2_bgr_adj_4_0);
  DEF_BIT(21, usb2_bgr_start);
  DEF_FIELD(23, 22, squelch_sel);
  DEF_FIELD(28, 24, usb2_bgr_vref_4_0);
  DEF_FIELD(30, 29, usb2_bgr_dbg_1_0);
  DEF_BIT(31, bypass_cal_done_r5);
  static auto Get() { return hwreg::RegisterAddr<PLL_REGISTER_50>(0x50); }
};

class PLL_REGISTER_54 : public hwreg::RegisterBase<PLL_REGISTER_54, uint32_t> {
 public:
  DEF_BIT(0, usb2_bgr_force);
  DEF_BIT(1, usb2_cal_ack_en);
  DEF_BIT(2, usb2_otg_aca_en);
  DEF_BIT(3, usb2_tx_strg_pd);
  DEF_FIELD(5, 4, usb2_otg_aca_trim_1_0);
  DEF_BIT(6, reserved0);
  DEF_BIT(7, hs_cdr_sel);
  DEF_FIELD(15, 8, hs_cdr_ctr);
  DEF_FIELD(19, 16, bypass_utmi_cntr);
  DEF_FIELD(25, 20, bypass_utmi_reg);
  static auto Get() { return hwreg::RegisterAddr<PLL_REGISTER_54>(0x54); }
};

class RESET_REGISTER : public hwreg::RegisterBase<RESET_REGISTER, uint32_t> {
 public:
  DEF_BIT(0, acodec);
  DEF_BIT(1, dma);
  DEF_BIT(2, sd_emmc_a);
  DEF_BIT(3, reserved0);
  DEF_BIT(4, usbctrl);
  DEF_BIT(5, reserved1);
  DEF_BIT(6, usbphy);
  DEF_FIELD(9, 7, reserved2);
  DEF_BIT(10, rsa);
  DEF_BIT(11, dmc);
  DEF_BIT(12, reserved3);
  DEF_BIT(13, irq_ctrl);
  DEF_BIT(14, reserved4);
  DEF_BIT(15, nic_vad);
  DEF_BIT(16, nic_axi);
  DEF_BIT(17, rama);
  DEF_BIT(18, ramb);
  DEF_FIELD(20, 19, reserved5);
  DEF_BIT(21, rom);
  DEF_BIT(22, spifc);
  DEF_BIT(23, gic);
  DEF_BIT(24, uart_c);
  DEF_BIT(25, uart_b);
  DEF_BIT(26, uart_a);
  DEF_BIT(27, osc_ring);
  static auto Get(uint32_t i) { return hwreg::RegisterAddr<RESET_REGISTER>(i); }
};

}  // namespace a1_usb_phy

#endif  //  SRC_DEVICES_USB_DRIVERS_A1_USB_PHY_USB_PHY_REGS_H_
