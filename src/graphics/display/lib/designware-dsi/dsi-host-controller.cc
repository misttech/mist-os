// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/designware-dsi/dsi-host-controller.h"

#include <fuchsia/hardware/dsiimpl/c/banjo.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/mipi-dsi/mipi-dsi.h>
#include <lib/mmio/mmio-buffer.h>
#include <zircon/assert.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <fbl/auto_lock.h>
#include <fbl/string_buffer.h>

#include "src/graphics/display/lib/designware-dsi/dw-mipi-dsi-reg.h"

// Header Creation Macros
#define GEN_HDR_WC_MSB(x) ((x & 0xFF) << 16)
#define GEN_HDR_WC_LSB(x) ((x & 0xFF) << 8)
#define GEN_HDR_VC(x) ((x & 0x03) << 6)
#define GEN_HDR_DT(x) ((x & 0x3F) << 0)

namespace designware_dsi {

namespace {
constexpr uint32_t kPowerReset = 0;
constexpr uint32_t kPowerOn = 1;
constexpr uint32_t kPhyTestCtrlSet = 0x2;
constexpr uint32_t kPhyTestCtrlClr = 0x0;

constexpr uint32_t kDPhyTimeout = 200000;
constexpr uint32_t kPhyDelay = 6;
constexpr uint32_t kPhyStopWaitTime = 0x28;  // value from vendor

// Generic retry value used for BTA and FIFO related events
constexpr uint32_t kRetryMax = 20000;

constexpr uint32_t kMaxPldFifoDepth = 200;

constexpr uint32_t kBitPldRFull = 5;
constexpr uint32_t kBitPldREmpty = 4;
constexpr uint32_t kBitPldWFull = 3;
constexpr uint32_t kBitPldWEmpty = 2;
constexpr uint32_t kBitCmdFull = 1;
constexpr uint32_t kBitCmdEmpty = 0;

// Computes the amount of time it takes to transmit a number of pixels across
// the DPI interface between the display engine frontend and the DesignWare DSI
// host controller.
//
// The result is expressed in D-PHY lane byte clock cycles, which is the
// number of bytes that can be transmitted on a D-PHY data lane in High Speed
// mode.
//
// `pixels` must be >= 0 and <= 2^23 - 1.
//
// The DPI pixel rate (Hz) must be divisble by the D-PHY data lane byte
// transmission rate (bytes per second), and the quotient must be >= 1 and
// <= 256.
constexpr int32_t DpiPixelToDphyLaneByteClockCycle(int32_t pixels, int64_t dpi_pixel_rate_hz,
                                                   int64_t dphy_data_lane_bytes_per_second) {
  ZX_DEBUG_ASSERT(pixels >= 0);
  ZX_DEBUG_ASSERT(pixels <= (1 << 23) - 1);

  ZX_DEBUG_ASSERT(dphy_data_lane_bytes_per_second % dpi_pixel_rate_hz == 0);
  int64_t dphy_data_lane_byte_rate_to_dpi_pixel_rate_ratio_i64 =
      dphy_data_lane_bytes_per_second / dpi_pixel_rate_hz;
  ZX_DEBUG_ASSERT(dphy_data_lane_byte_rate_to_dpi_pixel_rate_ratio_i64 >= 1);
  ZX_DEBUG_ASSERT(dphy_data_lane_byte_rate_to_dpi_pixel_rate_ratio_i64 <= 256);

  int32_t dphy_data_lane_byte_rate_to_dpi_pixel_rate_ratio =
      static_cast<int32_t>(dphy_data_lane_byte_rate_to_dpi_pixel_rate_ratio_i64);
  return pixels * dphy_data_lane_byte_rate_to_dpi_pixel_rate_ratio;
}

void LogBytes(cpp20::span<const uint8_t> bytes) {
  if (bytes.empty()) {
    return;
  }

  static constexpr size_t kByteCountPerPrint = 16;
  // Stores up to `kByteCountPerPrint` bytes in "0x01,0x02,..." format
  static constexpr size_t kStringBufferSize = kByteCountPerPrint * 5 + 1;
  fbl::StringBuffer<kStringBufferSize> string_buffer;
  for (size_t i = 0; i < bytes.size(); i++) {
    if (i % kByteCountPerPrint == 0 && i != 0) {
      FDF_LOG(INFO, "%s", string_buffer.c_str());
      string_buffer.Clear();
    }
    string_buffer.AppendPrintf("0x%02x,", bytes[i]);
  }
  if (!string_buffer.empty()) {
    FDF_LOG(INFO, "%s", string_buffer.c_str());
  }
}

}  // namespace

DsiHostController::DsiHostController(fdf::MmioBuffer dsi_mmio) : dsi_mmio_(std::move(dsi_mmio)) {}

zx_status_t DsiHostController::GetColorCode(color_code_t c, bool& packed, uint8_t& code) {
  zx_status_t status = ZX_OK;
  switch (c) {
    case COLOR_CODE_PACKED_16BIT_565:
      packed = true;
      code = 0;
      break;
    case COLOR_CODE_PACKED_18BIT_666:
      packed = true;
      code = 3;
      // Converts the duration of transmitting `pixels` on the DPI interface to
      // lane byte clock cycles (number of lane byte clock periods),
      break;
    case COLOR_CODE_LOOSE_24BIT_666:
      packed = false;
      code = 3;
      break;
    case COLOR_CODE_PACKED_24BIT_888:
      packed = true;
      code = 5;
      break;
    default:
      status = ZX_ERR_INVALID_ARGS;
      break;
  }
  return status;
}

zx_status_t DsiHostController::GetVideoMode(video_mode_t v, uint8_t& mode) {
  zx_status_t status = ZX_OK;
  switch (v) {
    case VIDEO_MODE_NON_BURST_PULSE:
      mode = 0;
      break;
    case VIDEO_MODE_NON_BURST_EVENT:
      mode = 1;
      break;
    case VIDEO_MODE_BURST:
      mode = 2;
      break;
    default:
      status = ZX_ERR_INVALID_ARGS;
  }
  return status;
}

void DsiHostController::PowerUp() {
  DsiDwPwrUpReg::Get().ReadFrom(&dsi_mmio_).set_shutdown(kPowerOn).WriteTo(&dsi_mmio_);
}

void DsiHostController::PowerDown() {
  DsiDwPwrUpReg::Get().ReadFrom(&dsi_mmio_).set_shutdown(kPowerReset).WriteTo(&dsi_mmio_);
}

void DsiHostController::PhySendCode(uint32_t code, uint32_t parameter) {
  // Write code
  DsiDwPhyTstCtrl1Reg::Get().FromValue(0).set_reg_value(code).WriteTo(&dsi_mmio_);

  // Toggle PhyTestClk
  DsiDwPhyTstCtrl0Reg::Get().FromValue(0).set_reg_value(kPhyTestCtrlSet).WriteTo(&dsi_mmio_);
  DsiDwPhyTstCtrl0Reg::Get().FromValue(0).set_reg_value(kPhyTestCtrlClr).WriteTo(&dsi_mmio_);

  // Write parameter
  DsiDwPhyTstCtrl1Reg::Get().FromValue(0).set_reg_value(parameter).WriteTo(&dsi_mmio_);

  // Toggle PhyTestClk
  DsiDwPhyTstCtrl0Reg::Get().FromValue(0).set_reg_value(kPhyTestCtrlSet).WriteTo(&dsi_mmio_);
  DsiDwPhyTstCtrl0Reg::Get().FromValue(0).set_reg_value(kPhyTestCtrlClr).WriteTo(&dsi_mmio_);
}

void DsiHostController::PhyPowerUp() {
  DsiDwPhyRstzReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_phy_forcepll(1)
      .set_phy_enableclk(1)
      .set_phy_rstz(1)
      .set_phy_shutdownz(1)
      .WriteTo(&dsi_mmio_);
}

void DsiHostController::PhyPowerDown() {
  DsiDwPhyRstzReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_phy_rstz(0)
      .set_phy_shutdownz(0)
      .WriteTo(&dsi_mmio_);
}

zx_status_t DsiHostController::PhyWaitForReady() {
  int timeout = kDPhyTimeout;
  while ((DsiDwPhyStatusReg::Get().ReadFrom(&dsi_mmio_).phy_lock() == 0) && timeout--) {
    zx_nanosleep(zx_deadline_after(ZX_USEC(kPhyDelay)));
  }
  if (timeout <= 0) {
    FDF_LOG(ERROR, "Timed out waiting for D-PHY lock");
    return ZX_ERR_TIMED_OUT;
  }

  timeout = kDPhyTimeout;
  while ((DsiDwPhyStatusReg::Get().ReadFrom(&dsi_mmio_).phy_stopstateclklane() == 0) && timeout--) {
    zx_nanosleep(zx_deadline_after(ZX_USEC(kPhyDelay)));
  }
  if (timeout <= 0) {
    FDF_LOG(ERROR, "Timed out waiting for D-PHY StopStateClk to be set");
    return ZX_ERR_TIMED_OUT;
  }
  return ZX_OK;
}

zx::result<> DsiHostController::IssueCommands(
    cpp20::span<const mipi_dsi::DsiCommandAndResponse> commands) {
  for (const mipi_dsi::DsiCommandAndResponse& command : commands) {
    zx_status_t status = IssueCommand(command);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to issue a command: %s", zx_status_get_string(status));
      return zx::error(status);
    }
  }
  return zx::ok();
}

