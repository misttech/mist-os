// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/lcd.h"

#include <lib/ddk/debug.h>
#include <lib/device-protocol/display-panel.h>
#include <lib/mipi-dsi/mipi-dsi.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <ddktl/device.h>
#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/panel-config.h"

namespace amlogic_display {

namespace {

zx_status_t CheckDsiDeviceRegister(ddk::DsiImplProtocolClient* dsiimpl, uint8_t reg, size_t count) {
  ZX_DEBUG_ASSERT(count > 0);

  const uint8_t payload = reg;

  // TODO(https://fxbug.dev/42085293): Replace the VLA with a fixed-size
  // array with size limits.
  uint8_t response[count];

  mipi_dsi_cmd_t cmd{
      .virt_chn_id = kMipiDsiVirtualChanId,
      .dsi_data_type = kMipiDsiDtGenShortRead1,
      .pld_data_list = &payload,
      .pld_data_count = 1,
      .rsp_data_list = response,
      .rsp_data_count = count,
      .flags = MIPI_DSI_CMD_FLAGS_ACK | MIPI_DSI_CMD_FLAGS_SET_MAX,
  };

  zx_status_t status = dsiimpl->SendCmd(&cmd, 1);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not read register %d", reg);
    return status;
  }
  return ZX_OK;
}

// Reads the display hardware ID from the MIPI-DSI interface.
//
// `dsiimpl` must be configured in DSI command mode.
zx::result<uint32_t> GetMipiDsiDisplayId(ddk::DsiImplProtocolClient dsiimpl) {
  // TODO(https://fxbug.dev/322450952): The Read Display Identification
  // Information (0x04) command is not guaranteed to be available on all
  // display driver ICs. The response size and the actual meaning of the
  // response may vary, depending on the DDIC models. Do not hardcode the
  // command address and the response size.
  //
  // The following command address and response size are specified on:
  // - JD9364 datasheet, Section 10.2.3 "RDDIDIF", page 146
  // - JD9365D datasheet, Section 10.2.3 "RDDIDIF", page 130
  // - ST7703I datasheet, Section 6.2.3 "Read Display ID", page 81
  // - NT35596 datasheet, Section 6.1 "User Command Set (Command 1)", page 158
  constexpr uint8_t kCommandReadDisplayIdentificationInformation = 0x04;
  constexpr int kCommandReadDisplayIdentificationInformationResponseSize = 3;

  const std::array<uint8_t, 1> payload = {
      kCommandReadDisplayIdentificationInformation,
  };
  std::array<uint8_t, kCommandReadDisplayIdentificationInformationResponseSize> response;
  // Create the command using mipi-dsi library
  mipi_dsi_cmd_t cmd;

  zx_status_t status = mipi_dsi::MipiDsi::CreateCommand(
      payload.data(), payload.size(), response.data(), response.size(), /*is_dcs=*/false, &cmd);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to create Read Display Identification Information command: %s",
           zx_status_get_string(status));
    return zx::error(status);
  }

  status = dsiimpl.SendCmd(&cmd, 1);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to read out Display ID: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  const uint32_t display_id = response[0] << 16 | response[1] << 8 | response[2];
  return zx::ok(display_id);
}

}  // namespace

