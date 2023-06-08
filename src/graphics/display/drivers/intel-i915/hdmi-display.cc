// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-i915/hdmi-display.h"

#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/time.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <cmath>
#include <cstdint>
#include <iterator>
#include <limits>
#include <optional>

#include <fbl/auto_lock.h>

#include "src/graphics/display/drivers/intel-i915/ddi-physical-layer-manager.h"
#include "src/graphics/display/drivers/intel-i915/dpll-config.h"
#include "src/graphics/display/drivers/intel-i915/dpll.h"
#include "src/graphics/display/drivers/intel-i915/hardware-common.h"
#include "src/graphics/display/drivers/intel-i915/i2c/gmbus-gpio.h"
#include "src/graphics/display/drivers/intel-i915/intel-i915.h"
#include "src/graphics/display/drivers/intel-i915/pci-ids.h"
#include "src/graphics/display/drivers/intel-i915/poll-until.h"
#include "src/graphics/display/drivers/intel-i915/registers-ddi.h"
#include "src/graphics/display/drivers/intel-i915/registers-dpll.h"
#include "src/graphics/display/drivers/intel-i915/registers-gmbus.h"
#include "src/graphics/display/drivers/intel-i915/registers-pipe.h"
#include "src/graphics/display/drivers/intel-i915/registers-transcoder.h"
#include "src/graphics/display/drivers/intel-i915/registers.h"