void DsiHostController::SetMode(dsi_mode_t mode) {
  // Configure the operation mode (cmd or vid)
  DsiDwModeCfgReg::Get().ReadFrom(&dsi_mmio_).set_cmd_video_mode(mode).WriteTo(&dsi_mmio_);
}

zx_status_t DsiHostController::Config(const dsi_config_t* dsi_config,
                                      int64_t dphy_data_lane_bits_per_second) {
  const display_setting_t disp_setting = dsi_config->display_setting;
  const designware_config_t dw_cfg =
      *(reinterpret_cast<designware_config_t*>(dsi_config->vendor_config_buffer));

  static constexpr int kBitsPerByte = 8;
  const int64_t dphy_data_lane_bytes_per_second = dphy_data_lane_bits_per_second / kBitsPerByte;

  bool packed;
  uint8_t code;
  uint8_t video_mode;
  zx_status_t status = GetColorCode(dsi_config->color_coding, packed, code);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Refusing config with invalid or unsupported color coding");
    return status;
  }

  status = GetVideoMode(dsi_config->video_mode_type, video_mode);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Refusing config with invalid or unsupported video mode");
    return status;
  }

  // Enable LP transmission in CMD Mode
  DsiDwCmdModeCfgReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_max_rd_pkt_size(1)
      .set_dcs_lw_tx(1)
      .set_dcs_sr_0p_tx(1)
      .set_dcs_sw_1p_tx(1)
      .set_dcs_sw_0p_tx(1)
      .set_gen_lw_tx(1)
      .set_gen_sr_2p_tx(1)
      .set_gen_sr_1p_tx(1)
      .set_gen_sr_0p_tx(1)
      .set_gen_sw_2p_tx(1)
      .set_gen_sw_1p_tx(1)
      .set_gen_sw_0p_tx(1)
      .WriteTo(&dsi_mmio_);

  // Packet header settings - Enable CRC and ECC. BTA will be enabled based on CMD
  DsiDwPckhdlCfgReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_crc_rx_en(1)
      .set_ecc_rx_en(1)
      .WriteTo(&dsi_mmio_);

  // DesignWare DSI Host Setup based on MIPI DSI Host Controller User Guide (Sec 3.1.1)

  // 1. Global configuration: Lane number and PHY stop wait time
  DsiDwPhyIfCfgReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_phy_stop_wait_time(kPhyStopWaitTime)
      .set_n_lanes(disp_setting.lane_num - 1)
      .WriteTo(&dsi_mmio_);

  // 2.1 Configure virtual channel
  DsiDwDpiVcidReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_dpi_vcid(kMipiDsiVirtualChanId)
      .WriteTo(&dsi_mmio_);

  // 2.2, Configure Color format
  DsiDwDpiColorCodingReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_loosely18_en(!packed)
      .set_dpi_color_coding(code)
      .WriteTo(&dsi_mmio_);
  // 2.3 Configure Signal polarity - Keep as default
  DsiDwDpiCfgPolReg::Get().FromValue(0).set_reg_value(0).WriteTo(&dsi_mmio_);

  // The following values are relevent for video mode
  // 3.1 Configure low power transitions and video mode type

  // Note: If the lp_cmd_en bit of the VID_MODE_CFG register is 0, the commands are sent
  // in high-speed in Video Mode. In this case, the DWC_mipi_dsi_host automatically determines
  // the area where each command can be sent and no programming or calculation is required.

  DsiDwVidModeCfgReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_vpg_en(0)
      .set_lp_cmd_en(0)
      .set_frame_bta_ack_en(1)
      .set_lp_hfp_en(1)
      .set_lp_hbp_en(1)
      .set_lp_vact_en(1)
      .set_lp_vfp_en(1)
      .set_lp_vbp_en(1)
      .set_lp_vsa_en(1)
      .set_vid_mode_type(video_mode)
      .WriteTo(&dsi_mmio_);

  // Define the max pkt size during Low Power mode
  DsiDwDpiLpCmdTimReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_outvact_lpcmd_time(dw_cfg.lp_cmd_pkt_size)
      .set_invact_lpcmd_time(dw_cfg.lp_cmd_pkt_size)
      .WriteTo(&dsi_mmio_);

  // 3.2   Configure video packet size settings
  DsiDwVidPktSizeReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_vid_pkt_size(disp_setting.h_active)
      .WriteTo(&dsi_mmio_);

  // Disable sending vid in chunk since they are ignored by DW host IP in burst mode
  DsiDwVidNumChunksReg::Get().FromValue(0).set_reg_value(0).WriteTo(&dsi_mmio_);
  DsiDwVidNullSizeReg::Get().FromValue(0).set_reg_value(0).WriteTo(&dsi_mmio_);

  // 4 Configure the video relative parameters according to the output type
  const int64_t pixel_clock_frequency_hz = disp_setting.lcd_clock;

  const int32_t horizontal_sync_width_pixels = static_cast<int32_t>(disp_setting.hsync_width);
  const int32_t horizontal_sync_width_duration_lane_byte_clock_cycles =
      DpiPixelToDphyLaneByteClockCycle(horizontal_sync_width_pixels, pixel_clock_frequency_hz,
                                       dphy_data_lane_bytes_per_second);
  DsiDwVidHsaTimeReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_vid_hsa_time(horizontal_sync_width_duration_lane_byte_clock_cycles)
      .WriteTo(&dsi_mmio_);

  const int32_t horizontal_back_porch_pixels = static_cast<int32_t>(disp_setting.hsync_bp);
  const int32_t horizontal_back_porch_duration_lane_byte_clock_cycles =
      DpiPixelToDphyLaneByteClockCycle(horizontal_back_porch_pixels, pixel_clock_frequency_hz,
                                       dphy_data_lane_bytes_per_second);
  DsiDwVidHbpTimeReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_vid_hbp_time(horizontal_back_porch_duration_lane_byte_clock_cycles)
      .WriteTo(&dsi_mmio_);

  const int32_t horizontal_total_pixels = static_cast<int32_t>(disp_setting.h_period);
  const int32_t horizontal_total_duration_lane_byte_clock_cycles = DpiPixelToDphyLaneByteClockCycle(
      horizontal_total_pixels, pixel_clock_frequency_hz, dphy_data_lane_bytes_per_second);
  DsiDwVidHlineTimeReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_vid_hline_time(horizontal_total_duration_lane_byte_clock_cycles)
      .WriteTo(&dsi_mmio_);

  DsiDwVidVsaLinesReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_vsa_lines(disp_setting.vsync_width)
      .WriteTo(&dsi_mmio_);

  DsiDwVidVbpLinesReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_vbp_lines(disp_setting.vsync_bp)
      .WriteTo(&dsi_mmio_);

  DsiDwVidVactiveLinesReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_vactive_lines(disp_setting.v_active)
      .WriteTo(&dsi_mmio_);

  DsiDwVidVfpLinesReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_vfp_lines(disp_setting.v_period - disp_setting.v_active - disp_setting.vsync_bp -
                     disp_setting.vsync_width)
      .WriteTo(&dsi_mmio_);

  // Internal dividers to divide lanebyteclk for timeout purposes
  DsiDwClkmgrCfgReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_to_clk_div(1)
      .set_tx_esc_clk_div(dw_cfg.lp_escape_time)
      .WriteTo(&dsi_mmio_);

  // Setup Phy Timers as provided by vendor
  DsiDwPhyTmrLpclkCfgReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_phy_clkhs2lp_time(dw_cfg.phy_timer_clkhs_to_lp)
      .set_phy_clklp2hs_time(dw_cfg.phy_timer_clklp_to_hs)
      .WriteTo(&dsi_mmio_);
  DsiDwPhyTmrCfgReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_phy_hs2lp_time(dw_cfg.phy_timer_hs_to_lp)
      .set_phy_lp2hs_time(dw_cfg.phy_timer_lp_to_hs)
      .WriteTo(&dsi_mmio_);

  DsiDwLpclkCtrlReg::Get()
      .ReadFrom(&dsi_mmio_)
      .set_auto_clklane_ctrl(dw_cfg.auto_clklane)
      .set_phy_txrequestclkhs(1)
      .WriteTo(&dsi_mmio_);

  return ZX_OK;
}

