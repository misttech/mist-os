// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/aml-pwm-regulator/aml-pwm-regulator.h"

#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <string>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/regulator/cpp/bind.h>

namespace {

const std::string_view kDriverName = "aml-pwm-regulator";

}  // namespace

namespace aml_pwm_regulator {

AmlPwmRegulator::AmlPwmRegulator(const VregMetadata& metadata,
                                 fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm_proto_client,
                                 AmlPwmRegulatorDriver* driver)
    : name_(std::string(metadata.name().data(), metadata.name().size())),
      min_voltage_uv_(metadata.min_voltage_uv()),
      voltage_step_uv_(metadata.voltage_step_uv()),
      num_steps_(metadata.num_steps()),
      current_step_(metadata.num_steps()),
      pwm_proto_client_(std::move(pwm_proto_client)) {}

void AmlPwmRegulator::SetVoltageStep(SetVoltageStepRequestView request,
                                     SetVoltageStepCompleter::Sync& completer) {
  if (request->step >= num_steps_) {
    FDF_LOG(ERROR, "Requested step (%u) is larger than allowed (total number of steps %u).",
            request->step, num_steps_);
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (request->step == current_step_) {
    completer.ReplySuccess();
    return;
  }

  auto config_result = pwm_proto_client_->GetConfig();
  if (!config_result.ok() || config_result->is_error()) {
    auto status = config_result.ok() ? config_result->error_value() : config_result.status();
    FDF_LOG(ERROR, "Unable to get PWM config. %s", zx_status_get_string(status));
    completer.ReplyError(status);
    return;
  }

  if (config_result->value()->config.period_ns == 0) {
    FDF_LOG(ERROR, "PWM period config of 0ns is invalid.");
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  aml_pwm::mode_config on = {aml_pwm::Mode::kOn, {}};
  fuchsia_hardware_pwm::wire::PwmConfig cfg = {
      .polarity = config_result->value()->config.polarity,
      .period_ns = config_result->value()->config.period_ns,
      .duty_cycle =
          static_cast<float>((num_steps_ - 1 - request->step) * 100.0 / ((num_steps_ - 1) * 1.0)),
      .mode_config =
          fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on)),
  };

  auto result = pwm_proto_client_->SetConfig(cfg);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Unable to configure PWM. %s", result.status_string());
    completer.ReplyError(result.status());
    return;
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Unable to configure PWM. %s", zx_status_get_string(result->error_value()));
    completer.ReplyError(result->error_value());
    return;
  }
  current_step_ = request->step;

  completer.ReplySuccess();
}

void AmlPwmRegulator::SetState(SetStateRequestView request, SetStateCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void AmlPwmRegulator::GetVoltageStep(GetVoltageStepCompleter::Sync& completer) {
  completer.ReplySuccess(current_step_);
}

void AmlPwmRegulator::GetRegulatorParams(GetRegulatorParamsCompleter::Sync& completer) {
  completer.ReplySuccess(min_voltage_uv_, voltage_step_uv_, num_steps_);
}

void AmlPwmRegulator::Enable(EnableCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void AmlPwmRegulator::Disable(DisableCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

zx::result<std::unique_ptr<AmlPwmRegulator>> AmlPwmRegulator::Create(
    const VregMetadata& metadata, AmlPwmRegulatorDriver* driver) {
  auto connect_result = driver->incoming()->Connect<fuchsia_hardware_pwm::Service::Pwm>("pwm");
  if (connect_result.is_error()) {
    FDF_LOG(ERROR, "Unable to connect to fidl protocol - status: %s",
            connect_result.status_string());
    return connect_result.take_error();
  }

  std::string name{metadata.name().data(), metadata.name().size()};
  fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm_proto_client(
      std::move(connect_result.value()));
  auto result = pwm_proto_client->Enable();
  if (!result.ok()) {
    FDF_LOG(ERROR, "VREG(%s): Unable to enable PWM - %s", name.c_str(), result.status_string());
    return zx::error(ZX_ERR_INTERNAL);
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "VREG(%s): Unable to enable PWM - %s", name.c_str(),
            zx_status_get_string(result->error_value()));
    return result->take_error();
  }
  auto device = std::make_unique<AmlPwmRegulator>(metadata, std::move(pwm_proto_client), driver);

  // Initialize our compat server.
  {
    zx::result<> result = device->compat_server_.Initialize(driver->incoming(), driver->outgoing(),
                                                            driver->node_name(), name);
    if (result.is_error()) {
      return result.take_error();
    }
  }

  {
    auto result = driver->outgoing()->AddService<fuchsia_hardware_vreg::Service>(
        fuchsia_hardware_vreg::Service::InstanceHandler({
            .vreg = device->bindings_.CreateHandler(
                device.get(), fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                fidl::kIgnoreBindingClosure),
        }),
        name);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to add Device service %s", result.status_string());
      return result.take_error();
    }
  }

  fidl::Arena arena;
  auto offers = device->compat_server_.CreateOffers2(arena);
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_vreg::Service>(arena, name));

  fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty> properties(arena, 1);
  properties[0] = fdf::MakeProperty(arena, bind_fuchsia_regulator::NAME, name);

  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, name)
                        .offers2(arena, std::move(offers))
                        .properties(properties)
                        .Build();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (!controller_endpoints.is_ok()) {
    FDF_LOG(ERROR, "Failed to create controller endpoints: %s",
            controller_endpoints.status_string());
    return controller_endpoints.take_error();
  }

  fidl::WireResult add_result =
      fidl::WireCall(driver->node())->AddChild(args, std::move(controller_endpoints->server), {});
  if (!add_result.ok()) {
    FDF_LOG(ERROR, "Failed to add child: %s", result.status_string());
    return zx::error(add_result.status());
  }

  device->controller_.Bind(std::move(controller_endpoints->client));

  return zx::ok(std::move(device));
}

AmlPwmRegulatorDriver::AmlPwmRegulatorDriver(fdf::DriverStartArgs start_args,
                                             fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

zx::result<> AmlPwmRegulatorDriver::Start() {
  fidl::Arena arena;
  auto decoded = compat::GetMetadata<fuchsia_hardware_vreg::wire::VregMetadata>(
      incoming(), arena, DEVICE_METADATA_VREG, "pdev");
  if (decoded.is_error()) {
    FDF_LOG(ERROR, "Failed to get vreg metadata: %s", decoded.status_string());
    return decoded.take_error();
  }

  const auto& metadata = *decoded.value();

  // Validate
  if (!metadata.has_name() || !metadata.has_min_voltage_uv() || !metadata.has_voltage_step_uv() ||
      !metadata.has_num_steps()) {
    FDF_LOG(ERROR, "Metadata incomplete");
    return zx::error(ZX_ERR_INTERNAL);
  }

  // Build Voltage Regulator
  auto regulator = AmlPwmRegulator::Create(metadata, this);
  if (regulator.is_error()) {
    return regulator.take_error();
  }
  regulators_ = std::move(*regulator);
  return zx::ok();
}

}  // namespace aml_pwm_regulator

FUCHSIA_DRIVER_EXPORT(aml_pwm_regulator::AmlPwmRegulatorDriver);
