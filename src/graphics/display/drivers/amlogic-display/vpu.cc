// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/vpu.h"

#include <lib/ddk/debug.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>

#include <cstdint>

#include <ddktl/device.h>

#include "src/graphics/display/drivers/amlogic-display/clock-regs.h"
#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/hhi-regs.h"
#include "src/graphics/display/drivers/amlogic-display/power-regs.h"
#include "src/graphics/display/drivers/amlogic-display/video-input-regs.h"
#include "src/graphics/display/drivers/amlogic-display/vpp-regs.h"
#include "src/graphics/display/drivers/amlogic-display/vpu-regs.h"

namespace amlogic_display {

namespace {
constexpr uint32_t kFirstTimeLoadMagicNumber = 0x304e65;  // 0Ne

constexpr int16_t RGB709_to_YUV709l_coeff[24] = {
    0x0000, 0x0000, 0x0000, 0x00bb, 0x0275, 0x003f, 0x1f99, 0x1ea6, 0x01c2, 0x01c2, 0x1e67, 0x1fd7,
    0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0040, 0x0200, 0x0200, 0x0000, 0x0000, 0x0000,
};

constexpr int16_t YUV709l_to_RGB709_coeff12[24] = {
    -256, -2048, -2048, 4788, 0, 7372, 4788, -876, -2190, 4788, 8686, 0,
    0,    0,     0,     0,    0, 0,    0,    0,    0,     0,    0,    0,
};

// Below co-efficients are used to convert 709L to RGB. The table is provided
// by Amlogic
//    ycbcr limit range, 709 to RGB
//    -16      1.164  0      1.793  0
//    -128     1.164 -0.213 -0.534  0
//    -128     1.164  2.115  0      0
constexpr uint32_t capture_yuv2rgb_coeff[3][3] = {
    {0x04a8, 0x0000, 0x072c}, {0x04a8, 0x1f26, 0x1ddd}, {0x04a8, 0x0876, 0x0000}};
constexpr uint32_t capture_yuv2rgb_preoffset[3] = {0x7c0, 0x600, 0x600};
constexpr uint32_t capture_yuv2rgb_offset[3] = {0, 0, 0};

constexpr VideoInputModuleId kVideoInputModuleId = VideoInputModuleId::kVideoInputModule1;

}  // namespace

// EE Reset registers on the CBUS (regular power-gated config registers domain).
//
// A311D datasheet section 8.8.2.1 "Register Description" > "EE Reset" describes
// the bits under the RESET{0,7}_REGISTER sections. The RESET{0,7}_MASK and
// RESET{0,7}_LEVEL sections describe the interactions between the registers,
// the watchdog timer, and the reset condition.
//
// A311D datasheet section 8.8.2.1 has full MMIO addresses. S905D3 datasheet
// section 6.8.2 with the same title covers the same registers, and also
// explicitly states that the base for all registers is 0xffd0'1000. This
// address is listed under the RESET entry in A311D section 8.1 "System" >
// "Memory Map" and S905D3 datasheet section 6.1 with the same name.
//
// The following datasheets have matching information.
// * S905D2, Section 6.7.2.1 "EE Reset", Section 6.1 "Memory Map"
// * T931, Section 6.8.2.1 "EE Reset", Section 6.1 "Memory Map"
#define RESET0_LEVEL 0x80
#define RESET1_LEVEL 0x84
#define RESET2_LEVEL 0x88
#define RESET4_LEVEL 0x90
#define RESET7_LEVEL 0x9c

#define READ32_VPU_REG(a) vpu_mmio_->Read32(a)
#define WRITE32_VPU_REG(a, v) vpu_mmio_->Write32(v, a)

#define READ32_HHI_REG(a) hhi_mmio_->Read32(a)
#define WRITE32_HHI_REG(a, v) hhi_mmio_->Write32(v, a)

#define READ32_RESET_REG(a) reset_mmio_->Read32(a)
#define WRITE32_RESET_REG(a, v) reset_mmio_->Write32(v, a)

zx_status_t Vpu::Init(ddk::PDevFidl& pdev) {
  if (initialized_) {
    return ZX_OK;
  }

  // Map VPU registers
  zx_status_t status = pdev.MapMmio(MMIO_VPU, &vpu_mmio_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "vpu: Could not map VPU mmio");
    return status;
  }

