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

#define READ_DISPLAY_ID_CMD (0x04)
#define READ_DISPLAY_ID_LEN (0x03)

namespace amlogic_display {

namespace {

static zx_status_t CheckMipiReg(ddk::DsiImplProtocolClient* dsiimpl, uint8_t reg, size_t count) {
  ZX_DEBUG_ASSERT(count > 0);

  const uint8_t payload[3] = {kMipiDsiDtGenShortRead1, 1, reg};
  uint8_t rsp[count];
  mipi_dsi_cmd_t cmd{
      .virt_chn_id = kMipiDsiVirtualChanId,
      .dsi_data_type = kMipiDsiDtGenShortRead1,
      .pld_data_list = payload,
      .pld_data_count = 1,
      .rsp_data_list = rsp,
      .rsp_data_count = count,
      .flags = MIPI_DSI_CMD_FLAGS_ACK | MIPI_DSI_CMD_FLAGS_SET_MAX,
  };

  zx_status_t status;
  if ((status = dsiimpl->SendCmd(&cmd, 1)) != ZX_OK) {
    zxlogf(ERROR, "Could not read register %c", reg);
    return status;
  }
  if (cmd.rsp_data_count != count) {
    zxlogf(ERROR, "MIPI-DSI register read was short: got %zu want %zu. Ignoring",
           cmd.rsp_data_count, count);
  }
  return ZX_OK;
}

// Reads the display hardware ID from the MIPI-DSI interface.
//
// `dsiimpl` must be configured in DSI command mode.
zx::result<uint32_t> GetMipiDsiDisplayId(ddk::DsiImplProtocolClient dsiimpl) {
  uint8_t txcmd = READ_DISPLAY_ID_CMD;
  uint8_t response[READ_DISPLAY_ID_LEN];
  zx_status_t status = ZX_OK;
  // Create the command using mipi-dsi library
  mipi_dsi_cmd_t cmd;

  status =
      mipi_dsi::MipiDsi::CreateCommand(&txcmd, 1, response, READ_DISPLAY_ID_LEN, COMMAND_GEN, &cmd);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to create READ_DISPLAY_ID command: %s", zx_status_get_string(status));
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
        status = CheckMipiReg(&dsiimpl_, /*reg=*/encoded_commands[i + 2],
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
      case /*kMipiDsiDtDcsShortWrite2*/ 0x25:
      case kMipiDsiDtDcsLongWrite:
      case kMipiDsiDtDcsRead0:
        is_dcs = true;
        __FALLTHROUGH;
      default:
        zxlogf(TRACE, "dsi_cmd op=0x%x size=%d is_dcs=%s", cmd_type, payload_size,
               is_dcs ? "yes" : "no");
        ZX_DEBUG_ASSERT(cmd_type != 0x37);
        // Create the command using mipi-dsi library
        mipi_dsi_cmd_t cmd;
        status = mipi_dsi::MipiDsi::CreateCommand(&encoded_commands[i + 2], payload_size, NULL, 0,
                                                  is_dcs, &cmd);
        if (status == ZX_OK) {
          if ((status = dsiimpl_.SendCmd(&cmd, 1)) != ZX_OK) {
            zxlogf(ERROR, "Error loading LCD init table. Aborting: %s",
                   zx_status_get_string(status));
            return zx::error(status);
          }
        } else {
          zxlogf(ERROR, "Invalid command at byte 0x%lx (%s). Skipping", i,
                 zx_status_get_string(status));
        }
        break;
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
