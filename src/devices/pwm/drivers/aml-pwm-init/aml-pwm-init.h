// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_PWM_DRIVERS_AML_PWM_INIT_AML_PWM_INIT_H_
#define SRC_DEVICES_PWM_DRIVERS_AML_PWM_INIT_AML_PWM_INIT_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.hardware.pwm/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/platform-defs.h>

#include <ddktl/device.h>
#include <soc/aml-common/aml-pwm-regs.h>

namespace pwm_init {

class PwmInitDevice;
using PwmInitDeviceType = ddk::Device<PwmInitDevice>;

class PwmInitDevice : public PwmInitDeviceType {
 public:
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  void DdkRelease() { delete this; }

 private:
  friend class FakePwmInitDevice;

  explicit PwmInitDevice(zx_device_t* parent, fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm,
                         fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> wifi_gpio,
                         fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> bt_gpio)
      : PwmInitDeviceType(parent),
        pwm_(std::move(pwm)),
        wifi_gpio_(std::move(wifi_gpio)),
        bt_gpio_(std::move(bt_gpio)) {}

  zx_status_t Init();

  fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm_;
  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> wifi_gpio_;
  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> bt_gpio_;
};

}  // namespace pwm_init

#endif  // SRC_DEVICES_PWM_DRIVERS_AML_PWM_INIT_AML_PWM_INIT_H_