  // Map HHI registers
  status = pdev.MapMmio(MMIO_HHI, &hhi_mmio_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "vpu: Could not map HHI mmio");
    return status;
  }

  // Map AOBUS registers
  status = pdev.MapMmio(MMIO_AOBUS, &aobus_mmio_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "vpu: Could not map AOBUS mmio");
    return status;
  }

  // Map RESET registers
  status = pdev.MapMmio(MMIO_RESET, &reset_mmio_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "vpu: Could not map RESET mmio");
    return status;
  }

  // VPU object is ready to be used
  initialized_ = true;
  fbl::AutoLock lock(&capture_mutex_);
  capture_state_ = CAPTURE_RESET;
  return ZX_OK;
}

bool Vpu::SetFirstTimeDriverLoad() {
  ZX_DEBUG_ASSERT(initialized_);
  uint32_t regVal = READ32_REG(VPU, VPP_DUMMY_DATA);
  if (regVal == kFirstTimeLoadMagicNumber) {
    // we have already been loaded once. don't set again.
    return false;
  }
  WRITE32_REG(VPU, VPP_DUMMY_DATA, kFirstTimeLoadMagicNumber);
  first_time_load_ = true;
  return true;
}

void Vpu::VppInit() {
  ZX_DEBUG_ASSERT(initialized_);

  // init vpu fifo control register
  SET_BIT32(VPU, VPP_OFIFO_SIZE, 0xFFF, 0, 12);
  WRITE32_REG(VPU, VPP_HOLD_LINES, 0x08080808);
  // default probe_sel, for highlight en
  SET_BIT32(VPU, VPP_MATRIX_CTRL, 0x7, 12, 3);

  // setting up os1 for rgb -> yuv limit
  const int16_t* m = RGB709_to_YUV709l_coeff;

  // VPP WRAP OSD1 matrix
  WRITE32_REG(VPU, VPP_WRAP_OSD1_MATRIX_PRE_OFFSET0_1, ((m[0] & 0xfff) << 16) | (m[1] & 0xfff));
  WRITE32_REG(VPU, VPP_WRAP_OSD1_MATRIX_PRE_OFFSET2, m[2] & 0xfff);
  WRITE32_REG(VPU, VPP_WRAP_OSD1_MATRIX_COEF00_01, ((m[3] & 0x1fff) << 16) | (m[4] & 0x1fff));
  WRITE32_REG(VPU, VPP_WRAP_OSD1_MATRIX_COEF02_10, ((m[5] & 0x1fff) << 16) | (m[6] & 0x1fff));
  WRITE32_REG(VPU, VPP_WRAP_OSD1_MATRIX_COEF11_12, ((m[7] & 0x1fff) << 16) | (m[8] & 0x1fff));
  WRITE32_REG(VPU, VPP_WRAP_OSD1_MATRIX_COEF20_21, ((m[9] & 0x1fff) << 16) | (m[10] & 0x1fff));
  WRITE32_REG(VPU, VPP_WRAP_OSD1_MATRIX_COEF22, m[11] & 0x1fff);
  WRITE32_REG(VPU, VPP_WRAP_OSD1_MATRIX_OFFSET0_1, ((m[18] & 0xfff) << 16) | (m[19] & 0xfff));
  WRITE32_REG(VPU, VPP_WRAP_OSD1_MATRIX_OFFSET2, m[20] & 0xfff);
  SET_BIT32(VPU, VPP_WRAP_OSD1_MATRIX_EN_CTRL, 1, 0, 1);

  // VPP WRAP OSD2 matrix
  WRITE32_REG(VPU, VPP_WRAP_OSD2_MATRIX_PRE_OFFSET0_1, ((m[0] & 0xfff) << 16) | (m[1] & 0xfff));
  WRITE32_REG(VPU, VPP_WRAP_OSD2_MATRIX_PRE_OFFSET2, m[2] & 0xfff);
  WRITE32_REG(VPU, VPP_WRAP_OSD2_MATRIX_COEF00_01, ((m[3] & 0x1fff) << 16) | (m[4] & 0x1fff));
  WRITE32_REG(VPU, VPP_WRAP_OSD2_MATRIX_COEF02_10, ((m[5] & 0x1fff) << 16) | (m[6] & 0x1fff));
  WRITE32_REG(VPU, VPP_WRAP_OSD2_MATRIX_COEF11_12, ((m[7] & 0x1fff) << 16) | (m[8] & 0x1fff));
  WRITE32_REG(VPU, VPP_WRAP_OSD2_MATRIX_COEF20_21, ((m[9] & 0x1fff) << 16) | (m[10] & 0x1fff));
  WRITE32_REG(VPU, VPP_WRAP_OSD2_MATRIX_COEF22, m[11] & 0x1fff);
  WRITE32_REG(VPU, VPP_WRAP_OSD2_MATRIX_OFFSET0_1, ((m[18] & 0xfff) << 16) | (m[19] & 0xfff));
  WRITE32_REG(VPU, VPP_WRAP_OSD2_MATRIX_OFFSET2, m[20] & 0xfff);
  SET_BIT32(VPU, VPP_WRAP_OSD2_MATRIX_EN_CTRL, 1, 0, 1);

  // VPP WRAP OSD3 matrix
  WRITE32_REG(VPU, VPP_WRAP_OSD3_MATRIX_PRE_OFFSET0_1, ((m[0] & 0xfff) << 16) | (m[1] & 0xfff));
  WRITE32_REG(VPU, VPP_WRAP_OSD3_MATRIX_PRE_OFFSET2, m[2] & 0xfff);
  WRITE32_REG(VPU, VPP_WRAP_OSD3_MATRIX_COEF00_01, ((m[3] & 0x1fff) << 16) | (m[4] & 0x1fff));
  WRITE32_REG(VPU, VPP_WRAP_OSD3_MATRIX_COEF02_10, ((m[5] & 0x1fff) << 16) | (m[6] & 0x1fff));
  WRITE32_REG(VPU, VPP_WRAP_OSD3_MATRIX_COEF11_12, ((m[7] & 0x1fff) << 16) | (m[8] & 0x1fff));
  WRITE32_REG(VPU, VPP_WRAP_OSD3_MATRIX_COEF20_21, ((m[9] & 0x1fff) << 16) | (m[10] & 0x1fff));
  WRITE32_REG(VPU, VPP_WRAP_OSD3_MATRIX_COEF22, m[11] & 0x1fff);
  WRITE32_REG(VPU, VPP_WRAP_OSD3_MATRIX_OFFSET0_1, ((m[18] & 0xfff) << 16) | (m[19] & 0xfff));
  WRITE32_REG(VPU, VPP_WRAP_OSD3_MATRIX_OFFSET2, m[20] & 0xfff);
  SET_BIT32(VPU, VPP_WRAP_OSD3_MATRIX_EN_CTRL, 1, 0, 1);

  WRITE32_REG(VPU, DOLBY_PATH_CTRL, 0xf);

  // POST2 matrix: YUV limit -> RGB  default is 12bit
  m = YUV709l_to_RGB709_coeff12;

  // VPP WRAP POST2 matrix
  WRITE32_REG(VPU, VPP_POST2_MATRIX_PRE_OFFSET0_1,
              (((m[0] >> 2) & 0xfff) << 16) | ((m[1] >> 2) & 0xfff));
  WRITE32_REG(VPU, VPP_POST2_MATRIX_PRE_OFFSET2, (m[2] >> 2) & 0xfff);
  WRITE32_REG(VPU, VPP_POST2_MATRIX_COEF00_01,
              (((m[3] >> 2) & 0x1fff) << 16) | ((m[4] >> 2) & 0x1fff));
  WRITE32_REG(VPU, VPP_POST2_MATRIX_COEF02_10,
              (((m[5] >> 2) & 0x1fff) << 16) | ((m[6] >> 2) & 0x1fff));
  WRITE32_REG(VPU, VPP_POST2_MATRIX_COEF11_12,
              (((m[7] >> 2) & 0x1fff) << 16) | ((m[8] >> 2) & 0x1fff));
  WRITE32_REG(VPU, VPP_POST2_MATRIX_COEF20_21,
              (((m[9] >> 2) & 0x1fff) << 16) | ((m[10] >> 2) & 0x1fff));
  WRITE32_REG(VPU, VPP_POST2_MATRIX_COEF22, (m[11] >> 2) & 0x1fff);
  WRITE32_REG(VPU, VPP_POST2_MATRIX_OFFSET0_1,
              (((m[18] >> 2) & 0xfff) << 16) | ((m[19] >> 2) & 0xfff));
  WRITE32_REG(VPU, VPP_POST2_MATRIX_OFFSET2, (m[20] >> 2) & 0xfff);
  SET_BIT32(VPU, VPP_POST2_MATRIX_EN_CTRL, 1, 0, 1);
}

