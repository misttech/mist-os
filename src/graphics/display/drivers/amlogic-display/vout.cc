// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/vout.h"

#include <fidl/fuchsia.images2/cpp/wire.h>
#include <lib/device-protocol/display-panel.h>
#include <zircon/status.h>

#include <ddktl/device.h>
#include <ddktl/fidl.h>
#include <fbl/alloc_checker.h>

#include "src/graphics/display/lib/api-types-cpp/display-id.h"

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

zx::result<display_setting_t> GetDisplaySettingForPanel(uint32_t panel_type) {
  switch (panel_type) {
    case PANEL_TV070WSM_FT:
    case PANEL_TV070WSM_FT_9365:
      return zx::ok(kDisplaySettingTV070WSM_FT);
    case PANEL_P070ACB_FT:
      return zx::ok(kDisplaySettingP070ACB_FT);
    case PANEL_KD070D82_FT_9365:
    case PANEL_KD070D82_FT:
      return zx::ok(kDisplaySettingKD070D82_FT);
    case PANEL_TV101WXM_FT_9365:
    case PANEL_TV101WXM_FT:
      return zx::ok(kDisplaySettingTV101WXM_FT);
    case PANEL_G101B158_FT:
      return zx::ok(kDisplaySettingG101B158_FT);
    case PANEL_TV080WXM_FT:
      return zx::ok(kDisplaySettingTV080WXM_FT);
    case PANEL_TV070WSM_ST7703I:
      return zx::ok(kDisplaySettingTV070WSM_ST7703I);
    case PANEL_MTF050FHDI_03:
      return zx::ok(kDisplaySettingMTF050FHDI_03);
    default:
      zxlogf(ERROR, "Unsupported panel detected!");
      return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
}

}  // namespace

zx::result<> Vout::InitDsi(zx_device_t* parent, uint32_t panel_type, uint32_t width,
                           uint32_t height) {
  type_ = VoutType::kDsi;

  supports_hpd_ = kDsiSupportedFeatures.hpd;

  dsi_.width = width;
  dsi_.height = height;

  auto dsi_host = DsiHost::Create(parent, panel_type);
  if (dsi_host.is_error()) {
    zxlogf(ERROR, "Could not create DSI host: %s", dsi_host.status_string());
    return dsi_host.take_error();
  }
  dsi_.dsi_host = std::move(dsi_host.value());
  ZX_ASSERT(dsi_.dsi_host);

  ddk::PDevFidl pdev;
  zx_status_t status = ddk::PDevFidl::FromFragment(parent, &pdev);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not get PDEV protocol");
    return zx::error(status);
  }
  auto clock = Clock::Create(pdev, kBootloaderDisplayEnabled);
  if (clock.is_error()) {
    zxlogf(ERROR, "Could not create Clock: %s", clock.status_string());
    return clock.take_error();
  }

  dsi_.clock = std::move(clock.value());
  ZX_ASSERT(dsi_.clock);

  zxlogf(INFO, "Fixed panel type is %d", dsi_.dsi_host->panel_type());
  zx::result display_setting = GetDisplaySettingForPanel(dsi_.dsi_host->panel_type());
  if (display_setting.is_error()) {
    return display_setting.take_error();
  }
  dsi_.disp_setting = display_setting.value();
  return zx::ok();
}

zx::result<> Vout::InitDsiForTesting(uint32_t panel_type, uint32_t width, uint32_t height) {
  type_ = VoutType::kDsi;

  supports_hpd_ = kDsiSupportedFeatures.hpd;

  dsi_.width = width;
  dsi_.height = height;

  zx::result display_setting = GetDisplaySettingForPanel(panel_type);
  if (display_setting.is_error()) {
    return display_setting.take_error();
  }
  dsi_.disp_setting = display_setting.value();
  return zx::ok();
}

