// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/mcu/drivers/chromiumos-ec-core/power_sensor.h"

#include <lib/ddk/debug.h>
#include <lib/fpromise/promise.h>

#include <chromiumos-platform-ec/ec_commands.h>

#include "src/devices/mcu/drivers/chromiumos-ec-core/chromiumos_ec_core.h"
#include "src/devices/mcu/drivers/chromiumos-ec-core/subdriver.h"

namespace chromiumos_ec_core::power_sensor {

void RegisterPowerSensorDriver(ChromiumosEcCore* ec) {
  CrOsEcPowerSensorDevice* device;
  zx_status_t status = CrOsEcPowerSensorDevice::Bind(ec->zxdev(), ec, &device);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to initialise power-sensor device: %s", zx_status_get_string(status));
  }
}

zx_status_t CrOsEcPowerSensorDevice::Bind(zx_device_t* parent, ChromiumosEcCore* ec,
                                          CrOsEcPowerSensorDevice** device) {
  fbl::AllocChecker ac;
  std::unique_ptr<CrOsEcPowerSensorDevice> dev(new (&ac) CrOsEcPowerSensorDevice(ec, parent));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }

  std::array offers = {
      fuchsia_hardware_power_sensor::Service::Name,
  };

  dev->outgoing_server_end_ = std::move(endpoints->server);
  auto status = dev->DdkAdd(ddk::DeviceAddArgs("cross-ec-power-sensor")
                                .set_flags(DEVICE_ADD_MUST_ISOLATE)
                                .set_fidl_service_offers(offers)
                                .set_outgoing_dir(endpoints->client.TakeChannel()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "failed to add device: %d", status);
    return status;
  }

  [[maybe_unused]] auto* placeholder1 = dev.release();

  return ZX_OK;
}

void CrOsEcPowerSensorDevice::DdkInit(ddk::InitTxn txn) {
  auto promise = UpdateState().then(
      [txn = std::move(txn)](fpromise::result<void, zx_status_t>& result) mutable {
        if (result.is_ok()) {
          txn.Reply(ZX_OK);
        } else {
          txn.Reply(result.take_error());
        }
      });

  ec_->executor().schedule_task(std::move(promise));
}

fpromise::promise<void, zx_status_t> CrOsEcPowerSensorDevice::UpdateState() {
  if (ec_->IsBoard(kAtlasBoardName)) {
    ec_params_adc_read request = {
        .adc_channel = kAtlasAdcPsysChannel,
    };

    return ec_->IssueCommand(EC_CMD_ADC_READ, 0, request)
        .and_then([this](CommandResult& result) -> fpromise::result<void, zx_status_t> {
          auto response = result.GetData<ec_response_adc_read>();

          auto new_power = static_cast<float>(response->adc_value) / 1'000'000;
          if (new_power <= 0) {
            zxlogf(ERROR, "EC returned negative or zero power usage");
            return fpromise::error(ZX_ERR_INTERNAL);
          }
          power_ = new_power;

          return fpromise::ok();
        });
  }

  return fpromise::make_error_promise(ZX_ERR_NOT_SUPPORTED);
}

void CrOsEcPowerSensorDevice::GetPowerWatts(GetPowerWattsCompleter::Sync& completer) {
  ec_->executor().schedule_task(UpdateState().then(
      [this, completer = completer.ToAsync()](fpromise::result<void, zx_status_t>& result) mutable {
        if (result.is_error()) {
          completer.ReplyError(result.take_error());
          return;
        }

        completer.ReplySuccess(power_);
      }));
}

void CrOsEcPowerSensorDevice::GetVoltageVolts(GetVoltageVoltsCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void CrOsEcPowerSensorDevice::DdkRelease() { delete this; }

}  // namespace chromiumos_ec_core::power_sensor
