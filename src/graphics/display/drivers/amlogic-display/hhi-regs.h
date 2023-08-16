// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_HHI_REGS_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_HHI_REGS_H_

#include <hwreg/bitfields.h>

// The register definitions here are from AMLogic A311D Datasheet version 08.
//
// This datasheet is distributed by Khadas for the VIM3, at
// https://dl.khadas.com/hardware/VIM3/Datasheet/A311D_Datasheet_08_Wesion.pdf

#define READ32_HHI_REG(a) hhi_mmio_->Read32(a)
#define WRITE32_HHI_REG(a, v) hhi_mmio_->Write32(v, a)

// clang-format off
#define HHI_MIPI_CNTL0 (0x000 << 2)
#define HHI_MIPI_CNTL1 (0x001 << 2)
#define HHI_MIPI_CNTL2 (0x002 << 2)
#define HHI_GCLK_MPEG2 (0x052 << 2)
#define HHI_VID_PLL_CLK_DIV (0x068 << 2)
#define HHI_VDAC_CNTL0_G12A (0x0bb << 2)
#define HHI_VDAC_CNTL1_G12A (0x0bc << 2)
#define HHI_HDMI_PLL_CNTL0 (0x0c8 << 2)
    #define PLL_CNTL_LOCK (1 << 31)
    #define PLL_CNTL_ENABLE (1 << 30)
    #define G12A_PLL_CNTL_RESET (1 << 29)
    #define PLL_CNTL_RESET (1 << 28)
    #define PLL_CNTL_N(x) (x << 9)
    #define PLL_CNTL_M_START (0)
    #define PLL_CNTL_M_BITS (9)
#define HHI_HDMI_PLL_CNTL1 (0x0c9 << 2)
    #define PLL_CNTL1_DIV_FRAC_START (0)
    #define PLL_CNTL1_DIV_FRAC_BITS (12)
#define HHI_HDMI_PLL_CNTL2 (0x0ca << 2)
#define HHI_HDMI_PLL_CNTL3 (0x0cb << 2)
#define HHI_HDMI_PLL_CNTL4 (0x0cc << 2)
#define HHI_HDMI_PLL_CNTL5 (0x0cd << 2)
#define HHI_HDMI_PLL_CNTL6 (0x0ce << 2)
#define HHI_HDMI_PLL_STS (0x0cf << 2)
#define HHI_HDMI_PLL_VLOCK_CNTL (0x0d1 << 2)
#define HHI_HDMI_PHY_CNTL0 (0xe8 << 2)
#define HHI_HDMI_PHY_CNTL1 (0xe9 << 2)
#define HHI_HDMI_PHY_CNTL2 (0xea << 2)
#define HHI_HDMI_PHY_CNTL3 (0xeb << 2)
#define HHI_HDMI_PHY_CNTL4 (0xec << 2)
#define HHI_HDMI_PHY_CNTL5 (0xed << 2)
#define HHI_HDMI_PHY_STATUS (0xee << 2)

#define ENCL_VIDEO_FILT_CTRL (0x1cc2 << 2)
#define ENCL_VIDEO_MAX_PXCNT (0x1cb0 << 2)
#define ENCL_VIDEO_HAVON_END (0x1cb1 << 2)
#define ENCL_VIDEO_HAVON_BEGIN (0x1cb2 << 2)
#define ENCL_VIDEO_VAVON_ELINE (0x1cb3 << 2)
#define ENCL_VIDEO_VAVON_BLINE (0x1cb4 << 2)
#define ENCL_VIDEO_HSO_BEGIN (0x1cb5 << 2)
#define ENCL_VIDEO_HSO_END (0x1cb6 << 2)
#define ENCL_VIDEO_VSO_BEGIN (0x1cb7 << 2)
#define ENCL_VIDEO_VSO_END (0x1cb8 << 2)
#define ENCL_VIDEO_VSO_BLINE (0x1cb9 << 2)
#define ENCL_VIDEO_VSO_ELINE (0x1cba << 2)
#define ENCL_VIDEO_MAX_LNCNT (0x1cbb << 2)
#define ENCL_VIDEO_MODE (0x1ca7 << 2)
#define ENCL_VIDEO_MODE_ADV (0x1ca8 << 2)
#define ENCL_VIDEO_RGBIN_CTRL (0x1cc7 << 2)
#define ENCL_VIDEO_EN (0x1ca0 << 2)