void Vpu::ConfigureClock() {
  ZX_DEBUG_ASSERT(initialized_);
  // vpu clock
  auto vpu_clock_control = VpuClockControl::Get().FromValue(0);
  vpu_clock_control.set_final_mux_selection(VpuClockControl::FinalMuxSource::kBranch0)
      .set_branch0_mux_source(VpuClockControl::ClockSource::kFixed666Mhz)
      .SetBranch0MuxDivider(1)
      .WriteTo(&*hhi_mmio_);
  vpu_clock_control.ReadFrom(&*hhi_mmio_).set_branch0_mux_enabled(true).WriteTo(&*hhi_mmio_);

  // vpu clkb
  // bit 0 is set since kVpuClkFrequency > clkB max frequency (350MHz)
  VpuClockBControl::Get()
      .FromValue(0)
      .set_clock_source(VpuClockBControl::ClockSource::kFixed500Mhz)
      .SetDivider2(2)
      .WriteTo(&*hhi_mmio_);

  // vapb clk
  // turn on ge2d clock since kVpuClkFrequency > 250MHz
  VideoAdvancedPeripheralBusClockControl::Get()
      .FromValue(0)
      .set_final_mux_selection(VideoAdvancedPeripheralBusClockControl::FinalMuxSource::kBranch0)
      .set_ge2d_clock_enabled(true)
      .set_branch0_mux_source(VideoAdvancedPeripheralBusClockControl::ClockSource::kFixed500Mhz)
      .SetBranch0MuxDivider(2)
      .WriteTo(&*hhi_mmio_);
  VideoAdvancedPeripheralBusClockControl::Get()
      .ReadFrom(&*hhi_mmio_)
      .set_branch0_mux_enabled(true)
      .WriteTo(&*hhi_mmio_);

  VideoClockOutputControl::Get()
      .ReadFrom(&*hhi_mmio_)
      .set_encoder_interlaced_enabled(false)
      .set_encoder_tv_enabled(false)
      .set_encoder_progressive_enabled(false)
      .set_encoder_lvds_enabled(false)
      .set_video_dac_clock_enabled(false)
      .set_hdmi_tx_pixel_clock_enabled(false)
      .set_lcd_analog_clock_phy3_enabled(false)
      .set_lcd_analog_clock_phy2_enabled(false)
      .WriteTo(&*hhi_mmio_);

  // dmc_arb_config
  WRITE32_REG(VPU, VPU_RDARB_MODE_L1C1, 0x0);
  WRITE32_REG(VPU, VPU_RDARB_MODE_L1C2, 0x10000);
  WRITE32_REG(VPU, VPU_RDARB_MODE_L2C1, 0x900000);
  WRITE32_REG(VPU, VPU_WRARB_MODE_L2C1, 0x20000);
}

