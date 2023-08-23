// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "temperature_driver.h"

namespace fake_driver {
TemperatureDeviceProtocolServer::TemperatureDeviceProtocolServer(float* temperature)
    : temperature_(temperature) {}

void TemperatureDeviceProtocolServer::GetTemperatureCelsius(
    GetTemperatureCelsiusCompleter::Sync& completer) {
  completer.Reply(ZX_OK, *temperature_);
}

void TemperatureDeviceProtocolServer::GetSensorName(GetSensorNameCompleter::Sync& completer) {
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void TemperatureDeviceProtocolServer::Serve(
    async_dispatcher_t* dispatcher, fidl::ServerEnd<fuchsia_hardware_temperature::Device> server) {
  bindings_.AddBinding(dispatcher, std::move(server), this, fidl::kIgnoreBindingClosure);
}

}  // namespace fake_driver