inline bool DsiHostController::IsPldREmpty() {
  return (DsiDwCmdPktStatusReg::Get().ReadFrom(&dsi_mmio_).gen_pld_r_empty() == 1);
}

inline bool DsiHostController::IsPldRFull() {
  return (DsiDwCmdPktStatusReg::Get().ReadFrom(&dsi_mmio_).gen_pld_r_full() == 1);
}

inline bool DsiHostController::IsPldWEmpty() {
  return (DsiDwCmdPktStatusReg::Get().ReadFrom(&dsi_mmio_).gen_pld_w_empty() == 1);
}

inline bool DsiHostController::IsPldWFull() {
  return (DsiDwCmdPktStatusReg::Get().ReadFrom(&dsi_mmio_).gen_pld_w_full() == 1);
}

inline bool DsiHostController::IsCmdEmpty() {
  return (DsiDwCmdPktStatusReg::Get().ReadFrom(&dsi_mmio_).gen_cmd_empty() == 1);
}

inline bool DsiHostController::IsCmdFull() {
  return (DsiDwCmdPktStatusReg::Get().ReadFrom(&dsi_mmio_).gen_cmd_full() == 1);
}

zx_status_t DsiHostController::WaitforFifo(uint32_t bit, bool val) {
  int retry = kRetryMax;
  while (((DsiDwCmdPktStatusReg::Get().ReadFrom(&dsi_mmio_).reg_value() >> bit) & 1) != val &&
         retry--) {
    zx_nanosleep(zx_deadline_after(ZX_USEC(10)));
  }
  if (retry <= 0) {
    return ZX_ERR_TIMED_OUT;
  }
  return ZX_OK;
}