namespace {

template <typename RegisterType>
void SetPowerBits(fdf::MmioBuffer& mmio, bool powered_on, int begin_bit_index, int end_bit_index) {
  // TODO(fxbug.com/132123): AMLogic-supplied software flips each bit
  // individually, with a 5us delay between flips. The power sequences in the
  // datasheets have no mention of individual bits, and seem to suggest setting
  // each register to its final value in one operation. Carry out an experiment
  // to document if the datasheet sequences work, as the AMLogic software may
  // cater to older chips.  Document the result either way.
  RegisterType power_register = RegisterType::Get().ReadFrom(&mmio);
  const uint32_t bit_value = powered_on ? 0 : 1;
  for (int bit_index = begin_bit_index; bit_index < end_bit_index; ++bit_index) {
    const uint32_t bit_mask = bit_value << bit_index;
    power_register.set_reg_value(power_register.reg_value() & ~bit_mask).WriteTo(&mmio);
    zx::nanosleep(zx::deadline_after(zx::usec(5)));
  }
}

template <typename RegisterType>
void SetPowerUnits(fdf::MmioBuffer& mmio, bool powered_on, int begin_unit_index,
                   int end_unit_index) {
  // TODO(fxbug.com/132123): AMLogic-supplied software flips each unit
  // individually, with a 5us delay between flips. The power sequences in the
  // datasheets have no mention of individual bits, and seem to suggest setting
  // each register to its final value in one operation. Carry out an experiment
  // to document if the datasheet sequences work, as the AMLogic software may
  // cater to older chips.  Document the result either way.
  RegisterType power_register = RegisterType::Get().ReadFrom(&mmio);
  const MemoryPowerDomainMode mode =
      powered_on ? MemoryPowerDomainMode::kPoweredOn : MemoryPowerDomainMode::kPoweredOff;
  for (int unit_index = begin_unit_index; unit_index < end_unit_index; ++unit_index) {
    const uint32_t unit_mask = static_cast<uint32_t>(mode) << unit_index;
    power_register.set_reg_value(power_register.reg_value() & ~unit_mask).WriteTo(&mmio);
    zx::nanosleep(zx::deadline_after(zx::usec(5)));
  }
}

}  // namespace

