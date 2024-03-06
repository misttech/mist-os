// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_PWM_DRIVERS_AML_PWM_INIT_AML_PWM_INIT_H_
#define SRC_DEVICES_PWM_DRIVERS_AML_PWM_INIT_AML_PWM_INIT_H_

#include <fidl/fuchsia.hardware.clock/cpp/wire.h>
#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.hardware.pwm/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
// For compatibility with wrapped DFv1 drivers:
#include <lib/driver/compat/cpp/device_server.h>

#include <soc/aml-common/aml-pwm-regs.h>

namespace pwm_init {

class PwmInitDevice {
 public:
  explicit PwmInitDevice(fidl::ClientEnd<fuchsia_hardware_clock::Clock> clock,
                         fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm,
                         fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> wifi_gpio,
                         fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> bt_gpio)
      : wifi_32k768_clk_(std::move(clock)),
        pwm_(std::move(pwm)),
        wifi_gpio_(std::move(wifi_gpio)),
        bt_gpio_(std::move(bt_gpio)) {}

  zx_status_t Init();

 private:
  fidl::WireSyncClient<fuchsia_hardware_clock::Clock> wifi_32k768_clk_;
  fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm_;
  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> wifi_gpio_;
  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> bt_gpio_;
};

class PwmInitDriver : public fdf::DriverBase {
 public:
  PwmInitDriver(fdf::DriverStartArgs start_args,
                fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  zx::result<> Start() override;

 private:
  std::unique_ptr<PwmInitDevice> initer_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_client_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;

  // For compatibility with wrapped DFv1 drivers:
  compat::SyncInitializedDeviceServer compat_server_;
};

}  // namespace pwm_init

#endif  // SRC_DEVICES_PWM_DRIVERS_AML_PWM_INIT_AML_PWM_INIT_H_