#define L_DE_HE_ADDR (0x1452 << 2)
#define L_DE_HS_ADDR (0x1451 << 2)
#define L_DE_VE_ADDR (0x1454 << 2)
#define L_DE_VS_ADDR (0x1453 << 2)
#define L_DITH_CNTL_ADDR (0x1408 << 2)
#define L_HSYNC_HE_ADDR (0x1456 << 2)
#define L_HSYNC_HS_ADDR (0x1455 << 2)
#define L_HSYNC_VS_ADDR (0x1457 << 2)
#define L_HSYNC_VE_ADDR (0x1458 << 2)
#define L_VSYNC_HS_ADDR (0x1459 << 2)
#define L_VSYNC_HE_ADDR (0x145a << 2)
#define L_VSYNC_VS_ADDR (0x145b << 2)
#define L_VSYNC_VE_ADDR (0x145c << 2)
#define L_OEH_HS_ADDR (0x1418 << 2)
#define L_OEH_HE_ADDR (0x1419 << 2)
#define L_OEH_VS_ADDR (0x141a << 2)
#define L_OEH_VE_ADDR (0x141b << 2)
#define L_OEV1_HS_ADDR (0x142f << 2)
#define L_OEV1_HE_ADDR (0x1430 << 2)
#define L_OEV1_VS_ADDR (0x1431 << 2)
#define L_OEV1_VE_ADDR (0x1432 << 2)
#define L_RGB_BASE_ADDR (0x1405 << 2)
#define L_RGB_COEFF_ADDR (0x1406 << 2)
#define L_STH1_HS_ADDR (0x1410 << 2)
#define L_STH1_HE_ADDR (0x1411 << 2)
#define L_STH1_VS_ADDR (0x1412 << 2)
#define L_STH1_VE_ADDR (0x1413 << 2)
#define L_TCON_MISC_SEL_ADDR (0x1441 << 2)
#define L_STV1_HS_ADDR (0x1427 << 2)
#define L_STV1_HE_ADDR (0x1428 << 2)
#define L_STV1_VS_ADDR (0x1429 << 2)
#define L_STV1_VE_ADDR (0x142a << 2)
#define L_INV_CNT_ADDR (0x1440 << 2)

#define VPP_MISC (0x1d26 << 2)
#define VPP_OUT_SATURATE (1 << 0)

// HHI_MIPI_CNTL0 Register Bit Def
#define MIPI_CNTL0_CMN_REF_GEN_CTRL(x) (x << 26)
#define MIPI_CNTL0_VREF_SEL(x) (x << 25)
#define VREF_SEL_VR 0
#define VREF_SEL_VBG 1
#define MIPI_CNTL0_LREF_SEL(x) (x << 24)
#define LREF_SEL_L_ROUT 0
#define LREF_SEL_LBG 1
#define MIPI_CNTL0_LBG_EN (1 << 23)
#define MIPI_CNTL0_VR_TRIM_CNTL(x) (x << 16)
#define MIPI_CNTL0_VR_GEN_FROM_LGB_EN (1 << 3)
#define MIPI_CNTL0_VR_GEN_BY_AVDD18_EN (1 << 2)

// HHI_MIPI_CNTL1 Register Bit Def
#define MIPI_CNTL1_DSI_VBG_EN (1 << 16)
#define MIPI_CNTL1_CTL (0x2e << 0)

// HHI_MIPI_CNTL2 Register Bit Def
#define MIPI_CNTL2_DEFAULT_VAL (0x2680fc5a)  // 4-lane DSI LINK

// HHI_HDMI_PHY_CNTL Register Bit Def
#define LCD_PLL_LOCK_HPLL_G12A (31)
#define LCD_PLL_EN_HPLL_G12A (28)
#define LCD_PLL_RST_HPLL_G12A (29)
#define LCD_PLL_OUT_GATE_CTRL_G12A (25)
#define LCD_PLL_OD3_HPLL_G12A (20)
#define LCD_PLL_OD2_HPLL_G12A (18)
#define LCD_PLL_OD1_HPLL_G12A (16)
#define LCD_PLL_N_HPLL_G12A (10)
#define LCD_PLL_M_HPLL_G12A (0)
// clang-format on

