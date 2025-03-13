// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/vout.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstdint>
#include <memory>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/amlogic-display/clock.h"
#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/dsi-host.h"
#include "src/graphics/display/drivers/amlogic-display/logging.h"
#include "src/graphics/display/drivers/amlogic-display/panel-config.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"

namespace amlogic_display {

namespace {

// List of supported features
struct supported_features_t {
  bool hpd;
};

constexpr supported_features_t kDsiSupportedFeatures = supported_features_t{
    .hpd = false,
};

constexpr supported_features_t kHdmiSupportedFeatures = supported_features_t{
    .hpd = true,
};

}  // namespace

Vout::Vout(std::unique_ptr<DsiHost> dsi_host, std::unique_ptr<Clock> dsi_clock, uint32_t width,
           uint32_t height, const PanelConfig* panel_config, inspect::Node node)
    : type_(VoutType::kDsi),
      supports_hpd_(kDsiSupportedFeatures.hpd),
      node_(std::move(node)),
      dsi_{
          .dsi_host = std::move(dsi_host),
          .clock = std::move(dsi_clock),
          .width = width,
          .height = height,
          .panel_config = *panel_config,
          .banjo_display_mode = display::ToBanjoDisplayMode(panel_config->display_timing),
      } {
  ZX_DEBUG_ASSERT(panel_config != nullptr);
  node_.RecordInt("vout_type", static_cast<int>(type()));
}

Vout::Vout(std::unique_ptr<HdmiHost> hdmi_host, inspect::Node node, uint8_t visual_debug_level)
    : type_(VoutType::kHdmi),
      supports_hpd_(kHdmiSupportedFeatures.hpd),
      node_(std::move(node)),
      hdmi_{.hdmi_host = std::move(hdmi_host)},
      visual_debug_level_(visual_debug_level) {
  node_.RecordInt("vout_type", static_cast<int>(type()));
}

zx::result<std::unique_ptr<Vout>> Vout::CreateDsiVout(fdf::Namespace& incoming, uint32_t panel_type,
                                                      uint32_t width, uint32_t height,
                                                      inspect::Node node) {
  fdf::info("Fixed panel type is {}", panel_type);
  const PanelConfig* panel_config = GetPanelConfig(panel_type);
  if (panel_config == nullptr) {
    fdf::error("Failed to get panel config for panel {}", panel_type);
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  zx::result<std::unique_ptr<DsiHost>> dsi_host_result =
      DsiHost::Create(incoming, panel_type, panel_config);
  if (dsi_host_result.is_error()) {
    fdf::error("Could not create DSI host: {}", dsi_host_result);
    return dsi_host_result.take_error();
  }
  std::unique_ptr<DsiHost> dsi_host = std::move(dsi_host_result).value();

  static constexpr char kPdevFragmentName[] = "pdev";
  zx::result<fidl::ClientEnd<fuchsia_hardware_platform_device::Device>> pdev_result =
      incoming.Connect<fuchsia_hardware_platform_device::Service::Device>(kPdevFragmentName);
  if (pdev_result.is_error()) {
    fdf::error("Failed to get the pdev client: {}", pdev_result);
    return pdev_result.take_error();
  }
  fidl::ClientEnd<fuchsia_hardware_platform_device::Device> platform_device =
      std::move(pdev_result).value();
  if (!platform_device.is_valid()) {
    fdf::error("Failed to get a valid platform device client");
    return zx::error(ZX_ERR_INTERNAL);
  }

  zx::result<std::unique_ptr<Clock>> clock_result =
      Clock::Create(platform_device, kBootloaderDisplayEnabled);
  if (clock_result.is_error()) {
    fdf::error("Could not create Clock: {}", clock_result);
    return clock_result.take_error();
  }
  std::unique_ptr<Clock> clock = std::move(clock_result).value();

  fbl::AllocChecker alloc_checker;
  std::unique_ptr<Vout> vout =
      fbl::make_unique_checked<Vout>(&alloc_checker, std::move(dsi_host), std::move(clock), width,
                                     height, panel_config, std::move(node));
  if (!alloc_checker.check()) {
    fdf::error("Failed to allocate memory for Vout.");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  return zx::ok(std::move(vout));
}

zx::result<std::unique_ptr<Vout>> Vout::CreateDsiVoutForTesting(uint32_t panel_type, uint32_t width,
                                                                uint32_t height) {
  const PanelConfig* panel_config = GetPanelConfig(panel_type);
  if (panel_config == nullptr) {
    fdf::error("Failed to get panel config for panel {}", panel_type);
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  fbl::AllocChecker alloc_checker;
  std::unique_ptr<Vout> vout = fbl::make_unique_checked<Vout>(
      &alloc_checker,
      /*dsi_host=*/nullptr, /*dsi_clock=*/nullptr, width, height, panel_config, inspect::Node{});
  if (!alloc_checker.check()) {
    fdf::error("Failed to allocate memory for Vout.");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  return zx::ok(std::move(vout));
}

zx::result<std::unique_ptr<Vout>> Vout::CreateHdmiVout(fdf::Namespace& incoming, inspect::Node node,
                                                       uint8_t visual_debug_level) {
  zx::result<std::unique_ptr<HdmiHost>> hdmi_host_result = HdmiHost::Create(incoming);
  if (hdmi_host_result.is_error()) {
    fdf::error("Could not create HDMI host: {}", hdmi_host_result);
    return hdmi_host_result.take_error();
  }

  fbl::AllocChecker alloc_checker;
  std::unique_ptr<Vout> vout = fbl::make_unique_checked<Vout>(
      &alloc_checker, std::move(hdmi_host_result).value(), std::move(node), visual_debug_level);
  if (!alloc_checker.check()) {
    fdf::error("Failed to allocate memory for Vout.");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  return zx::ok(std::move(vout));
}

raw_display_info_t Vout::CreateRawDisplayInfo(
    display::DisplayId display_id,
    cpp20::span<const fuchsia_images2_pixel_format_enum_value_t> pixel_formats) {
  switch (type_) {
    case VoutType::kDsi: {
      return raw_display_info_t{
          .display_id = display::ToBanjoDisplayId(display_id),
          .preferred_modes_list = &dsi_.banjo_display_mode,
          .preferred_modes_count = 1,
          .edid_bytes_list = nullptr,
          .edid_bytes_count = 0,
          .pixel_formats_list = pixel_formats.data(),
          .pixel_formats_count = pixel_formats.size(),
      };
    }
    case VoutType::kHdmi:
      ZX_DEBUG_ASSERT(!hdmi_.current_display_edid.is_empty());
      return raw_display_info_t{
          .display_id = display::ToBanjoDisplayId(display_id),
          .preferred_modes_list = nullptr,
          .preferred_modes_count = 0,
          .edid_bytes_list = hdmi_.current_display_edid.data(),
          .edid_bytes_count = hdmi_.current_display_edid.size(),
          .pixel_formats_list = pixel_formats.data(),
          .pixel_formats_count = pixel_formats.size(),
      };
  }
  ZX_ASSERT_MSG(false, "Invalid Vout type: %u", static_cast<uint8_t>(type_));
}

zx::result<> Vout::UpdateStateOnDisplayConnected() {
  switch (type_) {
    case VoutType::kHdmi: {
      auto read_extended_edid_result = hdmi_.hdmi_host->ReadExtendedEdid();
      if (read_extended_edid_result.is_error()) {
        // HdmiTransmitter::ReadExtendedEdid() already logs errors.
        return read_extended_edid_result.take_error();
      }
      hdmi_.current_display_edid = std::move(read_extended_edid_result).value();
      // A new connected display is not yet set up with any display timing.
      hdmi_.current_display_timing_ = {};
      return zx::ok();
    }
    case VoutType::kDsi:
      return zx::ok();
  }
  ZX_ASSERT_MSG(false, "Invalid Vout type: %u", static_cast<uint8_t>(type_));
}

void Vout::DisplayDisconnected() {
  switch (type_) {
    case VoutType::kHdmi:
      hdmi_.hdmi_host->HostOff();
      return;
    case VoutType::kDsi:
      return;
  }
  ZX_ASSERT_MSG(false, "Invalid Vout type: %u", static_cast<uint8_t>(type_));
}

zx::result<> Vout::PowerOff() {
  switch (type_) {
    case VoutType::kDsi: {
      dsi_.clock->Disable();
      dsi_.dsi_host->Disable();
      return zx::ok();
    }
    case VoutType::kHdmi: {
      hdmi_.hdmi_host->HostOff();
      return zx::ok();
    }
  }
  ZX_ASSERT_MSG(false, "Invalid Vout type: %u", static_cast<uint8_t>(type_));
}

zx::result<> Vout::PowerOn() {
  switch (type_) {
    case VoutType::kDsi: {
      zx::result<> clock_enable_result = dsi_.clock->Enable(dsi_.panel_config);
      if (!clock_enable_result.is_ok()) {
        fdf::error("Could not enable display clocks: {}", clock_enable_result);
        return clock_enable_result;
      }

      dsi_.clock->SetVideoOn(false);
      // Configure and enable DSI host interface.
      zx::result<> dsi_host_enable_result = dsi_.dsi_host->Enable(dsi_.clock->GetBitrate());
      if (!dsi_host_enable_result.is_ok()) {
        fdf::error("Could not enable DSI Host: {}", dsi_host_enable_result);
        return dsi_host_enable_result;
      }
      dsi_.clock->SetVideoOn(true);
      return zx::ok();
    }
    case VoutType::kHdmi: {
      zx::result<> hdmi_host_on_result = zx::make_result(hdmi_.hdmi_host->HostOn());
      if (!hdmi_host_on_result.is_ok()) {
        fdf::error("Could not enable HDMI host: {}", hdmi_host_on_result);
        return hdmi_host_on_result;
      }

      hdmi_.current_display_timing_ = {};
      return zx::ok();
    }
  }
  ZX_ASSERT_MSG(false, "Invalid Vout type: %u", static_cast<uint8_t>(type_));
}

zx::result<> Vout::SetFrameVisibility(bool frame_visible) {
  switch (type_) {
    case VoutType::kDsi:
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    case VoutType::kHdmi:
      // On HDMI video output, when the frames are invisible, the encoder
      // outputs a **green** background indicating that the display engine
      // front end is idle.
      static constexpr uint8_t kVisualDebugLevelInfoProduct = 1;

      // The following values are calculated using the conversion formulas defined
      // in the following standards:
      //
      // Rec. ITU-R BT.709-6, Parameter values for the HDTV1 standards for
      // production and international programme exchange, June 2015.
      // - Section 3 "Signal format", item 3.4 "Quantization of RGB, luminance
      //   and colour-difference signals", page 4.
      // - Section 3 "Signal format", item 3.5 "Derivation of luminance and
      //   colour difference signals via quantized RGB signals", page 4.

      // Black (R = 0, G = 0, B = 0) is (Y = 0, Cb = 512, Cr = 512) in YCbCr.
      static constexpr YCbCrColor kBlack = {.y = 0, .cb = 512, .cr = 512};

      // Green (R = 0, G = 128, B = 0) is (Y = 378, Cb = 339, Cr = 308) in YCbCr.
      static constexpr YCbCrColor kGreen = {.y = 378, .cb = 339, .cr = 308};

      if (visual_debug_level_ >= kVisualDebugLevelInfoProduct) {
        hdmi_.hdmi_host->ReplaceEncoderPixelColorWithColor(!frame_visible, kGreen);
      } else {
        hdmi_.hdmi_host->ReplaceEncoderPixelColorWithColor(!frame_visible, kBlack);
      }
      return zx::ok();
  }
}

bool Vout::IsDisplayTimingSupported(const display::DisplayTiming& timing) {
  ZX_DEBUG_ASSERT_MSG(type_ == VoutType::kHdmi,
                      "Vout display timing check is only supported for HDMI output.");
  return hdmi_.hdmi_host->IsDisplayTimingSupported(timing);
}

zx::result<> Vout::ApplyConfiguration(const display::DisplayTiming& timing) {
  ZX_DEBUG_ASSERT_MSG(type_ == VoutType::kHdmi,
                      "Vout display timing setup is only supported for HDMI output.");
  zx_status_t status = hdmi_.hdmi_host->ModeSet(timing);
  if (status != ZX_OK) {
    fdf::error("Failed to set HDMI display timing: {}", zx::make_result(status));
    return zx::error(status);
  }

  hdmi_.current_display_timing_ = timing;
  return zx::ok();
}

void Vout::Dump() {
  switch (type_) {
    case VoutType::kDsi: {
      LogPanelConfig(dsi_.panel_config);
      return;
    }
    case VoutType::kHdmi:
      fdf::info("horizontal_active_px = {}", hdmi_.current_display_timing_.horizontal_active_px);
      fdf::info("horizontal_front_porch_px = {}",
                hdmi_.current_display_timing_.horizontal_front_porch_px);
      fdf::info("horizontal_sync_width_px = {}",
                hdmi_.current_display_timing_.horizontal_sync_width_px);
      fdf::info("horizontal_back_porch_px = {}",
                hdmi_.current_display_timing_.horizontal_back_porch_px);
      fdf::info("vertical_active_lines = {}", hdmi_.current_display_timing_.vertical_active_lines);
      fdf::info("vertical_front_porch_lines = {}",
                hdmi_.current_display_timing_.vertical_front_porch_lines);
      fdf::info("vertical_sync_width_lines = {}",
                hdmi_.current_display_timing_.vertical_sync_width_lines);
      fdf::info("vertical_back_porch_lines = {}",
                hdmi_.current_display_timing_.vertical_back_porch_lines);
      fdf::info("pixel_clock_frequency_hz = {}",
                hdmi_.current_display_timing_.pixel_clock_frequency_hz);
      fdf::info("fields_per_frame (enum) = {}",
                static_cast<uint32_t>(hdmi_.current_display_timing_.fields_per_frame));
      fdf::info("hsync_polarity (enum) = {}",
                static_cast<uint32_t>(hdmi_.current_display_timing_.hsync_polarity));
      fdf::info("vsync_polarity (enum) = {}",
                static_cast<uint32_t>(hdmi_.current_display_timing_.vsync_polarity));
      fdf::info("vblank_alternates = {}", hdmi_.current_display_timing_.vblank_alternates);
      fdf::info("pixel_repetition = {}", hdmi_.current_display_timing_.pixel_repetition);
      return;
  }
  ZX_ASSERT_MSG(false, "Invalid Vout type: %u", static_cast<uint8_t>(type_));
}

}  // namespace amlogic_display