// This function write DSI commands based on the input buffer.
zx::result<> Lcd::PerformDisplayInitCommandSequence(cpp20::span<const uint8_t> encoded_commands) {
  zx_status_t status = ZX_OK;
  uint32_t delay_ms = 0;
  constexpr size_t kMinCmdSize = 2;

  for (size_t i = 0; i < encoded_commands.size() - kMinCmdSize;) {
    const uint8_t cmd_type = encoded_commands[i];
    const uint8_t payload_size = encoded_commands[i + 1];
    bool is_dcs = false;
    // This command has an implicit size=2, treat it specially.
    if (cmd_type == kDsiOpSleep) {
      if (payload_size == 0 || payload_size == 0xff) {
        return zx::make_result(status);
      }
      zx::nanosleep(zx::deadline_after(zx::msec(payload_size)));
      i += 2;
      continue;
    }
    if (payload_size == 0) {
      i += kMinCmdSize;
      continue;
    }
    if ((i + payload_size + kMinCmdSize) > encoded_commands.size()) {
      zxlogf(ERROR, "buffer[%lu] command 0x%x size=0x%x would overflow buffer size=%lu", i,
             cmd_type, payload_size, encoded_commands.size());
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }

    switch (cmd_type) {
      case kDsiOpDelay:
        delay_ms = 0;
        for (size_t j = 0; j < payload_size; j++) {
          delay_ms += encoded_commands[i + 2 + j];
        }
        if (delay_ms > 0) {
          zx::nanosleep(zx::deadline_after(zx::msec(delay_ms)));
        }
        break;
      case kDsiOpGpio:
        zxlogf(TRACE, "dsi_set_gpio size=%d value=%d", payload_size, encoded_commands[i + 3]);
        if (encoded_commands[i + 2] != 0) {
          zxlogf(ERROR, "Unrecognized GPIO pin (%d)", encoded_commands[i + 2]);
          // We _should_ bail here, but this spec-violating behavior is present
          // in the other drivers for this hardware.
          //
          // return ZX_ERR_UNKNOWN;
        } else {
          fidl::WireResult result = gpio_->ConfigOut(encoded_commands[i + 3]);
          if (!result.ok()) {
            zxlogf(ERROR, "Failed to send ConfigOut request to gpio: %s", result.status_string());
            return zx::error(result.status());
          }
          if (result->is_error()) {
            zxlogf(ERROR, "Failed to configure gpio to output: %s",
                   zx_status_get_string(result->error_value()));
            return result->take_error();
          }
        }
        if (payload_size > 2 && encoded_commands[i + 4]) {
          zxlogf(TRACE, "dsi_set_gpio sleep %d", encoded_commands[i + 4]);
          zx::nanosleep(zx::deadline_after(zx::msec(encoded_commands[i + 4])));
        }
        break;
      case kDsiOpReadReg:
        zxlogf(TRACE, "dsi_read size=%d reg=%d, count=%d", payload_size, encoded_commands[i + 2],
               encoded_commands[i + 3]);
        if (payload_size != 2) {
          zxlogf(ERROR, "Invalid MIPI-DSI reg check, expected a register and a target value!");
        }
        status = CheckDsiDeviceRegister(&dsiimpl_, /*reg=*/encoded_commands[i + 2],
                                        /*count=*/encoded_commands[i + 3]);
        if (status != ZX_OK) {
          zxlogf(ERROR, "Error reading MIPI register 0x%x: %s", encoded_commands[i + 2],
                 zx_status_get_string(status));
          return zx::error(status);
        }
        break;
      case kDsiOpPhyPowerOn:
        zxlogf(TRACE, "dsi_phy_power_on size=%d", payload_size);
        set_signal_power_(/*on=*/true);
        break;
      case kDsiOpPhyPowerOff:
        zxlogf(TRACE, "dsi_phy_power_off size=%d", payload_size);
        set_signal_power_(/*on=*/false);
        break;
      // All other cmd_type bytes are real DSI commands
      case kMipiDsiDtDcsShortWrite0:
      case kMipiDsiDtDcsShortWrite1:
      case kMipiDsiDtDcsLongWrite:
        is_dcs = true;
        [[fallthrough]];
      case kMipiDsiDtGenShortWrite0:
      case kMipiDsiDtGenShortWrite1:
      case kMipiDsiDtGenShortWrite2:
      case kMipiDsiDtGenLongWrite:
        zxlogf(TRACE, "DSI command type: 0x%02x payload size: %d", cmd_type, payload_size);
        // Create the command using mipi-dsi library
        mipi_dsi_cmd_t cmd;
        status = mipi_dsi::MipiDsi::CreateCommand(&encoded_commands[i + 2], payload_size, NULL, 0,
                                                  is_dcs, &cmd);
        ZX_ASSERT_MSG(status == ZX_OK, "Failed to create the MIPI-DSI command for command type %d",
                      cmd_type);

        status = dsiimpl_.SendCmd(&cmd, 1);
        if (status != ZX_OK) {
          zxlogf(ERROR, "Failed to send command to the MIPI-DSI peripheral: %s",
                 zx_status_get_string(status));
          return zx::error(status);
        }
        break;
      case kMipiDsiDtDcsRead0:
      case kMipiDsiDtGenShortRead0:
      case kMipiDsiDtGenShortRead1:
      case kMipiDsiDtGenShortRead2:
        // TODO(https://fxbug.dev/322438328): Support MIPI-DSI read commands.
        zxlogf(ERROR, "MIPI-DSI read command 0x%02x is not supported", cmd_type);
        return zx::error(ZX_ERR_NOT_SUPPORTED);
      default:
        zxlogf(ERROR, "MIPI-DSI / panel initialization command 0x%02x is not supported", cmd_type);
        return zx::error(ZX_ERR_NOT_SUPPORTED);
    }
    // increment by payload length
    i += payload_size + kMinCmdSize;
  }
  return zx::make_result(status);
}