void Vpu::PowerOn() {
  ZX_DEBUG_ASSERT(initialized_);

  // Implements the power sequences documented below.
  //
  // A311D datasheet Section 8.2.3 "EE Top Level Power Modes", Table 8-6 "Power
  //     Sequence of VPU", page 88
  // S905D3 datasheet Section 6.2.3.2 "EE Top Level Power Modes" > "VPU",
  //     Table 6-3 "Power & Global Clock Control Summary", page 75
  // S905D2 datasheet Section 6.2.3 "EE Top Level Power Modes", Table 6-4 "Power
  //     Sequence of EE Domain", page 84

  auto general_power = AlwaysOnGeneralPowerSleep::Get().ReadFrom(&aobus_mmio_.value());
  general_power.set_vpu_hdmi_powered_off(false).WriteTo(&aobus_mmio_.value());

  // TODO(fxbug.com/132123): The A311D power sequence waits for bits 9-8 in
  // `AlwaysOnGeneralPowerAck`here. The S905D3 and S905D2 power sequences only
  // wait for bit 8 in the same register. AMLogic-supplied bringup code uses a
  // hard-coded 20us timeout instead.

  SetPowerUnits<VpuMemoryPower0>(hhi_mmio_.value(), /*powered_on=*/true, 0, 16);
  SetPowerUnits<VpuMemoryPower1>(hhi_mmio_.value(), /*powered_on=*/true, 0, 16);

  // The S905D2 power sequence does not include `VpuMemoryPower2`. However, the
  // datasheet has a register-level reference for it, which indicates that all
  // fields outside of `vpp_watermark_power` are unused. So, the sequence below
  // is harmless on S905D2.
  //
  // The A311D power sequence lists bits 0-31 of `VpuMemoryPower2`. We follow
  // the AMLogic-supplied bringup code, which only flips the bits below.

  // TODO(fxbug.com/132123): The S905D3 power sequence and AMLogic-supplied
  // bringup code flips bits 0-31 of `VpuMemoryPower2`.
  SetPowerUnits<VpuMemoryPower2>(hhi_mmio_.value(), /*powered_on=*/true, 0, 1);
  SetPowerUnits<VpuMemoryPower2>(hhi_mmio_.value(), /*powered_on=*/true, 2, 9);
  SetPowerUnits<VpuMemoryPower2>(hhi_mmio_.value(), /*powered_on=*/true, 15, 16);

  // TODO(fxbug.com/132123): The S905D3 power sequence also flips bits 0-31 of
  // registers `VpuMemoryPower3` and `VpuMemoryPower4`. The AMLogic-supplied
  // bringup code flips bits 0-31 of `VpuMemoryPower3` and bits 0-3 of
  // `VpuMemoryPower4`.

  SetPowerBits<MemoryPower0>(hhi_mmio_.value(), /*powered_on=*/true, 8, 16);
  zx::nanosleep(zx::deadline_after(zx::usec(20)));

  // Reset VIU + VENC
  // Reset VENCI + VENCP + VADC + VENCL
  // Reset HDMI-APB + HDMI-SYS + HDMI-TX + HDMI-CEC
  CLEAR_MASK32(RESET, RESET0_LEVEL, ((1 << 5) | (1 << 10) | (1 << 19) | (1 << 13)));
  CLEAR_MASK32(RESET, RESET1_LEVEL, (1 << 5));
  CLEAR_MASK32(RESET, RESET2_LEVEL, (1 << 15));
  CLEAR_MASK32(RESET, RESET4_LEVEL,
               ((1 << 6) | (1 << 7) | (1 << 13) | (1 << 5) | (1 << 9) | (1 << 4) | (1 << 12)));
  CLEAR_MASK32(RESET, RESET7_LEVEL, (1 << 7));

  // TODO(fxbug.com/132123): The A311D and S905D3 power sequences disable output
  // isolation after VPU power ACK, and before changing the VPU memory power
  // registers. The AMLogic-supplied bringup code disables isolation after
  // completing the reset sequence.

  // TODO(fxbug.com/132123): The S905D3 power sequence and AMLogic-supplied
  // bringup code configure VPU/HDMI isolation in the
  // `AlwaysOnGeneralPowerIsolation` register instead.
  general_power.set_vpu_hdmi_isolation_enabled_s905d2_a311d(false).WriteTo(&aobus_mmio_.value());

  // release Reset
  SET_MASK32(RESET, RESET0_LEVEL, ((1 << 5) | (1 << 10) | (1 << 19) | (1 << 13)));
  SET_MASK32(RESET, RESET1_LEVEL, (1 << 5));
  SET_MASK32(RESET, RESET2_LEVEL, (1 << 15));
  SET_MASK32(RESET, RESET4_LEVEL,
             ((1 << 6) | (1 << 7) | (1 << 13) | (1 << 5) | (1 << 9) | (1 << 4) | (1 << 12)));
  SET_MASK32(RESET, RESET7_LEVEL, (1 << 7));

  ConfigureClock();
}