namespace amlogic_display {

// A311D datasheet Section 8.7.5 "Registers"

// HHI_HDMI_PLL_CNTL0
class HhiHdmiPllCntlReg : public hwreg::RegisterBase<HhiHdmiPllCntlReg, uint32_t> {
 public:
  DEF_BIT(31, hdmi_dpll_lock);
  DEF_BIT(30, hdmi_dpll_lock_a);
  DEF_BIT(29, hdmi_dpll_reset);
  DEF_BIT(28, hdmi_dpll_en);
  DEF_FIELD(25, 24, hdmi_dpll_out_gate_ctrl);
  DEF_FIELD(21, 20, hdmi_dpll_od3);
  DEF_FIELD(19, 18, hdmi_dpll_od2);
  DEF_FIELD(17, 16, hdmi_dpll_od1);
  DEF_FIELD(14, 10, hdmi_dpll_N);
  DEF_FIELD(7, 0, hdmi_dpll_M);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPllCntlReg>(HHI_HDMI_PLL_CNTL0); }
};

// HHI_HDMI_PLL_CNTL1
class HhiHdmiPllCntl1Reg : public hwreg::RegisterBase<HhiHdmiPllCntl1Reg, uint32_t> {
 public:
  DEF_FIELD(18, 0, hdmi_dpll_frac);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPllCntl1Reg>(HHI_HDMI_PLL_CNTL1); }
};

// HHI_HDMI_PLL_CNTL2
class HhiHdmiPllCntl2Reg : public hwreg::RegisterBase<HhiHdmiPllCntl2Reg, uint32_t> {
 public:
  DEF_FIELD(22, 20, hdmi_dpll_fref_sel);
  DEF_FIELD(17, 16, hdmi_dpll_os_ssc);
  DEF_FIELD(15, 12, hdmi_dpll_ssc_str_m);
  DEF_BIT(8, hdmi_dpll_ssc_en);
  DEF_FIELD(7, 4, hdmi_dpll_ssc_dep_sel);
  DEF_FIELD(1, 0, hdmi_dpll_ss_mode);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPllCntl2Reg>(HHI_HDMI_PLL_CNTL2); }
};

// HHI_HDMI_PLL_CNTL3
class HhiHdmiPllCntl3Reg : public hwreg::RegisterBase<HhiHdmiPllCntl3Reg, uint32_t> {
 public:
  DEF_BIT(31, hdmi_dpll_afc_bypass);
  DEF_BIT(30, hdmi_dpll_afc_clk_sel);
  DEF_BIT(29, hdmi_dpll_code_new);
  DEF_BIT(28, hdmi_dpll_dco_m_en);
  DEF_BIT(27, hdmi_dpll_dco_sdm_en);
  DEF_BIT(26, hdmi_dpll_div2);
  DEF_BIT(25, hdmi_dpll_div_mode);
  DEF_BIT(24, hdmi_dpll_fast_lock);
  DEF_BIT(23, hdmi_dpll_fb_pre_div);
  DEF_BIT(22, hdmi_dpll_filter_mode);
  DEF_BIT(21, hdmi_dpll_fix_en);
  DEF_BIT(20, hdmi_dpll_freq_shift_en);
  DEF_BIT(19, hdmi_dpll_load);
  DEF_BIT(18, hdmi_dpll_load_en);
  DEF_BIT(17, hdmi_dpll_lock_f);
  DEF_BIT(16, hdmi_dpll_pulse_width_en);
  DEF_BIT(15, hdmi_dpll_sdmnc_en);
  DEF_BIT(14, hdmi_dpll_sdmnc_mode);
  DEF_BIT(13, hdmi_dpll_sdmnc_range);
  DEF_BIT(12, hdmi_dpll_tdc_en);
  DEF_BIT(11, hdmi_dpll_tdc_mode_sel);
  DEF_BIT(10, hdmi_dpll_wait_en);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPllCntl3Reg>(HHI_HDMI_PLL_CNTL3); }
};

// HHI_HDMI_PLL_CNTL4
class HhiHdmiPllCntl4Reg : public hwreg::RegisterBase<HhiHdmiPllCntl4Reg, uint32_t> {
 public:
  DEF_FIELD(30, 28, hdmi_dpll_alpha);
  DEF_FIELD(26, 24, hdmi_dpll_rou);
  DEF_FIELD(22, 20, hdmi_dpll_lambda1);
  DEF_FIELD(18, 16, hdmi_dpll_lambda0);
  DEF_FIELD(13, 12, hdmi_dpll_acq_gain);
  DEF_FIELD(11, 8, hdmi_dpll_filter_pvt2);
  DEF_FIELD(7, 4, hdmi_dpll_filter_pvt1);
  DEF_FIELD(1, 0, hdmi_dpll_pfd_gain);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPllCntl4Reg>(HHI_HDMI_PLL_CNTL4); }
};

