// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/aml-pwm-regulator/aml-pwm-regulator.h"

#include <lib/ddk/metadata.h>
#include <lib/fdf/dispatcher.h>

#include "src/devices/power/drivers/aml-pwm-regulator/aml-pwm-regulator-bind.h"

namespace aml_pwm_regulator {

void AmlPwmRegulator::SetVoltageStep(SetVoltageStepRequestView request,
                                     SetVoltageStepCompleter::Sync& completer) {
  if (request->step >= num_steps_) {
    zxlogf(ERROR, "Requested step (%u) is larger than allowed (total number of steps %u).",
           request->step, num_steps_);
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (request->step == current_step_) {
    completer.ReplySuccess();
    return;
  }

  aml_pwm::mode_config on = {aml_pwm::ON, {}};
  pwm_config_t cfg = {
      false, period_ns_,
      static_cast<float>((num_steps_ - 1 - request->step) * 100.0 / ((num_steps_ - 1) * 1.0)),
      reinterpret_cast<uint8_t*>(&on), sizeof(on)};
  auto status = pwm_.SetConfig(&cfg);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Unable to configure PWM. %d", status);
    completer.ReplyError(status);
    return;
  }
  current_step_ = request->step;

  completer.ReplySuccess();
}

void AmlPwmRegulator::GetVoltageStep(GetVoltageStepCompleter::Sync& completer) {
  completer.Reply(current_step_);
}

void AmlPwmRegulator::GetRegulatorParams(GetRegulatorParamsCompleter::Sync& completer) {
  completer.Reply(min_voltage_uv_, voltage_step_uv_, num_steps_);
}

void AmlPwmRegulator::DdkInit(ddk::InitTxn txn) {
  fdf_dispatcher_t* fdf_dispatcher = fdf_dispatcher_get_current_dispatcher();
  ZX_ASSERT(fdf_dispatcher);
  async_dispatcher_t* dispatcher = fdf_dispatcher_get_async_dispatcher(fdf_dispatcher);
  outgoing_ = component::OutgoingDirectory(dispatcher);

  fuchsia_hardware_vreg::Service::InstanceHandler handler({
      .vreg = bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure),
  });
  auto result = outgoing_->AddService<fuchsia_hardware_vreg::Service>(std::move(handler));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service to the outgoing directory");
    txn.Reply(result.status_value());
    return;
  }

  result = outgoing_->Serve(std::move(outgoing_server_end_));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to serve the outgoing directory");
    txn.Reply(result.status_value());
    return;
  }

  txn.Reply(ZX_OK);
}

zx_status_t AmlPwmRegulator::Create(void* ctx, zx_device_t* parent) {
  // Get Metadata
  auto decoded =
      ddk::GetEncodedMetadata<fuchsia_hardware_vreg::wire::Metadata>(parent, DEVICE_METADATA_VREG);
  if (!decoded.is_ok()) {
    return decoded.error_value();
  }

  const auto& metadata = *decoded.value();

  // Validate
  if (!metadata.has_pwm_vreg()) {
    zxlogf(ERROR, "Metadata incomplete");
    return ZX_ERR_INTERNAL;
  }
  for (const auto& pwm_vreg : metadata.pwm_vreg()) {
    if (!pwm_vreg.has_pwm_index() || !pwm_vreg.has_period_ns() || !pwm_vreg.has_min_voltage_uv() ||
        !pwm_vreg.has_voltage_step_uv() || !pwm_vreg.has_num_steps()) {
      zxlogf(ERROR, "Metadata incomplete");
      return ZX_ERR_INTERNAL;
    }
  }

  // Build Voltage Regulators
  for (const auto& pwm_vreg : metadata.pwm_vreg()) {
    uint32_t idx = pwm_vreg.pwm_index();
    char name[20];
    snprintf(name, sizeof(name), "pwm-%u", idx);
    ddk::PwmProtocolClient pwm;
    if (auto status = ddk::PwmProtocolClient::CreateFromDevice(parent, name, &pwm);
        status != ZX_OK) {
      zxlogf(ERROR, "Failed to create PWM protocol client for %s: %s", name,
             zx_status_get_string(status));
      return ZX_ERR_INTERNAL;
    }
    auto status = pwm.Enable();
    if (status != ZX_OK) {
      zxlogf(ERROR, "Unable to enable PWM %u, %d", idx, status);
      return status;
    }

    fbl::AllocChecker ac;
    std::unique_ptr<AmlPwmRegulator> device(new (&ac)
                                                AmlPwmRegulator(parent, pwm_vreg, std::move(pwm)));
    if (!ac.check()) {
      return ZX_ERR_NO_MEMORY;
    }

    snprintf(name, sizeof(name), "pwm-%u-regulator", idx);
    zx_device_prop_t props[] = {
        {BIND_PWM_ID, 0, idx},
    };

    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (endpoints.is_error()) {
      return endpoints.status_value();
    }

    std::array offers = {
        fuchsia_hardware_vreg::Service::Name,
    };

    device->outgoing_server_end_ = std::move(endpoints->server);

    status = device->DdkAdd(ddk::DeviceAddArgs(name)
                                .set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE)
                                .set_props(props)
                                .set_fidl_service_offers(offers)
                                .set_outgoing_dir(endpoints->client.TakeChannel()));
    if (status != ZX_OK) {
      zxlogf(ERROR, "DdkAdd failed, status = %d", status);
    }

    // Let device runner take ownership of this object.
    [[maybe_unused]] auto* dummy = device.release();
  }

  return ZX_OK;
}

static constexpr zx_driver_ops_t pwm_regulator_driver_ops = []() {
  zx_driver_ops_t driver_ops = {};
  driver_ops.version = DRIVER_OPS_VERSION;
  driver_ops.bind = AmlPwmRegulator::Create;
  return driver_ops;
}();

}  // namespace aml_pwm_regulator

ZIRCON_DRIVER(aml_pwm_regulator, aml_pwm_regulator::pwm_regulator_driver_ops, "zircon", "0.1");
