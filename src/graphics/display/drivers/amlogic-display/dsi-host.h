// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_DSI_HOST_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_DSI_HOST_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fuchsia/hardware/dsiimpl/cpp/banjo.h>
#include <lib/fit/function.h>
#include <lib/zx/result.h>
#include <unistd.h>
#include <zircon/compiler.h>

#include <memory>
#include <optional>

#include <ddktl/device.h>
#include <hwreg/mmio.h>

#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/hhi-regs.h"
#include "src/graphics/display/drivers/amlogic-display/lcd.h"
#include "src/graphics/display/drivers/amlogic-display/mipi-phy.h"
#include "src/graphics/display/drivers/amlogic-display/panel-config.h"
#include "src/graphics/display/drivers/amlogic-display/vpu-regs.h"
#include "src/graphics/display/lib/designware-dsi/dsi-host-controller.h"

namespace amlogic_display {

class DsiHost {
 public:
  // Factory method intended for production use.
  //
  // This method doesn't modify hardware state in any way, and is thus safe to
  // use when adopting a device that was initialized by the bootloader.
  //
  // `parent` provides all resources needed to drive the DSI host hardware.
  // `panel_config` must be non-null and must outlive the `DsiHost` instance.
  //
  // Returns a non-null pointer to the DsiHost instance on success.
  static zx::result<std::unique_ptr<DsiHost>> Create(zx_device_t* parent, uint32_t panel_type,
                                                     const PanelConfig* panel_config);

  // Production code should prefer using the `Create()` factory method.
  //
  // `panel_config` must be non-null and must outlive the `DsiHost` instance.
  // `mipi_dsi_top_mmio` is the MIPI-DSI top-level control MMIO register
  // region. It must be a valid MMIO buffer.
  // `hhi_mmio` is the HHI (HIU) MMIO register region. It must be a valid
  // MMIO buffer.
  // `lcd_reset_gpio` is the LCD RESET GPIO pin and must be valid.
  // `designware_dsi_host_controller` must not be nullptr.
  // `lcd` must not be nullptr. It may depend on `designware_dsi_host_controller`.
  // `phy` must not be nullptr. It may depend on `designware_dsi_host_controller`.
  explicit DsiHost(
      uint32_t panel_type, const PanelConfig* panel_config, fdf::MmioBuffer mipi_dsi_top_mmio,
      fdf::MmioBuffer hhi_mmio, fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> lcd_reset_gpio,
      std::unique_ptr<designware_dsi::DsiHostController> designware_dsi_host_controller,
      std::unique_ptr<Lcd> lcd, std::unique_ptr<MipiPhy> phy, bool enabled);

  ~DsiHost() = default;

  DsiHost(const DsiHost&) = delete;
  DsiHost& operator=(const DsiHost&) = delete;
  DsiHost(DsiHost&&) = delete;
  DsiHost& operator=(DsiHost&&) = delete;

  // This function sets up mipi dsi interface. It includes both DWC and AmLogic blocks
  // The DesignWare setup could technically be moved to the dw_mipi_dsi driver. However,
  // given the highly configurable nature of this block, we'd have to provide a lot of
  // information to the generic driver. Therefore, it's just simpler to configure it here
  //
  // `dphy_data_lane_bits_per_second` is the bit transmission rate on each
  // D-PHY data lane.
  zx::result<> Enable(int64_t dphy_data_lane_bits_per_second);

  // This function will turn off DSI Host. It is a "best-effort" function. We will attempt
  // to shutdown whatever we can. Error during shutdown path is ignored and function proceeds
  // with shutting down.
  void Disable();
  void Dump();

  uint32_t panel_type() const { return panel_type_; }

 private:
  void PhyEnable();
  void PhyDisable();

  // Configures the MIPI DSI Host controller (transmitter) hardware for video
  // data transmission.
  //
  // `dphy_data_lane_bits_per_second` is the bit transmission rate on each
  // D-PHY data lane.
  zx::result<> ConfigureDsiHostController(int64_t dphy_data_lane_bits_per_second);

  // Performs the Amlogic-specific power operation sequence.
  //
  // The Amlogic-specific power operations are defined in the Amlogic MIPI DSI
  // Panel Tuning User Guide, Version 0.1 (Google internal), Section 2.4.10
  // "Power on/off step", pages 16-17.
  //
  // `power_on` is called for each command of type Signal.
  zx::result<> PerformPowerOpSequence(cpp20::span<const PowerOp> power_ops,
                                      fit::callback<zx::result<>()> power_on);

  fdf::MmioBuffer mipi_dsi_top_mmio_;
  fdf::MmioBuffer hhi_mmio_;

  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> lcd_reset_gpio_;

  uint32_t panel_type_;
  const PanelConfig& panel_config_;

  bool enabled_ = false;

  // Must outlive `lcd_` and `phy_`.
  std::unique_ptr<designware_dsi::DsiHostController> designware_dsi_host_controller_;

  std::unique_ptr<Lcd> lcd_;
  std::unique_ptr<MipiPhy> phy_;
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_DSI_HOST_H_
