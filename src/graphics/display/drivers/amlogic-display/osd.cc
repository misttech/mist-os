// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/osd.h"

#include <float.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>
#include <math.h>
#include <stdint.h>
#include <threads.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>
#include <zircon/utc.h>

#include <algorithm>
#include <cmath>
#include <cstdint>

#include <ddktl/device.h>
#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>

#include "lib/zx/pmt.h"
#include "lib/zx/vmar.h"
#include "src/graphics/display/drivers/amlogic-display/amlogic-display.h"
#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/hhi-regs.h"
#include "src/graphics/display/drivers/amlogic-display/rdma-regs.h"
#include "src/graphics/display/drivers/amlogic-display/vpp-regs.h"
#include "src/graphics/display/drivers/amlogic-display/vpu-regs.h"

namespace amlogic_display {

#define READ32_VPU_REG(a) vpu_mmio_->Read32(a)
#define WRITE32_VPU_REG(a, v) vpu_mmio_->Write32(v, a)

namespace {
constexpr uint32_t kMaximumAlpha = 0xff;

// We use bicubic interpolation for scaling.
// TODO(payamm): Add support for other types of interpolation
unsigned int osd_filter_coefs_bicubic[] = {
    0x00800000, 0x007f0100, 0xff7f0200, 0xfe7f0300, 0xfd7e0500, 0xfc7e0600, 0xfb7d0800,
    0xfb7c0900, 0xfa7b0b00, 0xfa7a0dff, 0xf9790fff, 0xf97711ff, 0xf87613ff, 0xf87416fe,
    0xf87218fe, 0xf8701afe, 0xf76f1dfd, 0xf76d1ffd, 0xf76b21fd, 0xf76824fd, 0xf76627fc,
    0xf76429fc, 0xf7612cfc, 0xf75f2ffb, 0xf75d31fb, 0xf75a34fb, 0xf75837fa, 0xf7553afa,
    0xf8523cfa, 0xf8503ff9, 0xf84d42f9, 0xf84a45f9, 0xf84848f8};

constexpr uint32_t kFloatToFixed3_10ScaleFactor = 1024;
constexpr int32_t kMaxFloatToFixed3_10 = (4 * kFloatToFixed3_10ScaleFactor) - 1;
constexpr int32_t kMinFloatToFixed3_10 = -4 * kFloatToFixed3_10ScaleFactor;
constexpr uint32_t kFloatToFixed3_10Mask = 0x1FFF;

constexpr uint32_t kFloatToFixed2_10ScaleFactor = 1024;
constexpr int32_t kMaxFloatToFixed2_10 = (2 * kFloatToFixed2_10ScaleFactor) - 1;
constexpr int32_t kMinFloatToFixed2_10 = -2 * kFloatToFixed2_10ScaleFactor;
constexpr uint32_t kFloatToFixed2_10Mask = 0xFFF;

// AFBC related constants
constexpr uint32_t kAfbcb16x16Pixel = 0;
[[maybe_unused]] constexpr uint32_t kAfbc32x8Pixel = 1;
constexpr uint32_t kAfbcRGBA8888 = 5;
constexpr uint32_t kAfbcColorReorderR = 1;
constexpr uint32_t kAfbcColorReorderG = 2;
constexpr uint32_t kAfbcColorReorderB = 3;
constexpr uint32_t kAfbcColorReorderA = 4;

class OsdRegisters {
 public:
  hwreg::RegisterAddr<OsdCtrlStatReg> ctrl_stat;
  hwreg::RegisterAddr<OsdCtrlStat2Reg> ctrl_stat2;        /* VIU_OSD1_CTRL_STAT2 */
  hwreg::RegisterAddr<OsdColorAddrReg> color_addr;        /* VIU_OSD1_COLOR_ADDR */
  hwreg::RegisterAddr<OsdColorReg> color;                 /* VIU_OSD1_COLOR */
  hwreg::RegisterAddr<OsdTcolorAgReg> tcolor_ag0;         /* VIU_OSD1_TCOLOR_AG0 */
  hwreg::RegisterAddr<OsdTcolorAgReg> tcolor_ag1;         /* VIU_OSD1_TCOLOR_AG1 */
  hwreg::RegisterAddr<OsdTcolorAgReg> tcolor_ag2;         /* VIU_OSD1_TCOLOR_AG2 */
  hwreg::RegisterAddr<OsdTcolorAgReg> tcolor_ag3;         /* VIU_OSD1_TCOLOR_AG3 */
  hwreg::RegisterAddr<OsdBlk0CfgW0Reg> blk0_cfg_w0;       /* VIU_OSD1_BLK0_CFG_W0 */
  hwreg::RegisterAddr<OsdBlk0CfgW1Reg> blk0_cfg_w1;       /* VIU_OSD1_BLK0_CFG_W1 */
  hwreg::RegisterAddr<OsdBlk0CfgW2Reg> blk0_cfg_w2;       /* VIU_OSD1_BLK0_CFG_W2 */
  hwreg::RegisterAddr<OsdBlk0CfgW3Reg> blk0_cfg_w3;       /* VIU_OSD1_BLK0_CFG_W3 */
  hwreg::RegisterAddr<OsdBlk0CfgW4Reg> blk0_cfg_w4;       /* VIU_OSD1_BLK0_CFG_W4 */
  hwreg::RegisterAddr<OsdBlk1CfgW4Reg> blk1_cfg_w4;       /* VIU_OSD1_BLK1_CFG_W4 */
  hwreg::RegisterAddr<OsdBlk2CfgW4Reg> blk2_cfg_w4;       /* VIU_OSD1_BLK2_CFG_W4 */
  hwreg::RegisterAddr<OsdFifoCtrlStatReg> fifo_ctrl_stat; /* VIU_OSD1_FIFO_CTRL_STAT */
  // hwreg::RegisterAddr<OsdTestRdDataReg> test_rddata;/* VIU_OSD1_TEST_RDDATA */
  hwreg::RegisterAddr<OsdProtCtrlReg> prot_ctrl;              /* VIU_OSD1_PROT_CTRL */
  hwreg::RegisterAddr<OsdMaliUnpackCtrlReg> mali_unpack_ctrl; /* VIU_OSD1_MALI_UNPACK_CTRL */
  hwreg::RegisterAddr<OsdDimmCtrlReg> dimm_ctrl;              /* VIU_OSD1_DIMM_CTRL */