// HHI_HDMI_PLL_CNTL5
class HhiHdmiPllCntl5Reg : public hwreg::RegisterBase<HhiHdmiPllCntl5Reg, uint32_t> {
 public:
  DEF_FIELD(30, 28, hdmi_dpll_adj_vco_ldo);
  DEF_FIELD(27, 24, hdmi_dpll_lm_w);
  DEF_FIELD(21, 16, hdmi_dpll_lm_s);
  DEF_FIELD(15, 0, hdmi_dpll_reve);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPllCntl5Reg>(HHI_HDMI_PLL_CNTL5); }
};

// HHI_HDMI_PLL_CNTL6
class HhiHdmiPllCntl6Reg : public hwreg::RegisterBase<HhiHdmiPllCntl6Reg, uint32_t> {
  DEF_FIELD(31, 30, hdmi_dpll_afc_hold_t);
  DEF_FIELD(29, 28, hdmi_dpll_lkw_sel);
  DEF_FIELD(27, 26, hdmi_dpll_dco_sdm_clk_sel);
  DEF_FIELD(25, 24, hdmi_dpll_afc_in);
  DEF_FIELD(23, 22, hdmi_dpll_afc_nt);

  // Bit 20 is also mentioned as vlock_cntl_en by all other HDMI PLL register
  // descriptions.
  DEF_FIELD(21, 20, hdmi_dpll_vc_in);

  DEF_FIELD(19, 18, hdmi_dpll_lock_long);
  DEF_FIELD(17, 16, hdmi_dpll_freq_shift_v);
  DEF_FIELD(14, 12, hdmi_dpll_data_sel);
  DEF_FIELD(10, 8, hdmi_dpll_sdmnc_ulms);
  DEF_FIELD(6, 0, hdmi_dpll_sdmnc_power);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPllCntl6Reg>(HHI_HDMI_PLL_CNTL6); }
};

// HHI_HDMI_PLL_STS
class HhiHdmiPllStsReg : public hwreg::RegisterBase<HhiHdmiPllStsReg, uint32_t> {
 public:
  DEF_BIT(31, hdmi_dpll_lock);
  DEF_BIT(30, hdmi_dpll_lock_a);
  DEF_BIT(29, hdmi_dpll_afc_done);
  DEF_FIELD(22, 16, hdmi_dpll_sdmnc_monitor);
  DEF_FIELD(9, 0, hdmi_dpll_out_rsv);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPllStsReg>(HHI_HDMI_PLL_STS); }
};

// HHI_HDMI_PLL_VLOCK_CNTL
class HhiHdmiPllVlockCntl : public hwreg::RegisterBase<HhiHdmiPllVlockCntl, uint32_t> {
 public:
  // 1: vlock_adj_en_to_pll
  // 0: hi_hdmi_pll_cntl3[19]
  DEF_BIT(1, vlock_adj_en_to_pll_sel);
  DEF_BIT(0, hdmi_pll_vlock_cntl_en);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPllVlockCntl>(HHI_HDMI_PLL_VLOCK_CNTL); }
};

//
// A311D datasheet Section 8.7.1.3 "HDMI Clock Tree" Figure 8-12 "HDMI Clock
// Tree"
class HhiVidPllClkDivReg : public hwreg::RegisterBase<HhiVidPllClkDivReg, uint32_t> {
 public:
  DEF_BIT(19, clk_final_en);
  DEF_BIT(18, clk_div1);
  DEF_FIELD(17, 16, clk_sel);
  DEF_BIT(15, set_preset);
  DEF_FIELD(14, 0, shift_preset);

  static auto Get() { return hwreg::RegisterAddr<HhiVidPllClkDivReg>(HHI_VID_PLL_CLK_DIV); }
};

class HhiGclkMpeg2Reg : public hwreg::RegisterBase<HhiGclkMpeg2Reg, uint32_t> {
 public:
  // Undocumented
  DEF_BIT(4, clk81_en);

  static auto Get() { return hwreg::RegisterAddr<HhiGclkMpeg2Reg>(HHI_GCLK_MPEG2); }
};

// HHI_HDMI_PHY_CNTL0
class HhiHdmiPhyCntl0Reg : public hwreg::RegisterBase<HhiHdmiPhyCntl0Reg, uint32_t> {
 public:
  DEF_FIELD(31, 16, hdmi_ctl1);
  DEF_FIELD(15, 0, hdmi_ctl2);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPhyCntl0Reg>(HHI_HDMI_PHY_CNTL0); }
};

