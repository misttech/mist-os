// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_PWM_DRIVERS_AML_PWM_AML_PWM_H_
#define SRC_DEVICES_PWM_DRIVERS_AML_PWM_AML_PWM_H_

#include <fidl/fuchsia.hardware.pwm/cpp/fidl.h>
#include <fuchsia/hardware/pwm/cpp/banjo.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/metadata/cpp/metadata_server.h>
#include <lib/mmio/mmio.h>
#include <zircon/types.h>

#include <array>
#include <cstdint>
#include <vector>

#include <ddktl/device.h>
#include <fbl/auto_lock.h>

#include "aml-pwm-regs.h"

namespace pwm {

constexpr uint32_t kPwmPairCount = 2;

class AmlPwm {
 public:
  explicit AmlPwm(fdf::MmioBuffer mmio, fuchsia_hardware_pwm::PwmChannelInfo channel1,
                  fuchsia_hardware_pwm::PwmChannelInfo channel2)
      : channels_{std::move(channel1), std::move(channel2)},
        enabled_{false, false},
        mmio_(std::move(mmio)) {}

  void Init() {
    for (uint32_t i = 0; i < kPwmPairCount; i++) {
      mode_configs_[i].mode = Mode::kOff;
      mode_configs_[i].regular = {};
      configs_[i] = {.polarity = channels_[i].polarity().value_or(false),
                     .period_ns = channels_[i].period_ns().value_or(0),
                     .duty_cycle = 0.0,
                     .mode_config_buffer = reinterpret_cast<uint8_t*>(&mode_configs_[i]),
                     .mode_config_size = sizeof(mode_config)};
      const bool should_initialize = !channels_[i].skip_init().value_or(false);
      if (should_initialize) {
        SetMode(i, Mode::kOff);
      }
    }
  }

  zx_status_t PwmImplGetConfig(uint32_t idx, pwm_config_t* out_config);
  zx_status_t PwmImplSetConfig(uint32_t idx, const pwm_config_t* config);
  zx_status_t PwmImplEnable(uint32_t idx);
  zx_status_t PwmImplDisable(uint32_t idx);

 private:
  friend class AmlPwmDriver;

  // Register fine control.

  // Sets the PWM controller working mode. `mode` must be a valid PWM mode.
  void SetMode(uint32_t idx, Mode mode);
  // Sets the duty cycle for the default timer.
  // `divider` is the actual PWM clock divider factor in range [1, 128].
  // `duty_cycle` must be a float value in the range [0.0, 100.0].
  void SetDutyCycle(uint32_t idx, int divider, uint32_t period, float duty_cycle);
  // Sets the duty cycle for the second timer in two-timer mode.
  // `divider` is the actual PWM clock divider factor in range [1, 128].
  // `duty_cycle` must be a float value in the range [0.0, 100.0].
  void SetDutyCycle2(uint32_t idx, int divider, uint32_t period, float duty_cycle);
  void Invert(uint32_t idx, bool on);
  void EnableHiZ(uint32_t idx, bool on);
  void EnableClock(uint32_t idx, bool on);
  void EnableConst(uint32_t idx, bool on);
  void SetClock(uint32_t idx, uint8_t sel);
  // Sets the PWM clock divider factor. `divider` must be in range [1, 128].
  void SetClockDivider(uint32_t idx, int divider);
  void EnableBlink(uint32_t idx, bool on);
  void SetBlinkTimes(uint32_t idx, uint8_t times);
  void SetDSSetting(uint32_t idx, uint16_t val);
  void SetTimers(uint32_t idx, uint8_t timer1, uint8_t timer2);

  std::array<fuchsia_hardware_pwm::PwmChannelInfo, kPwmPairCount> channels_;
  std::array<bool, kPwmPairCount> enabled_;
  std::array<pwm_config_t, kPwmPairCount> configs_;
  std::array<mode_config, kPwmPairCount> mode_configs_;
  std::array<fbl::Mutex, REG_COUNT> locks_;
  fdf::MmioBuffer mmio_;
};

class AmlPwmDriver : public fdf::DriverBase, public ddk::PwmImplProtocol<AmlPwmDriver> {
 public:
  static constexpr std::string_view kDriverName = "pwm";
  static constexpr std::string_view kChildNodeName = "aml-pwm-device";

  AmlPwmDriver(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

  // fdf::DriverBase implementation.
  zx::result<> Start() override;

  zx_status_t PwmImplGetConfig(uint32_t idx, pwm_config_t* out_config);
  zx_status_t PwmImplSetConfig(uint32_t idx, const pwm_config_t* config);
  zx_status_t PwmImplEnable(uint32_t idx);
  zx_status_t PwmImplDisable(uint32_t idx);

 private:
  std::vector<std::unique_ptr<AmlPwm>> pwms_;

  size_t max_pwm_id_ = 0;

  fidl::ClientEnd<fuchsia_driver_framework::NodeController> child_;
  compat::SyncInitializedDeviceServer compat_server_;
  compat::BanjoServer banjo_server_{ZX_PROTOCOL_PWM_IMPL, this, &pwm_impl_protocol_ops_};
  fdf_metadata::MetadataServer<fuchsia_hardware_pwm::PwmChannelsMetadata> metadata_server_;
};

}  // namespace pwm

#endif  // SRC_DEVICES_PWM_DRIVERS_AML_PWM_AML_PWM_H_