  hwreg::RegisterAddr<OsdScaleCoefIdxReg> scale_coef_idx;  /* VPP_OSD_SCALE_COEF_IDX */
  hwreg::RegisterAddr<OsdScaleCoefReg> scale_coef;         /* VPP_OSD_SCALE_COEF */
  hwreg::RegisterAddr<OsdVscPhaseStepReg> vsc_phase_step;  /* VPP_OSD_VSC_PHASE_STEP */
  hwreg::RegisterAddr<OsdVscInitPhaseReg> vsc_init_phase;  /* VPP_OSD_VSC_INI_PHASE */
  hwreg::RegisterAddr<OsdVscCtrl0Reg> vsc_ctrl0;           /* VPP_OSD_VSC_CTRL0 */
  hwreg::RegisterAddr<OsdHscPhaseStepReg> hsc_phase_step;  /* VPP_OSD_HSC_PHASE_STEP */
  hwreg::RegisterAddr<OsdHscInitPhaseReg> hsc_init_phase;  /* VPP_OSD_HSC_INI_PHASE */
  hwreg::RegisterAddr<OsdHscCtrl0Reg> hsc_ctrl0;           /* VPP_OSD_HSC_CTRL0 */
  hwreg::RegisterAddr<OsdScDummyDataReg> sc_dummy_data;    /* VPP_OSD_SC_DUMMY_DATA */
  hwreg::RegisterAddr<OsdScCtrl0Reg> sc_ctrl0;             /* VPP_OSD_SC_CTRL0 */
  hwreg::RegisterAddr<OsdSciWhM1Reg> sci_wh_m1;            /* VPP_OSD_SCI_WH_M1 */
  hwreg::RegisterAddr<OsdScoHStartEndReg> sco_h_start_end; /* VPP_OSD_SCO_H_START_END */
  hwreg::RegisterAddr<OsdScoVStartEndReg> sco_v_start_end; /* VPP_OSD_SCO_V_START_END */
  hwreg::RegisterAddr<AfbcHeaderBufAddrLowS0Reg>
      afbc_header_buf_addr_low_s; /* VPU_MAFBC_HEADER_BUF_ADDR_LOW_S0 */
  hwreg::RegisterAddr<AfbcHeaderBufAddrHighS0Reg>
      afbc_header_buf_addr_high_s; /* VPU_MAFBC_HEADER_BUF_ADDR_HIGH_S0 */
  hwreg::RegisterAddr<AfbcFormatSpecifierS0Reg>
      afbc_format_specifier_s;                                   /* VPU_MAFBC_FORMAT_SPECIFIER_S0 */
  hwreg::RegisterAddr<AfbcBufferWidthS0Reg> afbc_buffer_width_s; /* VPU_MAFBC_BUFFER_WIDTH_S0 */
  hwreg::RegisterAddr<AfbcBufferHeightS0Reg> afbc_buffer_height_s; /* VPU_MAFBC_BUFFER_HEIGHT_S0 */
  hwreg::RegisterAddr<AfbcBoundingBoxXStartS0Reg>
      afbc_bounding_box_x_start_s; /* VPU_MAFBC_BOUNDING_BOX_X_START_S0 */
  hwreg::RegisterAddr<AfbcBoundingBoxXEndS0Reg>
      afbc_bounding_box_x_end_s; /* VPU_MAFBC_BOUNDING_BOX_X_END_S0 */
  hwreg::RegisterAddr<AfbcBoundingBoxYStartS0Reg>
      afbc_bounding_box_y_start_s; /* VPU_MAFBC_BOUNDING_BOX_Y_START_S0 */
  hwreg::RegisterAddr<AfbcBoundingBoxYEndS0Reg>
      afbc_bounding_box_y_end_s; /* VPU_MAFBC_BOUNDING_BOX_Y_END_S0 */
  hwreg::RegisterAddr<AfbcOutputBufAddrLowS0Reg>
      afbc_output_buf_addr_low_s; /* VPU_MAFBC_OUTPUT_BUF_ADDR_LOW_S0 */
  hwreg::RegisterAddr<AfbcOutputBufAddrHighS0Reg>
      afbc_output_buf_addr_high_s; /* VPU_MAFBC_OUTPUT_BUF_ADDR_HIGH_S0 */
  hwreg::RegisterAddr<AfbcOutputBufStrideS0Reg>
      afbc_output_buf_stride_s; /* VPU_MAFBC_OUTPUT_BUF_STRIDE_S0 */
  hwreg::RegisterAddr<AfbcPrefetchCfgS0Reg> afbc_prefetch_cfg_s; /* VPU_MAFBC_PREFETCH_CFG_S0 */
};

OsdRegisters osd1_registers = {
    OsdCtrlStatReg::Get(VPU_VIU_OSD1_CTRL_STAT),
    OsdCtrlStat2Reg::Get(VPU_VIU_OSD1_CTRL_STAT2),
    OsdColorAddrReg::Get(VPU_VIU_OSD1_COLOR_ADDR),
    OsdColorReg::Get(VPU_VIU_OSD1_COLOR),
    OsdTcolorAgReg::Get(VPU_VIU_OSD1_TCOLOR_AG0),
    OsdTcolorAgReg::Get(VPU_VIU_OSD1_TCOLOR_AG1),
    OsdTcolorAgReg::Get(VPU_VIU_OSD1_TCOLOR_AG2),
    OsdTcolorAgReg::Get(VPU_VIU_OSD1_TCOLOR_AG3),
    OsdBlk0CfgW0Reg::Get(VPU_VIU_OSD1_BLK0_CFG_W0),
    OsdBlk0CfgW1Reg::Get(VPU_VIU_OSD1_BLK0_CFG_W1),
    OsdBlk0CfgW2Reg::Get(VPU_VIU_OSD1_BLK0_CFG_W2),
    OsdBlk0CfgW3Reg::Get(VPU_VIU_OSD1_BLK0_CFG_W3),
    OsdBlk0CfgW4Reg::Get(VPU_VIU_OSD1_BLK0_CFG_W4),
    OsdBlk1CfgW4Reg::Get(VPU_VIU_OSD1_BLK1_CFG_W4),
    OsdBlk2CfgW4Reg::Get(VPU_VIU_OSD1_BLK2_CFG_W4),
    OsdFifoCtrlStatReg::Get(VPU_VIU_OSD1_FIFO_CTRL_STAT),
    // OsdTestRdDataReg::Get(VPU_VIU_OSD1_TEST_RDDATA),
    OsdProtCtrlReg::Get(VPU_VIU_OSD1_PROT_CTRL),
    OsdMaliUnpackCtrlReg::Get(VPU_VIU_OSD1_MALI_UNPACK_CTRL),
    OsdDimmCtrlReg::Get(VPU_VIU_OSD1_DIMM_CTRL),

    OsdScaleCoefIdxReg::Get(VPU_VPP_OSD_SCALE_COEF_IDX),
    OsdScaleCoefReg::Get(VPU_VPP_OSD_SCALE_COEF),
    OsdVscPhaseStepReg::Get(VPU_VPP_OSD_VSC_PHASE_STEP),
    OsdVscInitPhaseReg::Get(VPU_VPP_OSD_VSC_INI_PHASE),
    OsdVscCtrl0Reg::Get(VPU_VPP_OSD_VSC_CTRL0),
    OsdHscPhaseStepReg::Get(VPU_VPP_OSD_HSC_PHASE_STEP),
    OsdHscInitPhaseReg::Get(VPU_VPP_OSD_HSC_INI_PHASE),
    OsdHscCtrl0Reg::Get(VPU_VPP_OSD_HSC_CTRL0),
    OsdScDummyDataReg::Get(VPU_VPP_OSD_SC_DUMMY_DATA),
    OsdScCtrl0Reg::Get(VPU_VPP_OSD_SC_CTRL0),
    OsdSciWhM1Reg::Get(VPU_VPP_OSD_SCI_WH_M1),
    OsdScoHStartEndReg::Get(VPU_VPP_OSD_SCO_H_START_END),
    OsdScoVStartEndReg::Get(VPU_VPP_OSD_SCO_V_START_END),
    AfbcHeaderBufAddrLowS0Reg::Get(VPU_MAFBC_HEADER_BUF_ADDR_LOW_S0),
    AfbcHeaderBufAddrHighS0Reg::Get(VPU_MAFBC_HEADER_BUF_ADDR_HIGH_S0),
    AfbcFormatSpecifierS0Reg::Get(VPU_MAFBC_FORMAT_SPECIFIER_S0),
    AfbcBufferWidthS0Reg::Get(VPU_MAFBC_BUFFER_WIDTH_S0),
    AfbcBufferHeightS0Reg::Get(VPU_MAFBC_BUFFER_HEIGHT_S0),
    AfbcBoundingBoxXStartS0Reg::Get(VPU_MAFBC_BOUNDING_BOX_X_START_S0),
    AfbcBoundingBoxXEndS0Reg::Get(VPU_MAFBC_BOUNDING_BOX_X_END_S0),
    AfbcBoundingBoxYStartS0Reg::Get(VPU_MAFBC_BOUNDING_BOX_Y_START_S0),
    AfbcBoundingBoxYEndS0Reg::Get(VPU_MAFBC_BOUNDING_BOX_Y_END_S0),
    AfbcOutputBufAddrLowS0Reg::Get(VPU_MAFBC_OUTPUT_BUF_ADDR_LOW_S0),
    AfbcOutputBufAddrHighS0Reg::Get(VPU_MAFBC_OUTPUT_BUF_ADDR_HIGH_S0),
    AfbcOutputBufStrideS0Reg::Get(VPU_MAFBC_OUTPUT_BUF_STRIDE_S0),
    AfbcPrefetchCfgS0Reg::Get(VPU_MAFBC_PREFETCH_CFG_S0),
};

}  // namespace

config_stamp_t Osd::GetLastConfigStampApplied() { return rdma_->GetLastConfigStampApplied(); }

Osd::Osd(bool supports_afbc, uint32_t fb_width, uint32_t fb_height, uint32_t display_width,
         uint32_t display_height, inspect::Node* unused_osd_inspect_node,
         std::optional<fdf::MmioBuffer> vpu_mmio, std::unique_ptr<RdmaEngine> rdma)
    : vpu_mmio_(std::move(vpu_mmio)),
      supports_afbc_(supports_afbc),
      fb_width_(fb_width),
      fb_height_(fb_height),
      display_width_(display_width),
      display_height_(display_height),
      rdma_(std::move(rdma)) {}

void Osd::Disable(config_stamp_t config_stamp) {
  rdma_->StopRdma();
  osd1_registers.ctrl_stat.ReadFrom(&(*vpu_mmio_)).set_blk_en(0).WriteTo(&(*vpu_mmio_));
  rdma_->ResetConfigStamp(config_stamp);
}

void Osd::Enable(void) {
  osd1_registers.ctrl_stat.ReadFrom(&(*vpu_mmio_)).set_blk_en(1).WriteTo(&(*vpu_mmio_));
}

uint32_t Osd::FloatToFixed2_10(float f) {
  auto fixed_num = static_cast<int32_t>(round(f * kFloatToFixed2_10ScaleFactor));

  // Amlogic hardware accepts values [-2 2). Let's make sure the result is within this range.
  // If not, clamp it
  fixed_num = std::clamp(fixed_num, kMinFloatToFixed2_10, kMaxFloatToFixed2_10);
  return fixed_num & kFloatToFixed2_10Mask;
}

uint32_t Osd::FloatToFixed3_10(float f) {
  auto fixed_num = static_cast<int32_t>(round(f * kFloatToFixed3_10ScaleFactor));

  // Amlogic hardware accepts values [-4 4). Let's make sure the result is within this range.
  // If not, clamp it
  fixed_num = std::clamp(fixed_num, kMinFloatToFixed3_10, kMaxFloatToFixed3_10);
  return fixed_num & kFloatToFixed3_10Mask;
}

void Osd::SetColorCorrection(uint32_t rdma_table_idx, const display_config_t* config) {
  if (!config->cc_flags) {
    // Disable color conversion engine
    rdma_->SetRdmaTableValue(rdma_table_idx, IDX_MATRIX_EN_CTRL,
                             vpu_mmio_->Read32(VPU_VPP_POST_MATRIX_EN_CTRL) & ~(1 << 0));
    return;
  }

  // Set enable bit
  rdma_->SetRdmaTableValue(rdma_table_idx, IDX_MATRIX_EN_CTRL,
                           vpu_mmio_->Read32(VPU_VPP_POST_MATRIX_EN_CTRL) | (1 << 0));

  // Load PreOffset values (or 0 if none entered)
  auto offset0_1 = (config->cc_flags & COLOR_CONVERSION_PREOFFSET
                        ? (FloatToFixed2_10(config->cc_preoffsets[0]) << 16 |
                           FloatToFixed2_10(config->cc_preoffsets[1]) << 0)
                        : 0);
  rdma_->SetRdmaTableValue(rdma_table_idx, IDX_MATRIX_PRE_OFFSET0_1, offset0_1);
  auto offset2 = (config->cc_flags & COLOR_CONVERSION_PREOFFSET
                      ? (FloatToFixed2_10(config->cc_preoffsets[2]) << 0)
                      : 0);
  rdma_->SetRdmaTableValue(rdma_table_idx, IDX_MATRIX_PRE_OFFSET2, offset2);
  // TODO(b/182481217): remove when this bug is closed.
  DISP_TRACE("pre offset0_1=%u offset2=%u\n", offset0_1, offset2);

  // Load PostOffset values (or 0 if none entered)
  offset0_1 = (config->cc_flags & COLOR_CONVERSION_POSTOFFSET
                   ? (FloatToFixed2_10(config->cc_postoffsets[0]) << 16 |
                      FloatToFixed2_10(config->cc_postoffsets[1]) << 0)
                   : 0);
  offset2 = (config->cc_flags & COLOR_CONVERSION_PREOFFSET
                 ? (FloatToFixed2_10(config->cc_postoffsets[2]) << 0)
                 : 0);
  rdma_->SetRdmaTableValue(rdma_table_idx, IDX_MATRIX_OFFSET0_1, offset0_1);
  rdma_->SetRdmaTableValue(rdma_table_idx, IDX_MATRIX_OFFSET2, offset2);
  // TODO(b/182481217): remove when this bug is closed.
  DISP_TRACE("post offset0_1=%u offset2=%u\n", offset0_1, offset2);

  // clang-format off
  const float identity[3][3] = {
      {1, 0, 0,},
      {0, 1, 0,},
      {0, 0, 1,},
  };
  // clang-format on

  const auto* ccm =
      (config->cc_flags & COLOR_CONVERSION_COEFFICIENTS) ? config->cc_coefficients : identity;

  // Load up the coefficient matrix registers
  auto coef00_01 = FloatToFixed3_10(ccm[0][0]) << 16 | FloatToFixed3_10(ccm[0][1]) << 0;
  auto coef02_10 = FloatToFixed3_10(ccm[0][2]) << 16 | FloatToFixed3_10(ccm[1][0]) << 0;
  auto coef11_12 = FloatToFixed3_10(ccm[1][1]) << 16 | FloatToFixed3_10(ccm[1][2]) << 0;
  auto coef20_21 = FloatToFixed3_10(ccm[2][0]) << 16 | FloatToFixed3_10(ccm[2][1]) << 0;
  auto coef22 = FloatToFixed3_10(ccm[2][2]) << 0;
  rdma_->SetRdmaTableValue(rdma_table_idx, IDX_MATRIX_COEF00_01, coef00_01);
  rdma_->SetRdmaTableValue(rdma_table_idx, IDX_MATRIX_COEF02_10, coef02_10);
  rdma_->SetRdmaTableValue(rdma_table_idx, IDX_MATRIX_COEF11_12, coef11_12);
  rdma_->SetRdmaTableValue(rdma_table_idx, IDX_MATRIX_COEF20_21, coef20_21);
  rdma_->SetRdmaTableValue(rdma_table_idx, IDX_MATRIX_COEF22, coef22);
  // TODO(b/182481217): remove when this bug is closed.
  DISP_TRACE("color correction regs 00_01=%xu 02_12=%xu 11_12=%xu 20_21=%u 22=%xu\n", coef00_01,
             coef02_10, coef11_12, coef20_21, coef22);
}

void Osd::FlipOnVsync(uint8_t idx, const display_config_t* config,
                      const config_stamp_t* config_stamp) {
  auto info = reinterpret_cast<ImageInfo*>(config[0].layer_list[0]->cfg.primary.image.handle);
  const int next_table_idx = rdma_->GetNextAvailableRdmaTableIndex();
  if (next_table_idx < 0) {
    DISP_ERROR("No table available!");
    return;
  }

  DISP_TRACE("Table index %d used", next_table_idx);
  DISP_TRACE("AFBC %s", info->is_afbc ? "enabled" : "disabled");

  if ((config[0].mode.h_addressable != display_width_) ||
      (config[0].mode.v_addressable != display_height_)) {
    DISP_INFO("Mode change (%d x %d) to (%d x %d)", display_width_, display_height_,
              config[0].mode.h_addressable, config[0].mode.v_addressable);
    display_width_ = config[0].mode.h_addressable;
    display_height_ = config[0].mode.v_addressable;
    fb_width_ = config[0].mode.h_addressable;
    fb_height_ = config[0].mode.v_addressable;
    HwInit();
  }

  if (config->gamma_table_present) {
    if (config->apply_gamma_table) {
      // Gamma Table needs to be programmed manually. Cannot use RDMA
      SetGamma(GammaChannel::kRed, config->gamma_red_list);
      SetGamma(GammaChannel::kGreen, config->gamma_green_list);
      SetGamma(GammaChannel::kBlue, config->gamma_blue_list);
    }
    // Enable Gamma at vsync using RDMA
    rdma_->SetRdmaTableValue(next_table_idx, IDX_GAMMA_EN, 1);
    // Set flag to indicate we have enabled gamma
    osd_enabled_gamma_ = true;
  } else {
    // Only disbale gamma if we enabled it.
    if (osd_enabled_gamma_) {
      // Disable Gamma at vsync using RDMA
      rdma_->SetRdmaTableValue(next_table_idx, IDX_GAMMA_EN, 0);
    } else {
      rdma_->SetRdmaTableValue(next_table_idx, IDX_GAMMA_EN,
                               VppGammaCntlPortReg::Get().ReadFrom(&(*vpu_mmio_)).en());
    }
  }
  auto cfg_w0 = osd1_registers.blk0_cfg_w0.FromValue(0);
  if (supports_afbc_ && info->is_afbc) {
    // AFBC: Enable sourcing from mali + configure as big endian
    cfg_w0.set_mali_src_en(1).set_little_endian(0);
  } else {
    // Update CFG_W0 with correct Canvas Index
    cfg_w0.set_mali_src_en(0).set_little_endian(1).set_tbl_addr(idx);
  }
  cfg_w0.set_blk_mode(OsdBlk0CfgW0Reg::kBlockMode32Bit);

  switch (info->pixel_format.pixel_format) {
    case fuchsia_images2::PixelFormat::kR8G8B8A8:
      cfg_w0.set_color_matrix(OsdBlk0CfgW0Reg::kColorMatrixAbgr8888);
      break;
    case fuchsia_images2::PixelFormat::kBgra32:
      cfg_w0.set_color_matrix(OsdBlk0CfgW0Reg::kColorMatrixArgb8888);
      break;
    default:
      // This should never happen. The image validity is guaranteed in
      // ImportImage() / CheckConfiguration().
      ZX_ASSERT_MSG(false, "Unsupported image format %u",
                    static_cast<uint32_t>(info->pixel_format.pixel_format));
      return;
  }

  rdma_->SetRdmaTableValue(next_table_idx, IDX_BLK0_CFG_W0, cfg_w0.reg_value());

  // Configure ctrl_stat and ctrl_stat2 registers
  auto osd_ctrl_stat_val = osd1_registers.ctrl_stat.ReadFrom(&(*vpu_mmio_));
  auto osd_ctrl_stat2_val = osd1_registers.ctrl_stat2.ReadFrom(&(*vpu_mmio_));

  // enable OSD Block
  osd_ctrl_stat_val.set_blk_en(1);

  // Amlogic supports two types of alpha blending:
  // Global: This alpha value is applied to the entire plane (i.e. all pixels)
  // Per-Pixel: Each pixel will be multiplied by its corresponding alpha channel
  //
  // If alpha blending is disabled by the client or we are supporting a format that does
  // not have an alpha channel, we need to:
  // a) Set global alpha multiplier to 1 (i.e. 0xFF)
  // b) Enable "replaced_alpha" and set its value to 0xFF. This will effectively
  //    tell the hardware to replace the value found in alpha channel with the "replaced"
  //    value
  //
  // If alpha blending is enabled but alpha_layer_val is NaN:
  // - Set global alpha multiplier to 1 (i.e. 0xFF)
  // - Disable "replaced_alpha" which allows hardware to use per-pixel alpha channel.
  //
  // If alpha blending is enabled and alpha_layer_val has a value:
  // - Set global alpha multiplier to alpha_layer_val
  // - Disable "replaced_alpha" which allows hardware to use per-pixel alpha channel.

  // Load default values: Set global alpha to 1 and enable replaced_alpha.
  osd_ctrl_stat2_val.set_replaced_alpha_en(1).set_replaced_alpha(kMaximumAlpha);
  osd_ctrl_stat_val.set_global_alpha(kMaximumAlpha);

  // This is guaranteed by AmlogicDisplay::CheckConfiguration().
  ZX_DEBUG_ASSERT(config->layer_count > 0);
  ZX_DEBUG_ASSERT(config->layer_list[0]->type == LAYER_TYPE_PRIMARY);
  const primary_layer_t& primary_layer = config->layer_list[0]->cfg.primary;
  if (primary_layer.alpha_mode != ALPHA_DISABLE) {
    // If a global alpha value is provided, apply it.
    if (!isnan(primary_layer.alpha_layer_val)) {
      auto num = static_cast<uint8_t>(round(primary_layer.alpha_layer_val * kMaximumAlpha));
      osd_ctrl_stat_val.set_global_alpha(num);
    }
  }

  // Use linear address for AFBC, Canvas otherwise
  osd_ctrl_stat_val.set_osd_mem_mode((supports_afbc_ && info->is_afbc) ? 1 : 0);
  osd_ctrl_stat2_val.set_pending_status_cleanup(1);

  rdma_->SetRdmaTableValue(next_table_idx, IDX_CTRL_STAT, osd_ctrl_stat_val.reg_value());
  rdma_->SetRdmaTableValue(next_table_idx, IDX_CTRL_STAT2, osd_ctrl_stat2_val.reg_value());

  // Complain if doesn't support AFBC, but trying to display with AFBC
  ZX_DEBUG_ASSERT(!(!supports_afbc_ && info->is_afbc));
  if (supports_afbc_ && info->is_afbc) {
    // Line Stride calculation based on vendor code
    auto a = fbl::round_up(fbl::round_up(info->image_width * 4, 16u) / 16, 2u);
    auto r = osd1_registers.blk2_cfg_w4.FromValue(0).set_linear_stride(a).reg_value();
    rdma_->SetRdmaTableValue(next_table_idx, IDX_BLK2_CFG_W4, r);

    // Set AFBC's Physical address since it does not use Canvas
    rdma_->SetRdmaTableValue(next_table_idx, IDX_AFBC_HEAD_BUF_ADDR_LOW,
                             (info->paddr & 0xFFFFFFFF));
    rdma_->SetRdmaTableValue(next_table_idx, IDX_AFBC_HEAD_BUF_ADDR_HIGH, (info->paddr >> 32));

    // Set OSD to unpack Mali source
    auto upackreg = osd1_registers.mali_unpack_ctrl.ReadFrom(&(*vpu_mmio_)).set_mali_unpack_en(1);
    rdma_->SetRdmaTableValue(next_table_idx, IDX_MALI_UNPACK_CTRL, upackreg.reg_value());

    // Switch OSD to Mali Source
    auto miscctrl = OsdPathMiscCtrlReg::Get(VPU_OSD_PATH_MISC_CTRL)
                        .ReadFrom(&(*vpu_mmio_))
                        .set_osd1_mali_sel(1);
    rdma_->SetRdmaTableValue(next_table_idx, IDX_PATH_MISC_CTRL, miscctrl.reg_value());

    // S0 is our index of 0, which is programmed for OSD1
    rdma_->SetRdmaTableValue(next_table_idx, IDX_AFBC_SURFACE_CFG,
                             AfbcSurfaceCfgReg::Get(VPU_MAFBC_SURFACE_CFG)
                                 .ReadFrom(&(*vpu_mmio_))
                                 .set_cont(0)
                                 .set_s0_en(1)
                                 .reg_value());
    // set command - This uses a separate RDMA Table
    rdma_->SetAfbcRdmaTableValue(
        AfbcCommandReg::Get(VPU_MAFBC_COMMAND).FromValue(0).set_direct_swap(1).reg_value());
  } else {
    // Set OSD to unpack Normal source
    auto upackreg = osd1_registers.mali_unpack_ctrl.ReadFrom(&(*vpu_mmio_)).set_mali_unpack_en(0);
    rdma_->SetRdmaTableValue(next_table_idx, IDX_MALI_UNPACK_CTRL, upackreg.reg_value());

    // Switch OSD to DDR Source
    auto miscctrl = OsdPathMiscCtrlReg::Get(VPU_OSD_PATH_MISC_CTRL)
                        .ReadFrom(&(*vpu_mmio_))
                        .set_osd1_mali_sel(0);
    rdma_->SetRdmaTableValue(next_table_idx, IDX_PATH_MISC_CTRL, miscctrl.reg_value());

    // Disable afbc sourcing
    rdma_->SetRdmaTableValue(next_table_idx, IDX_AFBC_SURFACE_CFG,
                             AfbcSurfaceCfgReg::Get(VPU_MAFBC_SURFACE_CFG)
                                 .ReadFrom(&(*vpu_mmio_))
                                 .set_s0_en(0)
                                 .reg_value());
    // clear command - This uses a separate RDMA Table
    rdma_->SetAfbcRdmaTableValue(
        AfbcCommandReg::Get(VPU_MAFBC_COMMAND).FromValue(0).set_direct_swap(0).reg_value());
  }

  SetColorCorrection(next_table_idx, config);

  // update last element of table which will be used to indicate whether RDMA operation was
  // completed or not
  rdma_->SetRdmaTableValue(next_table_idx, IDX_RDMA_CFG_STAMP_HIGH, (config_stamp->value >> 32));
  rdma_->SetRdmaTableValue(next_table_idx, IDX_RDMA_CFG_STAMP_LOW,
                           (config_stamp->value & 0xFFFFFFFF));

  rdma_->FlushRdmaTable(next_table_idx);
  if (supports_afbc_ && info->is_afbc) {
    rdma_->FlushAfbcRdmaTable();
  }

  rdma_->ExecRdmaTable(next_table_idx, config_stamp, supports_afbc_ && info->is_afbc);
}

void Osd::DefaultSetup() {
  // osd blend ctrl
  WRITE32_REG(VPU, VIU_OSD_BLEND_CTRL,
              4 << 29 | 0 << 27 |  // blend2_premult_en
                  1 << 26 |        // blend_din0 input to blend0
                  0 << 25 |        // blend1_dout to blend2
                  0 << 24 |        // blend1_din3 input to blend1
                  1 << 20 |        // blend_din_en
                  0 << 16 |        // din_premult_en
                  1 << 0);         // din_reoder_sel = OSD1

  // vpp osd1 blend ctrl
  WRITE32_REG(VPU, OSD1_BLEND_SRC_CTRL,
              (0 & 0xf) << 0 | (0 & 0x1) << 4 | (3 & 0xf) << 8 |  // postbld_src3_sel
                  (0 & 0x1) << 16 |                               // postbld_osd1_premult
                  (1 & 0x1) << 20);
  // vpp osd2 blend ctrl
  WRITE32_REG(VPU, OSD2_BLEND_SRC_CTRL,
              (0 & 0xf) << 0 | (0 & 0x1) << 4 | (0 & 0xf) << 8 |  // postbld_src4_sel
                  (0 & 0x1) << 16 |                               // postbld_osd2_premult
                  (1 & 0x1) << 20);

  // used default dummy data
  WRITE32_REG(VPU, VIU_OSD_BLEND_DUMMY_DATA0, 0x0 << 16 | 0x0 << 8 | 0x0);
  // used default dummy alpha data
  WRITE32_REG(VPU, VIU_OSD_BLEND_DUMMY_ALPHA, 0x0 << 20 | 0x0 << 11 | 0x0);

  // osdx setting
  WRITE32_REG(VPU, VPU_VIU_OSD_BLEND_DIN0_SCOPE_H, (fb_width_ - 1) << 16);

  WRITE32_REG(VPU, VPU_VIU_OSD_BLEND_DIN0_SCOPE_V, (fb_height_ - 1) << 16);

  WRITE32_REG(VPU, VIU_OSD_BLEND_BLEND0_SIZE, fb_height_ << 16 | fb_width_);
  WRITE32_REG(VPU, VIU_OSD_BLEND_BLEND1_SIZE, fb_height_ << 16 | fb_width_);
  SET_BIT32(VPU, DOLBY_PATH_CTRL, 0x3, 2, 2);

  WRITE32_REG(VPU, VPP_OSD1_IN_SIZE, fb_height_ << 16 | fb_width_);

  // setting blend scope
  WRITE32_REG(VPU, VPP_OSD1_BLD_H_SCOPE, 0 << 16 | (fb_width_ - 1));
  WRITE32_REG(VPU, VPP_OSD1_BLD_V_SCOPE, 0 << 16 | (fb_height_ - 1));

  // Set geometry to normal mode
  uint32_t data32 = ((fb_width_ - 1) & 0xfff) << 16;
  WRITE32_REG(VPU, VPU_VIU_OSD1_BLK0_CFG_W3, data32);
  data32 = ((fb_height_ - 1) & 0xfff) << 16;
  WRITE32_REG(VPU, VPU_VIU_OSD1_BLK0_CFG_W4, data32);

  WRITE32_REG(VPU, VPU_VIU_OSD1_BLK0_CFG_W1, ((fb_width_ - 1) & 0x1fff) << 16);
  WRITE32_REG(VPU, VPU_VIU_OSD1_BLK0_CFG_W2, ((fb_height_ - 1) & 0x1fff) << 16);

  // enable osd blk0
  osd1_registers.ctrl_stat.ReadFrom(&(*vpu_mmio_))
      .set_rsv(0)
      .set_osd_mem_mode(0)
      .set_premult_en(0)
      .set_blk_en(1)
      .WriteTo(&(*vpu_mmio_));
}

void Osd::EnableScaling(bool enable) {
  int hf_phase_step, vf_phase_step;
  int src_w, src_h, dst_w, dst_h;
  int bot_ini_phase;
  int vsc_ini_rcv_num, vsc_ini_rpt_p0_num;
  int hsc_ini_rcv_num, hsc_ini_rpt_p0_num;
  int hf_bank_len = 4;
  int vf_bank_len = 0;
  uint32_t data32 = 0x0;

  vf_bank_len = 4;
  hsc_ini_rcv_num = hf_bank_len;
  vsc_ini_rcv_num = vf_bank_len;
  hsc_ini_rpt_p0_num = (hf_bank_len / 2 - 1) > 0 ? (hf_bank_len / 2 - 1) : 0;
  vsc_ini_rpt_p0_num = (vf_bank_len / 2 - 1) > 0 ? (vf_bank_len / 2 - 1) : 0;
  src_w = fb_width_;
  src_h = fb_height_;
  dst_w = display_width_;
  dst_h = display_height_;

  data32 = 0x0;
  if (enable) {
    /* enable osd scaler */
    data32 |= 1 << 2; /* enable osd scaler */
    data32 |= 1 << 3; /* enable osd scaler path */
    WRITE32_REG(VPU, VPU_VPP_OSD_SC_CTRL0, data32);
  } else {
    /* disable osd scaler path */
    WRITE32_REG(VPU, VPU_VPP_OSD_SC_CTRL0, 0);
  }
  hf_phase_step = (src_w << 18) / dst_w;
  hf_phase_step = (hf_phase_step << 6);
  vf_phase_step = (src_h << 20) / dst_h;
  bot_ini_phase = 0;
  vf_phase_step = (vf_phase_step << 4);

  /* config osd scaler in/out hv size */
  data32 = 0x0;
  if (enable) {
    data32 = (((src_h - 1) & 0x1fff) | ((src_w - 1) & 0x1fff) << 16);
    WRITE32_REG(VPU, VPU_VPP_OSD_SCI_WH_M1, data32);
    data32 = (((display_width_ - 1) & 0xfff));
    WRITE32_REG(VPU, VPU_VPP_OSD_SCO_H_START_END, data32);
    data32 = (((display_height_ - 1) & 0xfff));
    WRITE32_REG(VPU, VPU_VPP_OSD_SCO_V_START_END, data32);
  }
  data32 = 0x0;
  if (enable) {
    data32 |=
        (vf_bank_len & 0x7) | ((vsc_ini_rcv_num & 0xf) << 3) | ((vsc_ini_rpt_p0_num & 0x3) << 8);
    data32 |= 1 << 24;
  }
  WRITE32_REG(VPU, VPU_VPP_OSD_VSC_CTRL0, data32);
  data32 = 0x0;
  if (enable) {
    data32 |=
        (hf_bank_len & 0x7) | ((hsc_ini_rcv_num & 0xf) << 3) | ((hsc_ini_rpt_p0_num & 0x3) << 8);
    data32 |= 1 << 22;
  }
  WRITE32_REG(VPU, VPU_VPP_OSD_HSC_CTRL0, data32);
  data32 = 0x0;
  if (enable) {
    data32 |= (bot_ini_phase & 0xffff) << 16;
    SET_BIT32(VPU, VPU_VPP_OSD_HSC_PHASE_STEP, hf_phase_step, 0, 28);
    SET_BIT32(VPU, VPU_VPP_OSD_HSC_INI_PHASE, 0, 0, 16);
    SET_BIT32(VPU, VPU_VPP_OSD_VSC_PHASE_STEP, vf_phase_step, 0, 28);
    WRITE32_REG(VPU, VPU_VPP_OSD_VSC_INI_PHASE, data32);
  }
}

void Osd::EnableGamma() {
  VppGammaCntlPortReg::Get().ReadFrom(&(*vpu_mmio_)).set_en(1).WriteTo(&(*vpu_mmio_));
}
void Osd::DisableGamma() {
  VppGammaCntlPortReg::Get().ReadFrom(&(*vpu_mmio_)).set_en(0).WriteTo(&(*vpu_mmio_));
}

zx_status_t Osd::WaitForGammaAddressReady() {
  // The following delay and retry count is from hardware vendor
  constexpr int32_t kGammaRetry = 100;
  constexpr zx::duration kGammaDelay = zx::usec(10);
  auto retry = kGammaRetry;
  while (!(VppGammaCntlPortReg::Get().ReadFrom(&(*vpu_mmio_)).adr_rdy()) && retry--) {
    zx::nanosleep(zx::deadline_after(kGammaDelay));
  }
  if (retry <= 0) {
    return ZX_ERR_TIMED_OUT;
  }
  return ZX_OK;
}

zx_status_t Osd::WaitForGammaWriteReady() {
  // The following delay and retry count is from hardware vendor
  constexpr int32_t kGammaRetry = 100;
  constexpr zx::duration kGammaDelay = zx::usec(10);
  auto retry = kGammaRetry;
  while (!(VppGammaCntlPortReg::Get().ReadFrom(&(*vpu_mmio_)).wr_rdy()) && retry--) {
    zx::nanosleep(zx::deadline_after(kGammaDelay));
  }
  if (retry <= 0) {
    return ZX_ERR_TIMED_OUT;
  }
  return ZX_OK;
}

zx_status_t Osd::SetGamma(GammaChannel channel, const float* data) {
  // Make sure Video Encoder is enabled
  // WRITE32_REG(VPU, ENCL_VIDEO_EN, 0);
  if (!(vpu_mmio_->Read32(ENCL_VIDEO_EN) & 0x1)) {
    return ZX_ERR_UNAVAILABLE;
  }

  // Wait for ADDR port to be ready
  zx_status_t status;
  if ((status = WaitForGammaAddressReady()) != ZX_OK) {
    return status;
  }

  // Select channel and enable auto-increment.
  // auto-increment: increments the gamma table address as we write into the
  // data register
  auto gamma_addrport_reg = VppGammaAddrPortReg::Get().FromValue(0);
  gamma_addrport_reg.set_auto_inc(1);
  gamma_addrport_reg.set_adr(0);
  switch (channel) {
    case GammaChannel::kRed:
      gamma_addrport_reg.set_sel_r(1);
      break;
    case GammaChannel::kGreen:
      gamma_addrport_reg.set_sel_g(1);
      break;
    case GammaChannel::kBlue:
      gamma_addrport_reg.set_sel_b(1);
      break;
    default:
      return ZX_ERR_INVALID_ARGS;
  }
  gamma_addrport_reg.WriteTo(&(*vpu_mmio_));

  // Write Gamma Table
  for (size_t i = 0; i < kGammaTableSize; i++) {
    // Only write if ready. The delay seems very excessive but this comes from vendor.
    status = WaitForGammaWriteReady();
    if (status != ZX_OK) {
      return status;
    }
    auto val = std::clamp(static_cast<uint16_t>(std::round(data[i] * 1023.0)),
                          static_cast<uint16_t>(0), static_cast<uint16_t>(1023));
    VppGammaDataPortReg::Get().FromValue(0).set_reg_value(val).WriteTo(&(*vpu_mmio_));
  }

  // Wait for ADDR port to be ready
  if ((status = WaitForGammaAddressReady()) != ZX_OK) {
    return status;
  }
  return ZX_OK;
}

void Osd::SetMinimumRgb(uint8_t minimum_rgb) {
  // According to spec, minimum rgb should be set as follows:
  // Shift value by 2bits (8bit -> 10bit) and write new value for
  // each channel separately.
  VppClipMisc1Reg::Get()
      .FromValue(0)
      .set_r_clamp(minimum_rgb << 2)
      .set_g_clamp(minimum_rgb << 2)
      .set_b_clamp(minimum_rgb << 2)
      .WriteTo(&(*vpu_mmio_));
}

// These configuration could be done during initialization.
zx_status_t Osd::ConfigAfbc() {
  // The format specifier must match the sysmem format modifier flags specified
  // in AmlogicDisplay::DisplayControllerImplSetBufferCollectionConstraints().
  //
  // Note RGBA8888 works for both RGBA and BGRA formats. The color channels can
  // be reordered by setting MALI_UNPACK_CTRL register.
  osd1_registers.afbc_format_specifier_s.FromValue(0)
      .set_block_split_mode_enabled(true)
      .set_tiled_header_enabled(false)
      .set_yuv_transform_enabled(true)
      .set_super_block_aspect(kAfbcb16x16Pixel)
      .set_pixel_format(kAfbcRGBA8888)
      .WriteTo(&(*vpu_mmio_));

  // Setup color RGBA channel order
  osd1_registers.mali_unpack_ctrl.ReadFrom(&(*vpu_mmio_))
      .set_r(kAfbcColorReorderR)
      .set_g(kAfbcColorReorderG)
      .set_b(kAfbcColorReorderB)
      .set_a(kAfbcColorReorderA)
      .WriteTo(&(*vpu_mmio_));

  // Set afbc input buffer width/height in pixel
  osd1_registers.afbc_buffer_width_s.FromValue(0).set_buffer_width(fb_width_).WriteTo(
      &(*vpu_mmio_));
  osd1_registers.afbc_buffer_height_s.FromValue(0)
      .set_buffer_height(fb_height_)
      .WriteTo(&(*vpu_mmio_));

  // Set afbc input buffer
  osd1_registers.afbc_bounding_box_x_start_s.FromValue(0).set_buffer_x_start(0).WriteTo(
      &(*vpu_mmio_));
  osd1_registers.afbc_bounding_box_x_end_s.FromValue(0)
      .set_buffer_x_end(fb_width_ - 1)  // vendor code has width - 1 - 1, which is technically
                                        // incorrect and gives the same result as this.
      .WriteTo(&(*vpu_mmio_));
  osd1_registers.afbc_bounding_box_y_start_s.FromValue(0).set_buffer_y_start(0).WriteTo(
      &(*vpu_mmio_));
  osd1_registers.afbc_bounding_box_y_end_s.FromValue(0)
      .set_buffer_y_end(fb_height_ -
                        1)  // vendor code has height -1 -1, but that cuts off the bottom row.
      .WriteTo(&(*vpu_mmio_));

  // Set output buffer stride
  osd1_registers.afbc_output_buf_stride_s.FromValue(0)
      .set_output_buffer_stride(fb_width_ * 4)
      .WriteTo(&(*vpu_mmio_));

  // Set afbc output buffer index
  // The way this is calculated based on vendor code is as follows:
  // Take OSD being used (1-based index): Therefore OSD1 -> index 1
  // out_addr = index << 24
  osd1_registers.afbc_output_buf_addr_low_s.FromValue(0).set_output_buffer_addr(1 << 24).WriteTo(
      &(*vpu_mmio_));
  osd1_registers.afbc_output_buf_addr_high_s.FromValue(0).set_output_buffer_addr(0).WriteTo(
      &(*vpu_mmio_));

  // Set linear address to the out_addr mentioned above
  osd1_registers.blk1_cfg_w4.FromValue(0).set_frame_addr(1 << 24).WriteTo(&(*vpu_mmio_));

  return ZX_OK;
}

void Osd::HwInit() {
  // Setup VPP horizontal width
  WRITE32_REG(VPU, VPP_POSTBLEND_H_SIZE, display_width_);

  // init vpu fifo control register
  uint32_t regVal = READ32_REG(VPU, VPP_OFIFO_SIZE);
  regVal = 0xfff << 20;
  regVal |= (0xfff + 1);
  WRITE32_REG(VPU, VPP_OFIFO_SIZE, regVal);

  // init osd fifo control and set DDR request priority to be urgent
  regVal = 1;
  regVal |= 4 << 5;   // hold_fifo_lines
  regVal |= 1 << 10;  // burst_len_sel 3 = 64. This bit is split between 10 and 31
  regVal |= 2 << 22;
  regVal |= 2 << 24;
  regVal |= 1 << 31;
  regVal |= 32 << 12;  // fifo_depth_val: 32*8 = 256
  WRITE32_REG(VPU, VPU_VIU_OSD1_FIFO_CTRL_STAT, regVal);
  WRITE32_REG(VPU, VPU_VIU_OSD2_FIFO_CTRL_STAT, regVal);

  SET_MASK32(VPU, VPP_MISC, VPP_POSTBLEND_EN);
  CLEAR_MASK32(VPU, VPP_MISC, VPP_PREBLEND_EN);

  osd1_registers.ctrl_stat.FromValue(0)
      .set_blk_en(1)
      .set_global_alpha(kMaximumAlpha)
      .set_osd_en(1)
      .WriteTo(&(*vpu_mmio_));

  // TODO: split this method into HwInit for each OSD.
  Osd2CtrlStatReg::Get()
      .FromValue(0)
      .set_blk_en(1)
      .set_global_alpha(kMaximumAlpha)
      .set_osd_en(1)
      .WriteTo(&(*vpu_mmio_));

  DefaultSetup();

  EnableScaling(false);

  // Apply scale coefficients
  osd1_registers.scale_coef_idx.ReadFrom(&(*vpu_mmio_))
      .set_hi_res_coef(0)
      .set_h_coef(0)
      .set_index(0)
      .WriteTo(&(*vpu_mmio_));
  for (unsigned int i : osd_filter_coefs_bicubic) {
    osd1_registers.scale_coef.FromValue(i).WriteTo(&(*vpu_mmio_));
  }

  osd1_registers.scale_coef_idx.ReadFrom(&(*vpu_mmio_))
      .set_hi_res_coef(0)
      .set_h_coef(1)
      .set_index(0)
      .WriteTo(&(*vpu_mmio_));
  for (unsigned int i : osd_filter_coefs_bicubic) {
    osd1_registers.scale_coef.FromValue(i).WriteTo(&(*vpu_mmio_));
  }

  // update blending
  WRITE32_REG(VPU, VPP_OSD1_BLD_H_SCOPE, 0 << 16 | (display_width_ - 1));
  WRITE32_REG(VPU, VPP_OSD1_BLD_V_SCOPE, 0 << 16 | (display_height_ - 1));
  WRITE32_REG(VPU, VPU_VPP_OUT_H_V_SIZE, display_width_ << 16 | display_height_);

  if (supports_afbc_) {
    // Configure AFBC Engine's one-time programmable fields, so it's ready
    ConfigAfbc();
  }
}

#define REG_OFFSET (0x20 << 2)
void Osd::Dump() {
  DumpNonRdmaRegisters();
  rdma_->DumpRdmaRegisters();
}

#define LOG_REG(reg) DISP_INFO("reg[0x%x]: 0x%08x " #reg, (reg), READ32_REG(VPU, (reg)))
#define LOG_REG_INSTANCE(reg, offset, index) \
  DISP_INFO("reg[0x%x]: 0x%08x " #reg " #%d", (reg) + (offset), READ32_REG(VPU, (reg)), index + 1)
void Osd::DumpNonRdmaRegisters() {
  uint32_t offset = 0;
  uint32_t index = 0;

  LOG_REG(VPU_VIU_VENC_MUX_CTRL);
  LOG_REG(VPU_VPP_MISC);
  LOG_REG(VPU_VPP_OFIFO_SIZE);
  LOG_REG(VPU_VPP_HOLD_LINES);

  LOG_REG(VPU_OSD_PATH_MISC_CTRL);
  LOG_REG(VPU_VIU_OSD_BLEND_CTRL);
  LOG_REG(VPU_VIU_OSD_BLEND_DIN0_SCOPE_H);
  LOG_REG(VPU_VIU_OSD_BLEND_DIN0_SCOPE_V);
  LOG_REG(VPU_VIU_OSD_BLEND_DIN1_SCOPE_H);
  LOG_REG(VPU_VIU_OSD_BLEND_DIN1_SCOPE_V);
  LOG_REG(VPU_VIU_OSD_BLEND_DIN2_SCOPE_H);
  LOG_REG(VPU_VIU_OSD_BLEND_DIN2_SCOPE_V);
  LOG_REG(VPU_VIU_OSD_BLEND_DIN3_SCOPE_H);
  LOG_REG(VPU_VIU_OSD_BLEND_DIN3_SCOPE_V);
  LOG_REG(VPU_VIU_OSD_BLEND_DUMMY_DATA0);
  LOG_REG(VPU_VIU_OSD_BLEND_DUMMY_ALPHA);
  LOG_REG(VPU_VIU_OSD_BLEND_BLEND0_SIZE);
  LOG_REG(VPU_VIU_OSD_BLEND_BLEND1_SIZE);

  LOG_REG(VPU_VPP_OSD1_IN_SIZE);
  LOG_REG(VPU_VPP_OSD1_BLD_H_SCOPE);
  LOG_REG(VPU_VPP_OSD1_BLD_V_SCOPE);
  LOG_REG(VPU_VPP_OSD2_BLD_H_SCOPE);
  LOG_REG(VPU_VPP_OSD2_BLD_V_SCOPE);
  LOG_REG(OSD1_BLEND_SRC_CTRL);
  LOG_REG(OSD2_BLEND_SRC_CTRL);
  LOG_REG(VPU_VPP_POSTBLEND_H_SIZE);
  LOG_REG(VPU_VPP_OUT_H_V_SIZE);

  LOG_REG(VPU_VPP_OSD_SC_CTRL0);
  LOG_REG(VPU_VPP_OSD_SCI_WH_M1);
  LOG_REG(VPU_VPP_OSD_SCO_H_START_END);
  LOG_REG(VPU_VPP_OSD_SCO_V_START_END);
  LOG_REG(VPU_VPP_POSTBLEND_H_SIZE);
  for (index = 0; index < 2; index++) {
    if (index == 1)
      offset = REG_OFFSET;
    LOG_REG_INSTANCE(VPU_VIU_OSD1_FIFO_CTRL_STAT, offset, index);
    LOG_REG_INSTANCE(VPU_VIU_OSD1_CTRL_STAT, offset, index);
    LOG_REG_INSTANCE(VPU_VIU_OSD1_CTRL_STAT2, offset, index);
    LOG_REG_INSTANCE(VPU_VIU_OSD1_BLK0_CFG_W0, offset, index);
    LOG_REG_INSTANCE(VPU_VIU_OSD1_BLK0_CFG_W1, offset, index);
    LOG_REG_INSTANCE(VPU_VIU_OSD1_BLK0_CFG_W2, offset, index);
    LOG_REG_INSTANCE(VPU_VIU_OSD1_BLK0_CFG_W3, offset, index);
    if (index == 1) {
      LOG_REG(VPU_VIU_OSD2_BLK0_CFG_W4);
    } else {
      LOG_REG(VPU_VIU_OSD1_BLK0_CFG_W4);
    }
  }

  DISP_INFO("Dumping all Color Correction Matrix related Registers\n");
  DISP_INFO("VPU_VPP_POST_MATRIX_COEF00_01 = 0x%x",
            vpu_mmio_->Read32(VPU_VPP_POST_MATRIX_COEF00_01));
  DISP_INFO("VPU_VPP_POST_MATRIX_COEF02_10 = 0x%x",
            vpu_mmio_->Read32(VPU_VPP_POST_MATRIX_COEF02_10));
  DISP_INFO("VPU_VPP_POST_MATRIX_COEF11_12 = 0x%x",
            vpu_mmio_->Read32(VPU_VPP_POST_MATRIX_COEF11_12));
  DISP_INFO("VPU_VPP_POST_MATRIX_COEF20_21 = 0x%x",
            vpu_mmio_->Read32(VPU_VPP_POST_MATRIX_COEF20_21));
  DISP_INFO("VPU_VPP_POST_MATRIX_COEF22 = 0x%x", vpu_mmio_->Read32(VPU_VPP_POST_MATRIX_COEF22));
  DISP_INFO("VPU_VPP_POST_MATRIX_OFFSET0_1 = 0x%x",
            vpu_mmio_->Read32(VPU_VPP_POST_MATRIX_OFFSET0_1));
  DISP_INFO("VPU_VPP_POST_MATRIX_OFFSET2 = 0x%x", vpu_mmio_->Read32(VPU_VPP_POST_MATRIX_OFFSET2));
  DISP_INFO("VPU_VPP_POST_MATRIX_PRE_OFFSET0_1 = 0x%x",
            vpu_mmio_->Read32(VPU_VPP_POST_MATRIX_PRE_OFFSET0_1));
  DISP_INFO("VPU_VPP_POST_MATRIX_PRE_OFFSET2 = 0x%x",
            vpu_mmio_->Read32(VPU_VPP_POST_MATRIX_PRE_OFFSET2));
  DISP_INFO("VPU_VPP_POST_MATRIX_EN_CTRL = 0x%x", vpu_mmio_->Read32(VPU_VPP_POST_MATRIX_EN_CTRL));
}

void Osd::Release() {
  Disable();
  rdma_->Release();
  thrd_join(rdma_irq_thread_, nullptr);
}

// static
zx::result<std::unique_ptr<Osd>> Osd::Create(ddk::PDevFidl* pdev, bool supports_afbc,
                                             uint32_t fb_width, uint32_t fb_height,
                                             uint32_t display_width, uint32_t display_height,
                                             inspect::Node* osd_node) {
  std::optional<fdf::MmioBuffer> vpu_mmio;
  // Map vpu mmio used by the OSD object
  zx_status_t status = pdev->MapMmio(MMIO_VPU, &vpu_mmio);
  if (status != ZX_OK) {
    DISP_ERROR("osd: Could not map VPU mmio");
    return zx::error(status);
  }

  auto rdma_or_status = RdmaEngine::Create(pdev, osd_node);
  if (rdma_or_status.is_error()) {
    return zx::error(rdma_or_status.status_value());
  }

  fbl::AllocChecker ac;
  std::unique_ptr<Osd> self(new (&ac) Osd(supports_afbc, fb_width, fb_height, display_width,
                                          display_height, osd_node, std::move(vpu_mmio),
                                          std::move(rdma_or_status.value())));
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  status = self->rdma_->SetupRdma(&(*self->vpu_mmio_));
  if (status != ZX_OK) {
    DISP_ERROR("Could not setup RDMA");
    return zx::error(status);
  }

  // TODO(rlb): consider moving thread creation into RdmaEngine with appropriate
  // facilities for controlling the thread from test code.
  auto start_thread = [](void* arg) { return static_cast<RdmaEngine*>(arg)->RdmaIrqThread(); };
  status = thrd_create_with_name(&self->rdma_irq_thread_, start_thread, self->rdma_.get(),
                                 "rdma_irq_thread");
  if (status != ZX_OK) {
    DISP_ERROR("Could not create rdma_thread");
    return zx::error(status);
  }

  return zx::ok(self.release());
}

}  // namespace amlogic_display
