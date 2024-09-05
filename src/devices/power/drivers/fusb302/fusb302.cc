// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/fusb302/fusb302.h"

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/profile.h>
#include <lib/zx/result.h>
#include <lib/zx/timer.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/threads.h>
#include <zircon/types.h>

#include <cinttypes>
#include <cstddef>
#include <cstdint>
#include <string>

#include <fbl/alloc_checker.h>
#include <fbl/string_buffer.h>

#include "src/devices/power/drivers/fusb302/fusb302-controls.h"
#include "src/devices/power/drivers/fusb302/pd-sink-state-machine.h"
#include "src/devices/power/drivers/fusb302/registers.h"
#include "src/devices/power/drivers/fusb302/state-machine-base.h"
#include "src/devices/power/drivers/fusb302/typec-port-state-machine.h"

namespace fusb302 {

void Fusb302::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                        const zx_packet_interrupt_t* interrupt) {
  ProcessStateChanges(signals_.ServiceInterrupts());
  irq_.ack();
}

void Fusb302::HandleTimeout(async_dispatcher_t*, async::WaitBase*, zx_status_t status,
                            const zx_packet_signal_t*) {
  HardwareStateChanges changes;
  FDF_LOG(TRACE, "State machine timer fired off");
  changes.timer_signaled = true;
  ProcessStateChanges(changes);
}

void Fusb302::ProcessStateChanges(HardwareStateChanges changes) {
  if (changes.received_reset) {
    pd_state_machine_.DidReceiveSoftReset();
    pd_state_machine_.Run(SinkPolicyEngineInput::kInitialized);
  }

  while (protocol_.HasUnreadMessage()) {
    pd_state_machine_.Run(SinkPolicyEngineInput::kMessageReceived);

    if (protocol_.HasUnreadMessage()) {
      pd_state_machine_.ProcessUnexpectedMessage();
      pd_state_machine_.Run(SinkPolicyEngineInput::kInitialized);
    }
  }

  if (changes.port_state_changed) {
    port_state_machine_.Run(TypeCPortInput::kPortStateChanged);

    if (port_state_machine_.current_state() == TypeCPortState::kSinkAttached) {
      pd_state_machine_.Run(SinkPolicyEngineInput::kInitialized);
    } else {
      pd_state_machine_.Reset();
    }
  }

  if (changes.timer_signaled &&
      port_state_machine_.current_state() == TypeCPortState::kSinkAttached) {
    pd_state_machine_.Run(SinkPolicyEngineInput::kTimerFired);
  }
}

zx_status_t Fusb302::ResetHardwareAndStartPowerRoleDetection() {
  auto status = ResetReg::Get().FromValue(0).set_sw_res(true).WriteTo(i2c_);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to write Reset register: %s", zx_status_get_string(status));
    return status;
  }

  zx::result<> result = signals_.InitInterruptUnit();
  if (!result.is_ok()) {
    return result.error_value();
  }

  result = controls_.ResetIntoPowerRoleDiscovery();
  if (!result.is_ok()) {
    return result.error_value();
  }

  return ZX_OK;
}

zx_status_t Fusb302::Init() {
  zx::result<> result = identity_.ReadIdentity();
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize inspect: %s", result.status_string());
    return result.error_value();
  }

  zx_status_t status = ResetHardwareAndStartPowerRoleDetection();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "ResetHardwareAndStartPowerRoleDetection() failed: %s",
            zx_status_get_string(status));
    return status;
  }

  irq_handler_.set_object(irq_.get());
  irq_handler_.Begin(dispatcher_.async_dispatcher());

  return ZX_OK;
}

zx::result<> Fusb302::WaitAsyncForTimer(zx::timer& timer) {
  timeout_handler_.set_object(timer.get());
  timeout_handler_.set_trigger(ZX_TIMER_SIGNALED);
  auto status = timeout_handler_.Begin(dispatcher_.async_dispatcher(),
                                       fit::bind_member(this, &Fusb302::HandleTimeout));
  if (status != ZX_OK) {
    FDF_LOG(WARNING, "Failed to wait on timer: %s", zx_status_get_string(status));
  }
  return zx::make_result(status);
}

zx::result<> Fusb302Device::Start() {
  // Map hardware resources.
  fidl::ClientEnd<fuchsia_hardware_i2c::Device> i2c;
  zx::interrupt irq;
  {
    zx::result result = incoming()->Connect<fuchsia_hardware_i2c::Service::Device>("i2c");
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to open i2c service: %s", result.status_string());
      return result.take_error();
    }
    i2c = std::move(result.value());
  }
  {
    zx::result result = incoming()->Connect<fuchsia_hardware_gpio::Service::Device>("gpio");
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to open gpio service: %s", result.status_string());
      return result.take_error();
    }

    auto gpio = fidl::WireSyncClient(std::move(result.value()));
    if (auto result = gpio->ConfigIn(fuchsia_hardware_gpio::GpioFlags::kPullUp);
        !result.ok() || result->is_error()) {
      FDF_LOG(ERROR, "GPIO ConfigIn() failed: %s",
              result.ok() ? zx_status_get_string(result->error_value())
                          : result.FormatDescription().c_str());
    }
    if (auto result = gpio->GetInterrupt(ZX_INTERRUPT_MODE_LEVEL_LOW);
        !result.ok() || result->is_error()) {
      FDF_LOG(ERROR, "GPIO GetInterrupt() failed: %s",
              result.ok() ? zx_status_get_string(result->error_value())
                          : result.FormatDescription().c_str());
    } else {
      irq = std::move(result->value()->irq);
    }
  }

  auto fusb302_dispatcher = fdf::SynchronizedDispatcher::Create(
      {}, "fusb302", [&](fdf_dispatcher_t*) {}, "fuchsia.devices.power.drivers.fusb302.interrupt");
  ZX_ASSERT_MSG(!fusb302_dispatcher.is_error(), "Creating dispatcher error: %s",
                zx_status_get_string(fusb302_dispatcher.status_value()));

  device_ = std::make_unique<fusb302::Fusb302>(std::move(*fusb302_dispatcher), std::move(i2c),
                                               std::move(irq));
  auto status = device_->Init();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Init() failed: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  auto result = outgoing()->component().AddUnmanagedProtocol<fuchsia_hardware_powersource::Source>(
      source_bindings_.CreateHandler(device_.get(), dispatcher(), fidl::kIgnoreBindingClosure),
      kDeviceName);
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add Device service %s", result.status_string());
    return result.take_error();
  }

  if (zx::result result = CreateDevfsNode(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to export to devfs %s", result.status_string());
    return result.take_error();
  }

  return zx::ok();
}

void Fusb302Device::Stop() { device_.reset(); }

zx::result<> Fusb302Device::CreateDevfsNode() {
  fidl::Arena arena;
  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    return connector.take_error();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .class_name("power");

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, kDeviceName)
                  .devfs_args(devfs.Build())
                  .Build();

  auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed to create endpoints: %s",
                node_endpoints.status_string());

  fidl::WireResult result = fidl::WireCall(node())->AddChild(
      args, std::move(controller_endpoints.server), std::move(node_endpoints->server));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child %s", result.status_string());
    return zx::error(result.status());
  }
  controller_.Bind(std::move(controller_endpoints.client));
  node_.Bind(std::move(node_endpoints->client));
  return zx::ok();
}

}  // namespace fusb302

FUCHSIA_DRIVER_EXPORT(fusb302::Fusb302Device);