zx_status_t DsiHostController::WaitforPldWNotFull() { return WaitforFifo(kBitPldWFull, 0); }

zx_status_t DsiHostController::WaitforPldWEmpty() { return WaitforFifo(kBitPldWEmpty, 1); }

zx_status_t DsiHostController::WaitforPldRFull() { return WaitforFifo(kBitPldRFull, 1); }

zx_status_t DsiHostController::WaitforPldRNotEmpty() { return WaitforFifo(kBitPldREmpty, 0); }

zx_status_t DsiHostController::WaitforCmdNotFull() { return WaitforFifo(kBitCmdFull, 0); }

zx_status_t DsiHostController::WaitforCmdEmpty() { return WaitforFifo(kBitCmdEmpty, 1); }

void DsiHostController::LogCommand(const mipi_dsi::DsiCommandAndResponse& command) {
  FDF_LOG(INFO, "MIPI DSI Outgoing Packet:");
  FDF_LOG(INFO, "Virtual Channel ID = 0x%x (%d)", command.virtual_channel_id,
          command.virtual_channel_id);
  FDF_LOG(INFO, "Data Type = 0x%x (%d)", command.data_type, command.data_type);
  FDF_LOG(INFO, "Payload size = %zu", command.payload.size());
  FDF_LOG(INFO, "Payload Data: [");
  LogBytes(command.payload);
  FDF_LOG(INFO, "]");
  FDF_LOG(INFO, "Response payload size = %zu", command.response_payload.size());
}

