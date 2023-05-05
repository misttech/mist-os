// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/fusb302/fusb302.h"

#include <fuchsia/hardware/gpio/cpp/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
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

namespace {

constexpr uint64_t kPortPacketKeyInterrupt = 1;
constexpr uint64_t kPortPacketKeyTimer = 2;

}  // namespace

zx::result<> Fusb302::PumpIrqPort() {
  zx_port_packet_t packet;
  zx_status_t status = port_.wait(zx::time(ZX_TIME_INFINITE), &packet);
  if (status != ZX_OK) {
    zxlogf(ERROR, "zx::port::wait() failed: %s", zx_status_get_string(status));
    return zx::error_result(status);
  }

  HardwareStateChanges changes;
  switch (packet.key) {
    case kPortPacketKeyInterrupt:
      changes = signals_.ServiceInterrupts();
      break;

    case kPortPacketKeyTimer:
      zxlogf(TRACE, "State machine timer fired off");
      changes.timer_signaled = true;
      break;

    default:
      zxlogf(ERROR, "Unrecognized packet key: %" PRIu64, packet.key);
      return zx::error_result(ZX_ERR_INTERNAL);
  }

  ProcessStateChanges(changes);

  if (packet.key == kPortPacketKeyInterrupt) {
    status = irq_.ack();
    if (status != ZX_OK) {
      zxlogf(ERROR, "IRQ ack() failed: %s", zx_status_get_string(status));
      return zx::error_result(status);
    }
  }

  return zx::ok();
}

zx_status_t Fusb302::IrqThreadEntryPoint() {
  zx_status_t status = ZX_OK;

  {
    const char* role_name = "fuchsia.devices.power.drivers.fusb302.interrupt";
    status = device_set_profile_by_role(parent_, thrd_get_zx_handle(irq_thread_), role_name,
                                        strlen(role_name));
    if (status != ZX_OK) {
      zxlogf(WARNING, "Failed to apply role to interrupt thread: %s", zx_status_get_string(status));
    }
  }

  zx::result<> last_pump_result = zx::ok();
  while (last_pump_result.is_ok()) {
    last_pump_result = PumpIrqPort();
  }

  is_thread_running_ = false;
  zxlogf(ERROR, "IRQ thread failed: %s", zx_status_get_string(status));
  return status;
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
    zxlogf(ERROR, "Failed to write Reset register: %s", zx_status_get_string(status));
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
    zxlogf(ERROR, "Failed to initialize inspect: %s", result.status_string());
    return result.error_value();
  }

  zx_status_t status = ResetHardwareAndStartPowerRoleDetection();
  if (status != ZX_OK) {
    zxlogf(ERROR, "ResetHardwareAndStartPowerRoleDetection() failed: %s",
           zx_status_get_string(status));
    return status;
  }

  status = zx::port::create(ZX_PORT_BIND_TO_INTERRUPT, &port_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "zx::port::create() failed: %s", zx_status_get_string(status));
    return status;
  }
  irq_.bind(port_, kPortPacketKeyInterrupt, /*options=*/0);
  status = thrd_status_to_zx_status(thrd_create_with_name(
      &irq_thread_,
      [](void* ctx) -> int { return reinterpret_cast<Fusb302*>(ctx)->IrqThreadEntryPoint(); }, this,
      "fusb302-irq-thread"));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to start thread: %s", zx_status_get_string(status));
    return status;
  }
  is_thread_running_ = true;

  return ZX_OK;
}

// static
zx_status_t Fusb302::Create(void* context, zx_device_t* parent) {
  auto client_end =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_i2c::Service::Device>(parent, "i2c");
  if (client_end.is_error()) {
    zxlogf(ERROR, "Failed to get I2C bus for registers: %s", client_end.status_string());
    return client_end.status_value();
  }

  ddk::GpioProtocolClient gpio;
  zx_status_t status = ddk::GpioProtocolClient::CreateFromDevice(parent, "gpio", &gpio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get GPIO for interrupt pin: %s", zx_status_get_string(status));
    return status;
  }
  status = gpio.ConfigIn(GPIO_PULL_UP);
  if (status != ZX_OK) {
    zxlogf(ERROR, "GPIO ConfigIn() failed: %s", zx_status_get_string(status));
  }
  zx::interrupt irq;
  status = gpio.GetInterrupt(ZX_INTERRUPT_MODE_LEVEL_LOW, &irq);
  if (status != ZX_OK) {
    zxlogf(ERROR, "GPIO GetInterrupt() failed: %s", zx_status_get_string(status));
  }

  fbl::AllocChecker alloc_checker;
  auto device = fbl::make_unique_checked<Fusb302>(&alloc_checker, parent, std::move(*client_end),
                                                  std::move(irq));
  if (!alloc_checker.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  status = device->Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Init() failed: %s", zx_status_get_string(status));
    return status;
  }

  status = device->DdkAdd(
      ddk::DeviceAddArgs("fusb302").set_inspect_vmo(device->inspect_.DuplicateVmo()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAdd() failed: %s", zx_status_get_string(status));
    return status;
  }

  // The device manager now owns `device`.
  [[maybe_unused]] auto* dropped_ptr = device.release();
  return ZX_OK;
}

void Fusb302::DdkRelease() { delete this; }

zx::result<> Fusb302::WaitAsyncForTimer(zx::timer& timer) {
  const zx_status_t status =
      timer.wait_async(port_, kPortPacketKeyTimer, ZX_TIMER_SIGNALED, /*options=*/0);
  if (status != ZX_OK) {
    zxlogf(WARNING, "Failed to wait on timer: %s", zx_status_get_string(status));
  }
  return zx::make_result(status);
}

namespace {

constexpr zx_driver_ops_t kDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = fusb302::Fusb302::Create,
};

}  // namespace

}  // namespace fusb302

ZIRCON_DRIVER(fusb302, fusb302::kDriverOps, "zircon", "0.1");