zx::result<> Lcd::Disable() {
  if (!enabled_) {
    zxlogf(INFO, "LCD is already off, no work to do");
    return zx::ok();
  }
  if (dsi_off_.size() == 0) {
    zxlogf(ERROR, "Unsupported panel (%d) detected!", panel_type_);
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  zxlogf(INFO, "Powering off the LCD [type=%d]", panel_type_);
  zx::result<> power_off_result = PerformDisplayInitCommandSequence(dsi_off_);
  if (!power_off_result.is_ok()) {
    zxlogf(ERROR, "Failed to decode and execute panel off sequence: %s",
           power_off_result.status_string());
    return power_off_result.take_error();
  }
  enabled_ = false;
  return zx::ok();
}

zx::result<> Lcd::Enable() {
  if (enabled_) {
    zxlogf(INFO, "LCD is already on, no work to do");
    return zx::ok();
  }

  if (dsi_on_.size() == 0) {
    zxlogf(ERROR, "Unsupported panel (%d) detected!", panel_type_);
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  zxlogf(INFO, "Powering on the LCD [type=%d]", panel_type_);
  zx::result<> power_on_result = PerformDisplayInitCommandSequence(dsi_on_);
  if (!power_on_result.is_ok()) {
    zxlogf(ERROR, "Failed to decode and execute panel init sequence: %s",
           power_on_result.status_string());
    return power_on_result.take_error();
  }

  // Check LCD initialization status by reading the display hardware ID.
  zx::result<uint32_t> display_id_result = GetMipiDsiDisplayId(dsiimpl_);
  if (!display_id_result.is_ok()) {
    zxlogf(ERROR, "Failed to communicate with LCD Panel to get the display hardware ID: %s",
           display_id_result.status_string());
    return display_id_result.take_error();
  }
  zxlogf(INFO, "LCD MIPI DSI display hardware ID: 0x%08x", display_id_result.value());
  zx_nanosleep(zx_deadline_after(ZX_USEC(10)));

  // LCD is on now.
  enabled_ = true;
  return zx::ok();
}

// static
zx::result<std::unique_ptr<Lcd>> Lcd::Create(uint32_t panel_type, cpp20::span<const uint8_t> dsi_on,
                                             cpp20::span<const uint8_t> dsi_off,
                                             fit::function<void(bool)> set_signal_power,
                                             ddk::DsiImplProtocolClient dsiimpl,
                                             fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> gpio,
                                             bool already_enabled) {
  fbl::AllocChecker alloc_checker;
  std::unique_ptr<Lcd> lcd =
      fbl::make_unique_checked<Lcd>(&alloc_checker, panel_type, std::move(set_signal_power));
  if (!alloc_checker.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  lcd->dsi_on_ = dsi_on;
  lcd->dsi_off_ = dsi_off;
  lcd->dsiimpl_ = dsiimpl;

  if (!gpio.is_valid()) {
    zxlogf(ERROR, "Could not obtain GPIO protocol");
    return zx::error(ZX_ERR_NO_RESOURCES);
  }
  lcd->gpio_.Bind(std::move(gpio));

  lcd->enabled_ = already_enabled;

  return zx::ok(std::move(lcd));
}

}  // namespace amlogic_display