zx_status_t DsiHostController::GenericPayloadRead(uint32_t* data) {
  // make sure there is something valid to read from payload fifo
  if (WaitforPldRNotEmpty() != ZX_OK) {
    FDF_LOG(ERROR, "Timed out waiting for data in PLD R FIFO");
    return ZX_ERR_TIMED_OUT;
  }
  *data = DsiDwGenPldDataReg::Get().ReadFrom(&dsi_mmio_).reg_value();
  return ZX_OK;
}

zx_status_t DsiHostController::GenericHdrWrite(uint32_t data) {
  // make sure cmd fifo is not full before writing into it
  if (WaitforCmdNotFull() != ZX_OK) {
    FDF_LOG(ERROR, "Timed out waiting for CMD FIFO to not be full");
    return ZX_ERR_TIMED_OUT;
  }
  DsiDwGenHdrReg::Get().FromValue(0).set_reg_value(data).WriteTo(&dsi_mmio_);
  return ZX_OK;
}

zx_status_t DsiHostController::GenericPayloadWrite(uint32_t data) {
  // Make sure PLD_W is not full before writing into it
  if (WaitforPldWNotFull() != ZX_OK) {
    FDF_LOG(ERROR, "Timed out waiting for PLD W FIFO to not be full");
    return ZX_ERR_TIMED_OUT;
  }
  DsiDwGenPldDataReg::Get().FromValue(0).set_reg_value(data).WriteTo(&dsi_mmio_);
  return ZX_OK;
}