void Vpu::PowerOff() {
  ZX_DEBUG_ASSERT(initialized_);

  // Implements the steps in Table 8-6 "Power Sequence of VPU" in A311D
  // datasheet Section 8.2.3 "EE Top Level Power Modes", in reverse order.

  auto general_power = AlwaysOnGeneralPowerSleep::Get().ReadFrom(&aobus_mmio_.value());
  general_power.set_vpu_hdmi_isolation_enabled_s905d2_a311d(true).WriteTo(&aobus_mmio_.value());
  zx::nanosleep(zx::deadline_after(zx::usec(20)));

  // TODO(fxbug.com/132123): The memories are powered down in exactly the same
  // order as powering up. If the order doesn't matter, we can unify the code.

  SetPowerUnits<VpuMemoryPower0>(hhi_mmio_.value(), /*powered_on=*/false, 0, 16);
  SetPowerUnits<VpuMemoryPower1>(hhi_mmio_.value(), /*powered_on=*/false, 0, 16);

  // The S905D2 power sequence does not include `VpuMemoryPower2`. However, the
  // datasheet has a register-level reference for it, which indicates that all
  // fields outside of `vpp_watermark_power` are unused. So, the sequence below
  // is harmless on S905D2.

  SetPowerUnits<VpuMemoryPower2>(hhi_mmio_.value(), /*powered_on=*/false, 0, 1);
  SetPowerUnits<VpuMemoryPower2>(hhi_mmio_.value(), /*powered_on=*/false, 2, 9);
  SetPowerUnits<VpuMemoryPower2>(hhi_mmio_.value(), /*powered_on=*/false, 15, 16);

  SetPowerBits<MemoryPower0>(hhi_mmio_.value(), /*powered_on=*/false, 8, 16);
  zx::nanosleep(zx::deadline_after(zx::usec(20)));

  // TODO(fxbug.com/132123): The A311D power sequence waits for bits 9-8 in
  // `AlwaysOnGeneralPowerAck`here. The S905D3 and S905D2 power sequences only
  // wait for bit 8 in the same register.

  general_power.set_vpu_hdmi_powered_off(true).WriteTo(&aobus_mmio_.value());

  VideoAdvancedPeripheralBusClockControl::Get()
      .ReadFrom(&*hhi_mmio_)
      .set_branch0_mux_enabled(false)
      .WriteTo(&*hhi_mmio_);
  VpuClockControl::Get().ReadFrom(&*hhi_mmio_).set_branch0_mux_enabled(false).WriteTo(&*hhi_mmio_);
}

void Vpu::AfbcPower(bool power_on) {
  ZX_DEBUG_ASSERT(initialized_);
  auto vpu_memory_power2 = VpuMemoryPower2::Get().ReadFrom(&hhi_mmio_.value());
  vpu_memory_power2.set_mali_afbc_decoder_power(power_on ? MemoryPowerDomainMode::kPoweredOn
                                                         : MemoryPowerDomainMode::kPoweredOff);
  vpu_memory_power2.WriteTo(&hhi_mmio_.value());
  zx::nanosleep(zx::deadline_after(zx::usec(5)));
}