namespace i915 {

// I2c functions

namespace {

// Recommended DDI buffer translation programming values

struct DdiPhyConfigEntry {
  uint32_t entry2;
  uint32_t entry1;
};

// The tables below have the values recommended by the documentation.
//
// Kaby Lake: IHD-OS-KBL-Vol 12-1.17 pages 187-190
// Skylake: IHD-OS-SKL-Vol 12-05.16 pages 181-183
//
// TODO(fxbug.dev/108252): Per-entry Iboost values.

constexpr DdiPhyConfigEntry kPhyConfigHdmiSkylakeUhs[11] = {
    {0x000000ac, 0x00000018}, {0x0000009d, 0x00005012}, {0x00000088, 0x00007011},
    {0x000000a1, 0x00000018}, {0x00000098, 0x00000018}, {0x00000088, 0x00004013},
    {0x000000cd, 0x80006012}, {0x000000df, 0x00000018}, {0x000000cd, 0x80003015},
    {0x000000c0, 0x80003015}, {0x000000c0, 0x80000018},
};

constexpr DdiPhyConfigEntry kPhyConfigHdmiSkylakeY[11] = {
    {0x000000a1, 0x00000018}, {0x000000df, 0x00005012}, {0x000000cb, 0x80007011},
    {0x000000a4, 0x00000018}, {0x0000009d, 0x00000018}, {0x00000080, 0x00004013},
    {0x000000c0, 0x80006012}, {0x0000008a, 0x00000018}, {0x000000c0, 0x80003015},
    {0x000000c0, 0x80003015}, {0x000000c0, 0x80000018},
};

cpp20::span<const DdiPhyConfigEntry> GetHdmiPhyConfigEntries(uint16_t device_id,
                                                             uint8_t* default_iboost) {
  if (is_skl_y(device_id) || is_kbl_y(device_id)) {
    *default_iboost = 3;
    return kPhyConfigHdmiSkylakeY;
  }

  *default_iboost = 1;
  return kPhyConfigHdmiSkylakeUhs;
}

}  // namespace

// Modesetting functions

// On DisplayDevice creation we cannot determine whether it is an HDMI
// display; this will be updated when intel-i915 Controller gets EDID
// information for this device (before Init()).
HdmiDisplay::HdmiDisplay(Controller* controller, display::DisplayId id, DdiId ddi_id,
                         DdiReference ddi_reference, const ddk::I2cImplProtocolClient& i2c)
    : DisplayDevice(controller, id, ddi_id, std::move(ddi_reference), Type::kHdmi), i2c_(i2c) {}

HdmiDisplay::~HdmiDisplay() = default;

bool HdmiDisplay::Query() {
  // HDMI isn't supported on these DDIs
  const registers::Platform platform = GetPlatform(controller()->device_id());
  if (!GMBusPinPair::HasValidPinPair(ddi_id(), platform)) {
    return false;
  }

  // Reset the GMBus registers and disable GMBus interrupts
  registers::GMBusClockPortSelect::Get().FromValue(0).WriteTo(mmio_space());
  registers::GMBusControllerInterruptMask::Get().FromValue(0).WriteTo(mmio_space());

  // The I2C address for writing the DDC data offset/reading DDC data.
  //
  // VESA Enhanced Display Data Channel (E-DDC) Standard version 1.3 revised
  // Dec 31 2020, Section 2.2.3 "DDC Addresses", page 17.
  //
  // TODO(fxbug.dev/117194): De-duplicate DDC address definitions from
  // HdmiDisplay and GMBusI2c implementations.
  constexpr static uint8_t kDdcDataAddress = 0x50;

  // The only way to tell if an HDMI monitor is actually connected is
  // to try to read a byte over I2C data address.
  for (unsigned i = 0; i < 3; i++) {
    uint8_t test_data = 0;
    i2c_impl_op_t op = {
        .address = kDdcDataAddress,
        .data_buffer = &test_data,
        .data_size = 1,
        .is_read = true,
        .stop = 1,
    };
    registers::GMBusClockPortSelect::Get().FromValue(0).WriteTo(mmio_space());
    if (i2c().Transact(&op, 1) == ZX_OK) {
      zxlogf(TRACE, "Found a hdmi/dvi monitor");
      return true;
    }
    zx_nanosleep(zx_deadline_after(ZX_MSEC(5)));
  }
  zxlogf(TRACE, "Failed to query hdmi i2c bus");
  return false;
}

bool HdmiDisplay::InitDdi() {
  // All the init happens during modeset
  return true;
}

bool HdmiDisplay::DdiModeset(const display_mode_t& mode) {
  pipe()->Reset();
  controller()->ResetDdi(ddi_id(), pipe()->connected_transcoder_id());

  const int32_t pixel_clock_khz = mode.pixel_clock_10khz * 10;
  DdiPllConfig pll_config = {
      .ddi_clock_khz = pixel_clock_khz * 5,
      .spread_spectrum_clocking = false,
      .admits_display_port = false,
      .admits_hdmi = true,
  };

  DisplayPll* dpll =
      controller()->dpll_manager()->SetDdiPllConfig(ddi_id(), /*is_edp=*/false, pll_config);
  if (dpll == nullptr) {
    return false;
  }

  ZX_DEBUG_ASSERT(controller()->power());
  controller()->power()->SetDdiIoPowerState(ddi_id(), /*enable=*/true);
  if (!PollUntil([&] { return controller()->power()->GetDdiIoPowerState(ddi_id()); }, zx::usec(1),
                 20)) {
    zxlogf(ERROR, "DDI %d IO power did not come up in 20us", ddi_id());
    return false;
  }

  controller()->power()->SetAuxIoPowerState(ddi_id(), /*enable=*/true);
  if (!PollUntil([&] { return controller()->power()->GetAuxIoPowerState(ddi_id()); }, zx::usec(1),
                 10)) {
    zxlogf(ERROR, "DDI %d IO power did not come up in 10us", ddi_id());
    return false;
  }

  return true;
}

bool HdmiDisplay::PipeConfigPreamble(const display_mode_t& mode, PipeId pipe_id,
                                     TranscoderId transcoder_id) {
  ZX_DEBUG_ASSERT_MSG(transcoder_id != TranscoderId::TRANSCODER_EDP,
                      "The EDP transcoder doesn't do HDMI");

  registers::TranscoderRegs transcoder_regs(transcoder_id);

  // Configure Transcoder Clock Select
  auto transcoder_clock_select = transcoder_regs.ClockSelect().ReadFrom(mmio_space());
  if (is_tgl(controller()->device_id())) {
    transcoder_clock_select.set_ddi_clock_tiger_lake(ddi_id());
  } else {
    transcoder_clock_select.set_ddi_clock_kaby_lake(ddi_id());
  }
  transcoder_clock_select.WriteTo(mmio_space());

  return true;
}

bool HdmiDisplay::PipeConfigEpilogue(const display_mode_t& mode, PipeId pipe_id,
                                     TranscoderId transcoder_id) {
  ZX_DEBUG_ASSERT(type() == DisplayDevice::Type::kHdmi || type() == DisplayDevice::Type::kDvi);
  ZX_DEBUG_ASSERT_MSG(transcoder_id != TranscoderId::TRANSCODER_EDP,
                      "The EDP transcoder doesn't do HDMI");

  registers::TranscoderRegs transcoder_regs(transcoder_id);

  auto transcoder_ddi_control = transcoder_regs.DdiControl().ReadFrom(mmio_space());
  transcoder_ddi_control.set_enabled(true);
  if (is_tgl(controller()->device_id())) {
    transcoder_ddi_control.set_ddi_tiger_lake(ddi_id());
  } else {
    transcoder_ddi_control.set_ddi_kaby_lake(ddi_id());
  }
  transcoder_ddi_control.set_ddi_mode(type() == DisplayDevice::Type::kHdmi
                                          ? registers::TranscoderDdiControl::kModeHdmi
                                          : registers::TranscoderDdiControl::kModeDvi);
  transcoder_ddi_control.set_bits_per_color(registers::TranscoderDdiControl::k8bpc)
      .set_vsync_polarity_not_inverted((mode.flags & MODE_FLAG_VSYNC_POSITIVE) != 0)
      .set_hsync_polarity_not_inverted((mode.flags & MODE_FLAG_HSYNC_POSITIVE) != 0)
      .set_is_port_sync_secondary_kaby_lake(false)
      .set_allocate_display_port_virtual_circuit_payload(false)
      .WriteTo(mmio_space());

  auto transcoder_config = transcoder_regs.Config().ReadFrom(mmio_space());
  transcoder_config.set_enabled_target(true)
      .set_interlaced_display((mode.flags & MODE_FLAG_INTERLACED) != 0)
      .WriteTo(mmio_space());

  // Configure voltage swing and related IO settings.
  //
  // TODO(fxbug.dev/114460): Move voltage swing configuration logic to a
  // DDI-specific class.

  // kUseDefaultIdx always fails the idx-in-bounds check, so no additional handling is needed
  uint8_t idx = controller()->igd_opregion().GetHdmiBufferTranslationIndex(ddi_id());
  uint8_t i_boost_override = controller()->igd_opregion().GetIBoost(ddi_id(), false /* is_dp */);

  uint8_t default_iboost;
  const cpp20::span<const DdiPhyConfigEntry> entries =
      GetHdmiPhyConfigEntries(controller()->device_id(), &default_iboost);
  if (idx >= entries.size()) {
    idx = 8;  // Default index
  }

  registers::DdiRegs ddi_regs(ddi_id());
  auto phy_config_entry1 = ddi_regs.PhyConfigEntry1(9).FromValue(0);
  phy_config_entry1.set_reg_value(entries[idx].entry1);
  if (i_boost_override) {
    phy_config_entry1.set_balance_leg_enable(1);
  }
  phy_config_entry1.WriteTo(mmio_space());

  auto phy_config_entry2 = ddi_regs.PhyConfigEntry2(9).FromValue(0);
  phy_config_entry2.set_reg_value(entries[idx].entry2).WriteTo(mmio_space());

  auto phy_balance_control = registers::DdiPhyBalanceControl::Get().ReadFrom(mmio_space());
  phy_balance_control.set_disable_balance_leg(0);
  phy_balance_control.balance_leg_select_for_ddi(ddi_id()).set(i_boost_override ? i_boost_override
                                                                                : default_iboost);
  phy_balance_control.WriteTo(mmio_space());

  // Configure and enable DDI_BUF_CTL
  auto buffer_control = ddi_regs.BufferControl().ReadFrom(mmio_space());
  buffer_control.set_enabled(true);
  buffer_control.WriteTo(mmio_space());

  return true;
}

DdiPllConfig HdmiDisplay::ComputeDdiPllConfig(int32_t pixel_clock_10khz) {
  const int32_t pixel_clock_khz = pixel_clock_10khz * 10;
  return DdiPllConfig{
      .ddi_clock_khz = pixel_clock_khz * 5,
      .spread_spectrum_clocking = false,
      .admits_display_port = false,
      .admits_hdmi = true,
  };
}

bool HdmiDisplay::CheckPixelRate(uint64_t pixel_rate_hz) {
  // Pixel rates of 300M/165M pixels per second for HDMI/DVI. The Intel docs state
  // that the maximum link bit rate of an HDMI port is 3GHz, not 3.4GHz that would
  // be expected  based on the HDMI spec.
  if ((type() == DisplayDevice::Type::kHdmi ? 300'000'000 : 165'000'000) < pixel_rate_hz) {
    return false;
  }

  DdiPllConfig pll_config = ComputeDdiPllConfig(static_cast<int32_t>(pixel_rate_hz / 10'000));
  if (pll_config.IsEmpty()) {
    return false;
  }

  DpllOscillatorConfig dco_config = CreateDpllOscillatorConfigKabyLake(pll_config.ddi_clock_khz);
  return dco_config.frequency_khz != 0;
}

}  // namespace i915