zx::result<> Vout::InitHdmi(zx_device_t* parent,
                            fidl::ClientEnd<fuchsia_hardware_hdmi::Hdmi> hdmi) {
  type_ = VoutType::kHdmi;

  supports_hpd_ = kHdmiSupportedFeatures.hpd;

  fbl::AllocChecker ac;
  hdmi_.hdmi_host = fbl::make_unique_checked<HdmiHost>(&ac, parent, std::move(hdmi));
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  if (zx_status_t status = hdmi_.hdmi_host->Init(); status != ZX_OK) {
    zxlogf(ERROR, "Could not initialize HDMI host: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

void Vout::PopulateAddedDisplayArgs(
    added_display_args_t* args, display::DisplayId display_id,
    cpp20::span<const fuchsia_images2_pixel_format_enum_value_t> pixel_formats) {
  switch (type_) {
    case VoutType::kDsi:
      args->display_id = display::ToBanjoDisplayId(display_id);
      args->edid_present = false;
      args->panel.params.height = dsi_.height;
      args->panel.params.width = dsi_.width;
      args->panel.params.refresh_rate_e2 = 6000;  // Just guess that it's 60fps
      args->pixel_format_list = pixel_formats.data();
      args->pixel_format_count = pixel_formats.size();
      args->cursor_info_count = 0;
      break;
    case VoutType::kHdmi:
      args->display_id = display::ToBanjoDisplayId(display_id);
      args->edid_present = true;
      args->panel.i2c.ops = &i2c_impl_protocol_ops_;
      args->panel.i2c.ctx = this;
      args->pixel_format_list = pixel_formats.data();
      args->pixel_format_count = pixel_formats.size();
      args->cursor_info_count = 0;
      break;
    default:
      zxlogf(ERROR, "Unrecognized vout type %u", type_);
      return;
  }
}

void Vout::DisplayConnected() {
  switch (type_) {
    case kHdmi:
      hdmi_.cur_display_mode_ = {};
      break;
    default:
      break;
  }
}

void Vout::DisplayDisconnected() {
  switch (type_) {
    case kHdmi:
      hdmi_.hdmi_host->HostOff();
      break;
    default:
      break;
  }
}

zx::result<> Vout::PowerOff() {
  switch (type_) {
    case VoutType::kDsi: {
      dsi_.clock->Disable();
      dsi_.dsi_host->Disable(dsi_.disp_setting);
      return zx::ok();
    }
    case VoutType::kHdmi: {
      hdmi_.hdmi_host->HostOff();
      return zx::ok();
    }
    case VoutType::kUnknown:
      break;
  }
  zxlogf(ERROR, "Unrecognized Vout type %u", type_);
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> Vout::PowerOn() {
  switch (type_) {
    case VoutType::kDsi: {
      zx::result<> clock_enable_result = dsi_.clock->Enable(dsi_.disp_setting);
      if (!clock_enable_result.is_ok()) {
        zxlogf(ERROR, "Could not enable display clocks: %s", clock_enable_result.status_string());
        return clock_enable_result;
      }

      dsi_.clock->SetVideoOn(false);
      // Configure and enable DSI host interface.
      zx::result<> dsi_host_enable_result =
          dsi_.dsi_host->Enable(dsi_.disp_setting, dsi_.clock->GetBitrate());
      if (!dsi_host_enable_result.is_ok()) {
        zxlogf(ERROR, "Could not enable DSI Host: %s", dsi_host_enable_result.status_string());
        return dsi_host_enable_result;
      }
      dsi_.clock->SetVideoOn(true);
      return zx::ok();
    }
    case VoutType::kHdmi: {
      zx::result<> hdmi_host_on_result = zx::make_result(hdmi_.hdmi_host->HostOn());
      if (!hdmi_host_on_result.is_ok()) {
        zxlogf(ERROR, "Could not enable HDMI host: %s", hdmi_host_on_result.status_string());
        return hdmi_host_on_result;
      }

      // After HDMI host is on, the display timings is reset and the driver
      // must perform modeset again.
      // This clears the previously set display mode to force an HDMI modeset
      // on the next ApplyConfiguration().
      hdmi_.cur_display_mode_ = {};
      return zx::ok();
    }
    case VoutType::kUnknown:
      break;
  }
  zxlogf(ERROR, "Unrecognized Vout type %u", type_);
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

bool Vout::CheckMode(const display_mode_t* mode) {
  switch (type_) {
    case kDsi:
      return false;
    case kHdmi:
      return memcmp(&hdmi_.cur_display_mode_, mode, sizeof(display_mode_t)) &&
             (hdmi_.hdmi_host->GetVic(mode) != ZX_OK);
    default:
      return false;
  }
}

zx::result<> Vout::ApplyConfiguration(const display_mode_t* mode) {
  switch (type_) {
    case kDsi:
      return zx::ok();
    case kHdmi: {
      if (!memcmp(&hdmi_.cur_display_mode_, mode, sizeof(display_mode_t))) {
        // No new configs
        return zx::ok();
      }

      display_mode_t modified_mode;
      memcpy(&modified_mode, mode, sizeof(display_mode_t));
      zx_status_t status = hdmi_.hdmi_host->GetVic(&modified_mode);
      if (status != ZX_OK) {
        zxlogf(ERROR, "Failed to get video clock for current HDMI display mode: %s",
               zx_status_get_string(status));
        return zx::error(status);
      }

      memcpy(&hdmi_.cur_display_mode_, mode, sizeof(display_mode_t));
      // FIXME: Need documentation for HDMI PLL initialization
      hdmi_.hdmi_host->ConfigurePll();
      hdmi_.hdmi_host->ModeSet(modified_mode);
      return zx::ok();
    }
    default:
      return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
}

zx::result<> Vout::OnDisplaysChanged(added_display_info_t& info) {
  switch (type_) {
    case kDsi:
      return zx::ok();
    case kHdmi:
      hdmi_.hdmi_host->UpdateOutputColorFormat(
          info.is_standard_srgb_out ? fuchsia_hardware_hdmi::wire::ColorFormat::kCfRgb
                                    : fuchsia_hardware_hdmi::wire::ColorFormat::kCf444);
      return zx::ok();
    default:
      return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
}

zx_status_t Vout::I2cImplTransact(const i2c_impl_op_t* op_list, size_t op_count) {
  switch (type_) {
    case kHdmi:
      return hdmi_.hdmi_host->EdidTransfer(op_list, op_count);
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
}

void Vout::Dump() {
  switch (type_) {
    case VoutType::kDsi:
      zxlogf(INFO, "#############################");
      zxlogf(INFO, "Dumping disp_setting structure:");
      zxlogf(INFO, "#############################");
      zxlogf(INFO, "h_active = 0x%x (%u)", dsi_.disp_setting.h_active, dsi_.disp_setting.h_active);
      zxlogf(INFO, "v_active = 0x%x (%u)", dsi_.disp_setting.v_active, dsi_.disp_setting.v_active);
      zxlogf(INFO, "h_period = 0x%x (%u)", dsi_.disp_setting.h_period, dsi_.disp_setting.h_period);
      zxlogf(INFO, "v_period = 0x%x (%u)", dsi_.disp_setting.v_period, dsi_.disp_setting.v_period);
      zxlogf(INFO, "hsync_width = 0x%x (%u)", dsi_.disp_setting.hsync_width,
             dsi_.disp_setting.hsync_width);
      zxlogf(INFO, "hsync_bp = 0x%x (%u)", dsi_.disp_setting.hsync_bp, dsi_.disp_setting.hsync_bp);
      zxlogf(INFO, "hsync_pol = 0x%x (%u)", dsi_.disp_setting.hsync_pol,
             dsi_.disp_setting.hsync_pol);
      zxlogf(INFO, "vsync_width = 0x%x (%u)", dsi_.disp_setting.vsync_width,
             dsi_.disp_setting.vsync_width);
      zxlogf(INFO, "vsync_bp = 0x%x (%u)", dsi_.disp_setting.vsync_bp, dsi_.disp_setting.vsync_bp);
      zxlogf(INFO, "vsync_pol = 0x%x (%u)", dsi_.disp_setting.vsync_pol,
             dsi_.disp_setting.vsync_pol);
      zxlogf(INFO, "lcd_clock = 0x%x (%u)", dsi_.disp_setting.lcd_clock,
             dsi_.disp_setting.lcd_clock);
      zxlogf(INFO, "lane_num = 0x%x (%u)", dsi_.disp_setting.lane_num, dsi_.disp_setting.lane_num);
      zxlogf(INFO, "bit_rate_max = 0x%x (%u)", dsi_.disp_setting.bit_rate_max,
             dsi_.disp_setting.bit_rate_max);
      zxlogf(INFO, "clock_factor = 0x%x (%u)", dsi_.disp_setting.clock_factor,
             dsi_.disp_setting.clock_factor);
      break;
    case VoutType::kHdmi:
      zxlogf(INFO, "pixel_clock_10khz = 0x%x (%u)", hdmi_.cur_display_mode_.pixel_clock_10khz,
             hdmi_.cur_display_mode_.pixel_clock_10khz);
      zxlogf(INFO, "h_addressable = 0x%x (%u)", hdmi_.cur_display_mode_.h_addressable,
             hdmi_.cur_display_mode_.h_addressable);
      zxlogf(INFO, "h_front_porch = 0x%x (%u)", hdmi_.cur_display_mode_.h_front_porch,
             hdmi_.cur_display_mode_.h_front_porch);
      zxlogf(INFO, "h_sync_pulse = 0x%x (%u)", hdmi_.cur_display_mode_.h_sync_pulse,
             hdmi_.cur_display_mode_.h_sync_pulse);
      zxlogf(INFO, "h_blanking = 0x%x (%u)", hdmi_.cur_display_mode_.h_blanking,
             hdmi_.cur_display_mode_.h_blanking);
      zxlogf(INFO, "v_addressable = 0x%x (%u)", hdmi_.cur_display_mode_.v_addressable,
             hdmi_.cur_display_mode_.v_addressable);
      zxlogf(INFO, "v_front_porch = 0x%x (%u)", hdmi_.cur_display_mode_.v_front_porch,
             hdmi_.cur_display_mode_.v_front_porch);
      zxlogf(INFO, "v_sync_pulse = 0x%x (%u)", hdmi_.cur_display_mode_.v_sync_pulse,
             hdmi_.cur_display_mode_.v_sync_pulse);
      zxlogf(INFO, "v_blanking = 0x%x (%u)", hdmi_.cur_display_mode_.v_blanking,
             hdmi_.cur_display_mode_.v_blanking);
      zxlogf(INFO, "flags = 0x%x (%u)", hdmi_.cur_display_mode_.flags,
             hdmi_.cur_display_mode_.flags);
      break;
    default:
      zxlogf(ERROR, "Unrecognized Vout type %u", type_);
  }
}

}  // namespace amlogic_display
