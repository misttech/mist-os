// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_BACKLIGHT_DRIVERS_VIM3_PWM_BACKLIGHT_VIM3_PWM_BACKLIGHT_H_
#define SRC_UI_BACKLIGHT_DRIVERS_VIM3_PWM_BACKLIGHT_VIM3_PWM_BACKLIGHT_H_

#include <fidl/fuchsia.hardware.backlight/cpp/wire.h>
#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.hardware.pwm/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

namespace vim3_pwm_backlight {

// Driver for VIM3's PWM backlight controller, used for Khadas TS050
// touchscreen.
class Vim3PwmBacklight : public fdf::DriverBase,
                         public fidl::WireServer<fuchsia_hardware_backlight::Device> {
 public:
  struct State {
    bool operator==(const State& other) const {
      return power == other.power && brightness == other.brightness;
    }

    // True iff the PWM backlight controller is powered on.
    bool power;

    // Normalized brightness, must be in range [0.0, 1.0].
    double brightness;
  };

  static constexpr std::string_view kDriverName = "vim3_pwm_backlight";

  Vim3PwmBacklight(fdf::DriverStartArgs start_args,
                   fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

  // fdf::DriverBase implementation.
  zx::result<> Start() override;

  // fidl::WireServer<fuchsia_hardware_backlight::Device> implementation.
  void GetStateNormalized(GetStateNormalizedCompleter::Sync& completer) override;
  void SetStateNormalized(SetStateNormalizedRequestView request,
                          SetStateNormalizedCompleter::Sync& completer) override;

  void GetStateAbsolute(GetStateAbsoluteCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void SetStateAbsolute(SetStateAbsoluteRequestView request,
                        SetStateAbsoluteCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetMaxAbsoluteBrightness(GetMaxAbsoluteBrightnessCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  // Initialize the hardware controller and sets the backlight to the predefined
  // initial state.
  zx_status_t Initialize();

  // Sets the backlight to `target` state. `target` must be a valid state.
  //
  // If it succeeds, it returns ZX_OK and changes the stored state / inspect
  // values.
  // If it fails, it will try to revert to the previous state and return the
  // error value; the stored state / inspect values won't change.
  zx_status_t SetState(State target);

  // Configures the PWM controller.
  //
  // `enabled` sets the controller mode (on or off).
  // `duty_cycle` stands for the duty cycle percentage, must be a value in
  // range [0.0, 100.0].
  zx_status_t SetPwmConfig(bool enabled, float duty_cycle);

  // Initializes the GPIO pin to power on / off backlight.
  //
  // Must run once before setting the GPIO value.
  zx_status_t InitializeGpioBacklightPower(bool initially_enabled);

  // Sets the GPIO pin to power on / off backlight.
  zx_status_t SetGpioBacklightPower(bool enabled);

  void StoreState(State state);

  inspect::Node root_;

  fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm_proto_client_;
  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> gpio_backlight_power_;

  State state_;

  inspect::BoolProperty power_property_;
  inspect::DoubleProperty brightness_property_;

  fidl::ServerBindingGroup<fuchsia_hardware_backlight::Device> bindings_;
};

}  // namespace vim3_pwm_backlight

#endif  // SRC_UI_BACKLIGHT_DRIVERS_VIM3_PWM_BACKLIGHT_VIM3_PWM_BACKLIGHT_H_