void DsiHostController::EnableBta() {
  // enable ack req after each packet transmission
  DsiDwCmdModeCfgReg::Get().ReadFrom(&dsi_mmio_).set_ack_rqst_en(1).WriteTo(&dsi_mmio_);
  // enable But Turn-Around request
  DsiDwPckhdlCfgReg::Get().ReadFrom(&dsi_mmio_).set_bta_en(1).WriteTo(&dsi_mmio_);
}

void DsiHostController::DisableBta() {
  // disable ack req after each packet transmission
  DsiDwCmdModeCfgReg::Get().ReadFrom(&dsi_mmio_).set_ack_rqst_en(0).WriteTo(&dsi_mmio_);

  // disable But Turn-Around request
  DsiDwPckhdlCfgReg::Get().ReadFrom(&dsi_mmio_).set_bta_en(0).WriteTo(&dsi_mmio_);
}

zx_status_t DsiHostController::WaitforBtaAck() {
  int retry = kRetryMax;
  while (DsiDwCmdPktStatusReg::Get().ReadFrom(&dsi_mmio_).gen_rd_cmd_busy() && retry--) {
    zx_nanosleep(zx_deadline_after(ZX_USEC(10)));
  }
  if (retry <= 0) {
    FDF_LOG(ERROR, "Timed out waiting for read completion");
    return ZX_ERR_TIMED_OUT;
  }
  return ZX_OK;
}