// HHI_HDMI_PHY_CNTL1
class HhiHdmiPhyCntl1Reg : public hwreg::RegisterBase<HhiHdmiPhyCntl1Reg, uint32_t> {
 public:
  DEF_FIELD(31, 30, new_prbs_mode);
  DEF_FIELD(29, 28, new_prbs_prbsmode);
  DEF_BIT(27, new_prbs_sel);
  DEF_BIT(26, new_prbs_en);
  DEF_FIELD(25, 24, ch3_swap);  // 0: ch0, 1: ch1, 2: ch2, 3: ch3
  DEF_FIELD(23, 22, ch2_swap);  // 0: ch0, 1: ch1, 2: ch2, 3: ch3
  DEF_FIELD(21, 20, ch1_swap);  // 0: ch0, 1: ch1, 2: ch2, 3: ch3
  DEF_FIELD(19, 18, ch0_swap);  // 0: ch0, 1: ch1, 2: ch2, 3: ch3
  DEF_BIT(17, bit_invert);
  DEF_BIT(16, msb_lsb_swap);
  DEF_BIT(15, capture_add1);
  DEF_BIT(14, capture_clk_gate_en);
  DEF_BIT(13, hdmi_tx_prbs_en);
  DEF_BIT(12, hdmi_tx_prbs_err_en);
  DEF_FIELD(11, 8, hdmi_tx_sel_high);
  DEF_FIELD(7, 4, hdmi_tx_sel_low);
  DEF_BIT(3, hdmi_fifo_wr_enable);
  DEF_BIT(2, hdmi_fifo_enable);
  DEF_BIT(1, hdmi_tx_phy_clk_en);
  DEF_BIT(0, hdmi_tx_phy_soft_reset);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPhyCntl1Reg>(HHI_HDMI_PHY_CNTL1); }
};

// HHI_HDMI_PHY_CNTL2
class HhiHdmiPhyCntl2Reg : public hwreg::RegisterBase<HhiHdmiPhyCntl2Reg, uint32_t> {
 public:
  DEF_BIT(8, test_error);
  DEF_FIELD(7, 0, hdmi_regrd);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPhyCntl2Reg>(HHI_HDMI_PHY_CNTL2); }
};

// HHI_HDMI_PHY_CNTL3
class HhiHdmiPhyCntl3Reg : public hwreg::RegisterBase<HhiHdmiPhyCntl3Reg, uint32_t> {
 public:
  // All bits reserved

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPhyCntl3Reg>(HHI_HDMI_PHY_CNTL3); }
};

// HHI_HDMI_PHY_CNTL4
class HhiHdmiPhyCntl4Reg : public hwreg::RegisterBase<HhiHdmiPhyCntl4Reg, uint32_t> {
 public:
  DEF_FIELD(31, 24, new_prbs_err_thr);
  DEF_FIELD(21, 20, dtest_sel);
  DEF_BIT(19, new_prbs_clr_ber_meter);
  DEF_BIT(17, new_prbs_freez_ber);
  DEF_BIT(16, new_prbs_inverse_in);
  DEF_FIELD(15, 14, new_prbs_mode);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPhyCntl4Reg>(HHI_HDMI_PHY_CNTL4); }
};

// HHI_HDMI_PHY_CNTL5
class HhiHdmiPhyCntl5Reg : public hwreg::RegisterBase<HhiHdmiPhyCntl5Reg, uint32_t> {
 public:
  DEF_FIELD(31, 24, new_pbrs_err_thr);
  DEF_FIELD(21, 20, dtest_sel);
  DEF_BIT(19, new_prbs_clr_ber_meter);
  DEF_BIT(17, new_prbs_freez_ber);
  DEF_BIT(16, new_prbs_inverse_in);
  DEF_FIELD(15, 14, new_prbs_mode);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPhyCntl5Reg>(HHI_HDMI_PHY_CNTL5); }
};

// HHI_HDMI_PHY_STATUS
class HhiHdmiPhyStatusReg : public hwreg::RegisterBase<HhiHdmiPhyStatusReg, uint32_t> {
 public:
  DEF_BIT(29, prbs_enabled);
  DEF_BIT(28, test_error);
  DEF_BIT(24, new_prbs_pattern_nok);
  DEF_BIT(20, new_prbs_lock);
  DEF_FIELD(19, 0, new_prbs_ber_meter);

  static auto Get() { return hwreg::RegisterAddr<HhiHdmiPhyStatusReg>(HHI_HDMI_PHY_STATUS); }
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_HHI_REGS_H_