zx_status_t Vpu::CaptureInit(uint8_t canvas_idx, uint32_t height, uint32_t stride) {
  ZX_DEBUG_ASSERT(initialized_);
  fbl::AutoLock lock(&capture_mutex_);
  if (capture_state_ == CAPTURE_ACTIVE) {
    zxlogf(ERROR, "Capture in progress");
    return ZX_ERR_UNAVAILABLE;
  }

  // Set up sources for writeback mux 0.
  WritebackMuxControl::Get()
      .ReadFrom(&(*vpu_mmio_))
      .SetMux0Selection(WritebackMuxSource::kDisabled)
      .WriteTo(&(*vpu_mmio_));
  WritebackMuxControl::Get()
      .ReadFrom(&(*vpu_mmio_))
      .SetMux0Selection(WritebackMuxSource::kViuWriteback0)
      .WriteTo(&(*vpu_mmio_));
  WrBackMiscCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).set_chan0_hsync_enable(1).WriteTo(&(*vpu_mmio_));
  WrBackCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).set_chan0_sel(5).WriteTo(&(*vpu_mmio_));

  // setup hold lines and vdin selection to internal loopback
  VideoInputCommandControl::Get(kVideoInputModuleId)
      .ReadFrom(&(*vpu_mmio_))
      .set_hold_lines(0)
      .set_input_source_selection(VideoInputCommandControl::InputSource::kWritebackMux0)
      .WriteTo(&(*vpu_mmio_));
  VdinLFifoCtrlReg::Get().FromValue(0).set_fifo_buf_size(0x780).WriteTo(&(*vpu_mmio_));

  // Setup input channel FIFO.src/graphics/display/drivers/amlogic-display/video-input-regs.h
  VideoInputChannelFifoControl3::Get(kVideoInputModuleId)
      .ReadFrom(&(*vpu_mmio_))
      .set_channel6_data_enabled(true)
      .set_channel6_go_field_signal_enabled(true)
      .set_channel6_go_line_signal_enabled(true)
      .set_channel6_input_vsync_is_negative(false)
      .set_channel6_input_hsync_is_negative(false)
      .set_channel6_async_fifo_software_reset_on_vsync(true)
      .set_channel6_clear_fifo_overflow_bit(false)
      .set_channel6_async_fifo_software_reset(false)
      .WriteTo(&(*vpu_mmio_));

  VdInMatrixCtrlReg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_select(1)
      .set_enable(1)
      .WriteTo(&(*vpu_mmio_));

  VdinCoef00_01Reg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_coef00(capture_yuv2rgb_coeff[0][0])
      .set_coef01(capture_yuv2rgb_coeff[0][1])
      .WriteTo(&(*vpu_mmio_));

  VdinCoef02_10Reg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_coef02(capture_yuv2rgb_coeff[0][2])
      .set_coef10(capture_yuv2rgb_coeff[1][0])
      .WriteTo(&(*vpu_mmio_));

  VdinCoef11_12Reg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_coef11(capture_yuv2rgb_coeff[1][1])
      .set_coef12(capture_yuv2rgb_coeff[1][2])
      .WriteTo(&(*vpu_mmio_));

  VdinCoef20_21Reg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_coef20(capture_yuv2rgb_coeff[2][0])
      .set_coef21(capture_yuv2rgb_coeff[2][1])
      .WriteTo(&(*vpu_mmio_));

  VdinCoef22Reg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_coef22(capture_yuv2rgb_coeff[2][2])
      .WriteTo(&(*vpu_mmio_));

  VdinOffset0_1Reg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_offset0(capture_yuv2rgb_offset[0])
      .set_offset1(capture_yuv2rgb_offset[1])
      .WriteTo(&(*vpu_mmio_));

  VdinOffset2Reg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_offset2(capture_yuv2rgb_offset[2])
      .WriteTo(&(*vpu_mmio_));

  VdinPreOffset0_1Reg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_preoffset0(capture_yuv2rgb_preoffset[0])
      .set_preoffset1(capture_yuv2rgb_preoffset[1])
      .WriteTo(&(*vpu_mmio_));

  VdinPreOffset2Reg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_preoffset2(capture_yuv2rgb_preoffset[2])
      .WriteTo(&(*vpu_mmio_));

  // setup vdin input dimensions
  VdinIntfWidthM1Reg::Get().FromValue(stride - 1).WriteTo(&(*vpu_mmio_));

  // Configure memory size
  VdInWrHStartEndReg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_start(0)
      .set_end(stride - 1)
      .WriteTo(&(*vpu_mmio_));
  VdInWrVStartEndReg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_start(0)
      .set_end(height - 1)
      .WriteTo(&(*vpu_mmio_));

  // Write output canvas index, 128 bit endian, eol with width, enable 4:4:4 RGB888 mode
  VdInWrCtrlReg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_eol_sel(1)
      .set_word_swap(1)
      .set_memory_format(1)
      .set_canvas_idx(canvas_idx)
      .WriteTo(&(*vpu_mmio_));

  // TODO(fxbug.com/132123): This seems unnecessary. Vpu::PowerOn() already
  // enables both `vdin0_memory_power` and `vdin1_memory_power`. The
  // AMLogic-supplied bringup code waits 5us after flipping a power gate.
  auto vpu_memory_power0 = VpuMemoryPower0::Get().ReadFrom(&(*hhi_mmio_));
  vpu_memory_power0.set_vdin1_memory_power(MemoryPowerDomainMode::kPoweredOn)
      .WriteTo(&(*hhi_mmio_));

  // Capture state is now in IDLE mode
  capture_state_ = CAPTURE_IDLE;
  return ZX_OK;
}