// MIPI DSI Functions as implemented by DWC IP
zx_status_t DsiHostController::GenWriteShort(const mipi_dsi::DsiCommandAndResponse& command) {
  if (command.payload.size() > 2) {
    FDF_LOG(ERROR, "Invalid payload size (%zu) for a Generic Short Write command",
            command.payload.size());
    return ZX_ERR_INVALID_ARGS;
  }
  if (command.data_type != kMipiDsiDtGenShortWrite0 &&
      command.data_type != kMipiDsiDtGenShortWrite1 &&
      command.data_type != kMipiDsiDtGenShortWrite2 &&
      command.data_type != kMipiDsiDtSetMaxRetPkt) {
    FDF_LOG(ERROR, "Invalid data type (%d) for Generic Short Write", command.data_type);
    return ZX_ERR_INVALID_ARGS;
  }

  uint32_t regVal = 0;
  regVal |= GEN_HDR_DT(command.data_type);
  regVal |= GEN_HDR_VC(command.virtual_channel_id);
  if (command.payload.size() >= 1) {
    regVal |= GEN_HDR_WC_LSB(command.payload[0]);
  }
  if (command.payload.size() == 2) {
    regVal |= GEN_HDR_WC_MSB(command.payload[1]);
  }

  return GenericHdrWrite(regVal);
}

zx_status_t DsiHostController::DcsWriteShort(const mipi_dsi::DsiCommandAndResponse& command) {
  // Check that the payload size and command match
  if (command.payload.size() != 1 && command.payload.size() != 2) {
    FDF_LOG(ERROR, "Invalid payload size (%zu) for a DCS Short Write command",
            command.payload.size());
    return ZX_ERR_INVALID_ARGS;
  }
  if (command.data_type != kMipiDsiDtDcsShortWrite0 &&
      command.data_type != kMipiDsiDtDcsShortWrite1) {
    FDF_LOG(ERROR, "Invalid data type (%d) for Generic Short Write", command.data_type);
    return ZX_ERR_INVALID_ARGS;
  }

  uint32_t regVal = 0;
  regVal |= GEN_HDR_DT(command.data_type);
  regVal |= GEN_HDR_VC(command.virtual_channel_id);
  regVal |= GEN_HDR_WC_LSB(command.payload[0]);
  if (command.payload.size() == 2) {
    regVal |= GEN_HDR_WC_MSB(command.payload[1]);
  }

  return GenericHdrWrite(regVal);
}

// This function writes a generic long command. We can only write a maximum of FIFO_DEPTH
// to the payload fifo. This value is implementation specific.
zx_status_t DsiHostController::GenWriteLong(const mipi_dsi::DsiCommandAndResponse& command) {
  zx_status_t status = ZX_OK;
  uint32_t pld_data_idx = 0;  // payload data index
  uint32_t regVal = 0;

  ZX_DEBUG_ASSERT(command.payload.size() < kMaxPldFifoDepth);
  size_t ts = command.payload.size();  // initial transfer size

  // first write complete words
  while (ts >= 4) {
    regVal = command.payload[pld_data_idx + 0] << 0 | command.payload[pld_data_idx + 1] << 8 |
             command.payload[pld_data_idx + 2] << 16 | command.payload[pld_data_idx + 3] << 24;
    pld_data_idx += 4;

    status = GenericPayloadWrite(regVal);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Generic Payload write failed: %s", zx_status_get_string(status));
      return status;
    }
    ts -= 4;
  }

  // Write remaining bytes
  if (ts > 0) {
    regVal = command.payload[pld_data_idx++] << 0;
    if (ts > 1) {
      regVal |= command.payload[pld_data_idx++] << 8;
    }
    if (ts > 2) {
      regVal |= command.payload[pld_data_idx++] << 16;
    }
    status = GenericPayloadWrite(regVal);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Generic Payload write failed: %s", zx_status_get_string(status));
      return status;
    }
  }

  // At this point, we have written all of our mipi payload to FIFO. Let's transmit it
  regVal = 0;
  regVal |= GEN_HDR_DT(command.data_type);
  regVal |= GEN_HDR_VC(command.virtual_channel_id);
  regVal |= GEN_HDR_WC_LSB(static_cast<uint32_t>(command.payload.size()) & 0xFF);
  regVal |= GEN_HDR_WC_MSB((command.payload.size() & 0xFF00) >> 16);

  return GenericHdrWrite(regVal);
}

