// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_POWER_DRIVERS_FUSB302_FUSB302_H_
#define SRC_DEVICES_POWER_DRIVERS_FUSB302_FUSB302_H_

#include <fidl/fuchsia.hardware.i2c/cpp/wire.h>
#include <fidl/fuchsia.hardware.powersource/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/inspect/cpp/inspector.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>
#include <lib/zx/timer.h>
#include <threads.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <cstdint>

#include <ddktl/device.h>
#include <ddktl/fidl.h>

#include "src/devices/power/drivers/fusb302/fusb302-controls.h"
#include "src/devices/power/drivers/fusb302/fusb302-fifos.h"
#include "src/devices/power/drivers/fusb302/fusb302-identity.h"
#include "src/devices/power/drivers/fusb302/fusb302-protocol.h"
#include "src/devices/power/drivers/fusb302/fusb302-sensors.h"
#include "src/devices/power/drivers/fusb302/fusb302-signals.h"
#include "src/devices/power/drivers/fusb302/pd-sink-state-machine.h"
#include "src/devices/power/drivers/fusb302/typec-port-state-machine.h"
#include "src/devices/power/drivers/fusb302/usb-pd-sink-policy.h"

namespace fusb302 {

class Fusb302;
using DeviceType =
    ddk::Device<Fusb302, ddk::Messageable<fuchsia_hardware_powersource::Source>::Mixin>;

// Fusb302: Device that keeps track of the state of the HW, services FIDL requests, and runs the IRQ
// thread, which in turn runs StateMachine when called on.
class Fusb302 : public DeviceType {
 public:
  Fusb302(zx_device_t* parent, fidl::ClientEnd<fuchsia_hardware_i2c::Device> i2c, zx::interrupt irq)
      : DeviceType(parent),
        i2c_(std::move(i2c)),
        irq_(std::move(irq)),
        identity_(i2c_, inspect_.GetRoot().CreateChild("Identity")),
        sensors_(i2c_, inspect_.GetRoot().CreateChild("Sensors")),
        fifos_(i2c_),
        protocol_(fifos_),
        signals_(i2c_, sensors_, protocol_),
        controls_(i2c_, sensors_, inspect_.GetRoot().CreateChild("Controls")),
        sink_policy_({.min_voltage_mv = 5'000, .max_voltage_mv = 12'000, .max_power_mw = 24'000}),
        port_state_machine_(*this, inspect_.GetRoot().CreateChild("PortStateMachine")),
        pd_state_machine_(sink_policy_, *this,
                          inspect_.GetRoot().CreateChild("SinkPolicyEngineStateMachine")) {
    ZX_DEBUG_ASSERT(i2c_.is_valid());
    ZX_DEBUG_ASSERT(irq_.is_valid());
  }

  Fusb302(const Fusb302&) = delete;
  Fusb302& operator=(const Fusb302&) = delete;

  ~Fusb302() override {
    const zx_status_t status = irq_.destroy();
    if (status != ZX_OK) {
      zxlogf(WARNING, "zx::interrupt::destroy() failed: %s", zx_status_get_string(status));
    }
    if (is_thread_running_) {
      thrd_join(irq_thread_, nullptr);
      is_thread_running_ = false;
    }
  }

  static zx_status_t Create(void* context, zx_device_t* parent);

  void DdkRelease();

  // TODO (rdzhuang): change power FIDL to supply required values in SourceInfo
  void GetPowerInfo(GetPowerInfoCompleter::Sync& completer) override {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
  }
  void GetStateChangeEvent(GetStateChangeEventCompleter::Sync& completer) override {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
  }
  void GetBatteryInfo(GetBatteryInfoCompleter::Sync& completer) override {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
  }

  Fusb302Sensors& sensors() { return sensors_; }
  Fusb302Protocol& protocol() { return protocol_; }
  Fusb302Controls& controls() { return controls_; }

  inspect::Inspector& InspectorForTesting() { return inspect_; }

  // Asynchronously waits for a timer to be signaled once.
  //
  // When the timer is signaled, the state machines connected to this instance
  // will be run with an indication that a timer was signaled.
  zx::result<> WaitAsyncForTimer(zx::timer& timer);

 private:
  // Initialization Functions and Variables
  zx_status_t Init();
  zx_status_t ResetHardwareAndStartPowerRoleDetection();

  // Initial routine / entry point for the IRQ handling thread.
  zx_status_t IrqThreadEntryPoint();

  // Reads one packet from the interrupt request port, and services it.
  //
  // Returns an error iff it's not safe to continue pumping the port.
  zx::result<> PumpIrqPort();

  void ProcessStateChanges(HardwareStateChanges changes);

  fidl::ClientEnd<fuchsia_hardware_i2c::Device> i2c_;
  zx::interrupt irq_;
  zx::port port_;
  std::atomic_bool is_thread_running_ = false;
  thrd_t irq_thread_;

  inspect::Inspector inspect_;

  Fusb302Identity identity_;
  Fusb302Sensors sensors_;
  Fusb302Fifos fifos_;
  Fusb302Protocol protocol_;
  Fusb302Signals signals_;
  Fusb302Controls controls_;

  usb_pd::SinkPolicy sink_policy_;

  TypeCPortStateMachine port_state_machine_;
  SinkPolicyEngineStateMachine pd_state_machine_;
};

}  // namespace fusb302

#endif  // SRC_DEVICES_POWER_DRIVERS_FUSB302_FUSB302_H_