zx_status_t Vpu::CaptureStart() {
  ZX_DEBUG_ASSERT(initialized_);
  fbl::AutoLock lock(&capture_mutex_);
  if (capture_state_ != CAPTURE_IDLE) {
    zxlogf(ERROR, "Capture state is not idle! (%d)", capture_state_);
    return ZX_ERR_BAD_STATE;
  }

  // Now that loopback mode is configured, start capture
  // pause write output
  VdInWrCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).set_write_ctrl(0).WriteTo(&(*vpu_mmio_));

  // disable vdin path
  VideoInputCommandControl::Get(kVideoInputModuleId)
      .ReadFrom(&(*vpu_mmio_))
      .set_video_input_enabled(false)
      .WriteTo(&(*vpu_mmio_));

  // reset mif
  VdInMiscCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).set_mif_reset(1).WriteTo(&(*vpu_mmio_));
  zx_nanosleep(zx_deadline_after(ZX_USEC(10)));
  VdInMiscCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).set_mif_reset(0).WriteTo(&(*vpu_mmio_));

  // resume write output
  VdInWrCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).set_write_ctrl(1).WriteTo(&(*vpu_mmio_));

  // wait until resets finishes
  zx_nanosleep(zx_deadline_after(ZX_MSEC(20)));

  // Clear status bit
  VdInWrCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).set_done_status_clear_bit(1).WriteTo(&(*vpu_mmio_));

  // Set as urgent
  VdInWrCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).set_write_req_urgent(1).WriteTo(&(*vpu_mmio_));

  // Enable loopback
  VdInWrCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).set_write_mem_enable(1).WriteTo(&(*vpu_mmio_));

  // enable vdin path
  VideoInputCommandControl::Get(kVideoInputModuleId)
      .ReadFrom(&(*vpu_mmio_))
      .set_video_input_enabled(true)
      .WriteTo(&(*vpu_mmio_));

  capture_state_ = CAPTURE_ACTIVE;
  return ZX_OK;
}

zx_status_t Vpu::CaptureDone() {
  fbl::AutoLock lock(&capture_mutex_);
  capture_state_ = CAPTURE_IDLE;
  // pause write output
  VdInWrCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).set_write_ctrl(0).WriteTo(&(*vpu_mmio_));

  // disable vdin path
  VideoInputCommandControl::Get(kVideoInputModuleId)
      .ReadFrom(&(*vpu_mmio_))
      .set_video_input_enabled(0)
      .WriteTo(&(*vpu_mmio_));

  // reset mif
  VdInMiscCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).set_mif_reset(1).WriteTo(&(*vpu_mmio_));
  zx_nanosleep(zx_deadline_after(ZX_USEC(10)));
  VdInMiscCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).set_mif_reset(0).WriteTo(&(*vpu_mmio_));

  return ZX_OK;
}

void Vpu::CapturePrintRegisters() {
  zxlogf(INFO, "** Display Loopback Register Dump **");
  zxlogf(INFO, "VdInComCtrl0Reg = 0x%x",
         VideoInputCommandControl::Get(kVideoInputModuleId).ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdInComStatus0Reg = 0x%x",
         VideoInputCommandStatus0::Get(kVideoInputModuleId).ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdInMatrixCtrlReg = 0x%x",
         VdInMatrixCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdinCoef00_01Reg = 0x%x",
         VdinCoef00_01Reg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdinCoef02_10Reg = 0x%x",
         VdinCoef02_10Reg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdinCoef11_12Reg = 0x%x",
         VdinCoef11_12Reg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdinCoef20_21Reg = 0x%x",
         VdinCoef20_21Reg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdinCoef22Reg = 0x%x", VdinCoef22Reg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdinOffset0_1Reg = 0x%x",
         VdinOffset0_1Reg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdinOffset2Reg = 0x%x", VdinOffset2Reg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdinPreOffset0_1Reg = 0x%x",
         VdinPreOffset0_1Reg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdinPreOffset2Reg = 0x%x",
         VdinPreOffset2Reg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdinLFifoCtrlReg = 0x%x",
         VdinLFifoCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdinIntfWidthM1Reg = 0x%x",
         VdinIntfWidthM1Reg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdInWrCtrlReg = 0x%x", VdInWrCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdInWrHStartEndReg = 0x%x",
         VdInWrHStartEndReg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdInWrVStartEndReg = 0x%x",
         VdInWrVStartEndReg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(
      INFO, "VdInAFifoCtrl3Reg = 0x%x",
      VideoInputChannelFifoControl3::Get(kVideoInputModuleId).ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdInMiscCtrlReg = 0x%x",
         VdInMiscCtrlReg::Get().ReadFrom(&(*vpu_mmio_)).reg_value());
  zxlogf(INFO, "VdInIfMuxCtrlReg = 0x%x",
         WritebackMuxControl::Get().ReadFrom(&(*vpu_mmio_)).reg_value());

  zxlogf(INFO, "Dumping from 0x1300 to 0x1373");
  for (int i = 0x1300; i <= 0x1373; i++) {
    zxlogf(INFO, "reg[0x%x] = 0x%x", i, READ32_VPU_REG(i << 2));
  }
}

}  // namespace amlogic_display