zx_status_t DsiHostController::GenRead(const mipi_dsi::DsiCommandAndResponse& command) {
  uint32_t regVal = 0;
  zx_status_t status = ZX_OK;

  // valid cmd packet
  if (command.payload.size() > 2) {
    FDF_LOG(ERROR, "Invalid payload size (%zu) for a Generic Read command", command.payload.size());
    return ZX_ERR_INVALID_ARGS;
  }
  if (command.response_payload.empty()) {
    FDF_LOG(ERROR, "Response payload buffer is empty for a Generic Read command");
    return ZX_ERR_INVALID_ARGS;
  }

  regVal = 0;
  regVal |= GEN_HDR_DT(command.data_type);
  regVal |= GEN_HDR_VC(command.virtual_channel_id);
  if (command.payload.size() >= 1) {
    regVal |= GEN_HDR_WC_LSB(command.payload[0]);
  }
  if (command.payload.size() == 2) {
    regVal |= GEN_HDR_WC_MSB(command.payload[1]);
  }

  // Packet is ready. Let's enable BTA first
  EnableBta();

  if ((status = GenericHdrWrite(regVal)) != ZX_OK) {
    // no need to print extra error msg
    return status;
  }

  if ((status = WaitforBtaAck()) != ZX_OK) {
    // bta never returned. no need to print extra error msg
    return status;
  }

  // Got ACK. Let's start reading
  // We should only read rlen worth of DATA. Let's hope the device is not sending
  // more than it should.
  size_t ts = command.response_payload.size();
  uint32_t rsp_data_idx = 0;
  uint32_t data;
  while (ts >= 4) {
    status = GenericPayloadRead(&data);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to read payload data: %s", zx_status_get_string(status));
      return status;
    }
    command.response_payload[rsp_data_idx++] = static_cast<uint8_t>((data >> 0) & 0xFF);
    command.response_payload[rsp_data_idx++] = static_cast<uint8_t>((data >> 8) & 0xFF);
    command.response_payload[rsp_data_idx++] = static_cast<uint8_t>((data >> 16) & 0xFF);
    command.response_payload[rsp_data_idx++] = static_cast<uint8_t>((data >> 24) & 0xFF);
    ts -= 4;
  }

  // Read out remaining bytes
  if (ts > 0) {
    status = GenericPayloadRead(&data);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to read payload data: %s", zx_status_get_string(status));
      return status;
    }
    command.response_payload[rsp_data_idx++] = (data >> 0) & 0xFF;
    if (ts > 1) {
      command.response_payload[rsp_data_idx++] = (data >> 8) & 0xFF;
    }
    if (ts > 2) {
      command.response_payload[rsp_data_idx++] = (data >> 16) & 0xFF;
    }
  }

  // we are done. Display BTA
  DisableBta();
  return status;
}

zx_status_t DsiHostController::IssueCommand(const mipi_dsi::DsiCommandAndResponse& command) {
  fbl::AutoLock lock(&command_lock_);
  zx_status_t status = ZX_OK;

  switch (command.data_type) {
    case kMipiDsiDtGenShortWrite0:
    case kMipiDsiDtGenShortWrite1:
    case kMipiDsiDtGenShortWrite2:
    case kMipiDsiDtSetMaxRetPkt:
      status = GenWriteShort(command);
      break;
    case kMipiDsiDtGenLongWrite:
    case kMipiDsiDtDcsLongWrite:
      status = GenWriteLong(command);
      break;
    case kMipiDsiDtGenShortRead0:
    case kMipiDsiDtGenShortRead1:
    case kMipiDsiDtGenShortRead2:
    case kMipiDsiDtDcsRead0:
      status = GenRead(command);
      break;
    case kMipiDsiDtDcsShortWrite0:
    case kMipiDsiDtDcsShortWrite1:
      status = DcsWriteShort(command);
      break;
    default:
      FDF_LOG(ERROR, "Unsupported/Invalid DSI command data type: %d", command.data_type);
      status = ZX_ERR_INVALID_ARGS;
  }

  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to perform DSI command and/or response: %s",
            zx_status_get_string(status));
    LogCommand(command);
  }

  return status;
}

}  // namespace designware_dsi
