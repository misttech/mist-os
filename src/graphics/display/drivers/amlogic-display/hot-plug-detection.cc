// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/hot-plug-detection.h"

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <memory>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "src/graphics/display/drivers/amlogic-display/irq-handler-loop-util.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/namespace/namespace.h"

namespace amlogic_display {

// static
zx::result<std::unique_ptr<HotPlugDetection>> HotPlugDetection::Create(
    display::Namespace& incoming, HotPlugDetection::OnStateChangeHandler on_state_change) {
  static const char kHpdGpioFragmentName[] = "gpio-hdmi-hotplug-detect";
  zx::result<fidl::ClientEnd<fuchsia_hardware_gpio::Gpio>> pin_gpio_result =
      incoming.Connect<fuchsia_hardware_gpio::Service::Device>(kHpdGpioFragmentName);
  if (pin_gpio_result.is_error()) {
    zxlogf(ERROR, "Failed to get gpio protocol from fragment %s: %s", kHpdGpioFragmentName,
           pin_gpio_result.status_string());
    return pin_gpio_result.take_error();
  }

  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> pin_gpio(std::move(pin_gpio_result.value()));

  fidl::WireResult interrupt_result = pin_gpio->GetInterrupt(ZX_INTERRUPT_MODE_LEVEL_HIGH);
  if (interrupt_result->is_error()) {
    zxlogf(ERROR, "Failed to send GetInterrupt request to HPD GPIO: %s",
           interrupt_result.status_string());
    return interrupt_result->take_error();
  }
  fidl::WireResultUnwrapType<fuchsia_hardware_gpio::Gpio::GetInterrupt>& interrupt_response =
      interrupt_result.value();
  if (interrupt_response.is_error()) {
    zxlogf(ERROR, "Failed to get interrupt from HPD GPIO: %s",
           zx_status_get_string(interrupt_response.error_value()));
    return interrupt_response.take_error();
  }

  fbl::AllocChecker alloc_checker;
  std::unique_ptr<HotPlugDetection> hot_plug_detection = fbl::make_unique_checked<HotPlugDetection>(
      &alloc_checker, pin_gpio.TakeClientEnd(), std::move(interrupt_response->irq),
      std::move(on_state_change));
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Out of memory while allocating HotPlugDetection");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> init_result = hot_plug_detection->Init();
  if (init_result.is_error()) {
    zxlogf(ERROR, "Failed to initalize HPD: %s", init_result.status_string());
    return init_result.take_error();
  }

  return zx::ok(std::move(hot_plug_detection));
}

HotPlugDetection::HotPlugDetection(fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> pin_gpio,
                                   zx::interrupt pin_gpio_interrupt,
                                   HotPlugDetection::OnStateChangeHandler on_state_change)
    : pin_gpio_(std::move(pin_gpio)),
      pin_gpio_irq_(std::move(pin_gpio_interrupt)),
      on_state_change_(std::move(on_state_change)),
      irq_handler_loop_config_(CreateIrqHandlerAsyncLoopConfig()),
      irq_handler_loop_(&irq_handler_loop_config_) {
  ZX_DEBUG_ASSERT(on_state_change_);
  pin_gpio_irq_handler_.set_object(pin_gpio_irq_.get());
}

HotPlugDetection::~HotPlugDetection() {
  // In order to shut down the interrupt handler and join the thread, the
  // interrupt must be destroyed first.
  if (pin_gpio_irq_.is_valid()) {
    zx_status_t status = pin_gpio_irq_.destroy();
    if (status != ZX_OK) {
      zxlogf(ERROR, "GPIO interrupt destroy failed: %s", zx_status_get_string(status));
    }
  }

  irq_handler_loop_.Shutdown();

  // After the interrupt handler loop is shut down, the interrupt is unused
  // and we can safely release the interrupt.
  if (pin_gpio_.is_valid()) {
    fidl::WireResult release_result = pin_gpio_->ReleaseInterrupt();
    if (!release_result.ok()) {
      zxlogf(ERROR, "Failed to connect to GPIO FIDL protocol: %s", release_result.status_string());
    } else {
      fidl::WireResultUnwrapType<fuchsia_hardware_gpio::Gpio::ReleaseInterrupt>& release_response =
          release_result.value();
      if (release_response.is_error()) {
        zxlogf(ERROR, "Failed to release GPIO interrupt: %s",
               zx_status_get_string(release_response.error_value()));
      }
    }
  }
}

HotPlugDetectionState HotPlugDetection::CurrentState() {
  fbl::AutoLock lock(&mutex_);
  return current_pin_state_;
}

zx::result<> HotPlugDetection::Init() {
  fidl::WireResult<fuchsia_hardware_gpio::Gpio::ConfigIn> config_in_result =
      pin_gpio_->ConfigIn(fuchsia_hardware_gpio::GpioFlags::kPullDown);
  if (config_in_result->is_error()) {
    zxlogf(ERROR, "Failed to send ConfigIn request to hpd gpio: %s",
           config_in_result.status_string());
    return zx::error(config_in_result.status());
  }
  fidl::WireResultUnwrapType<fuchsia_hardware_gpio::Gpio::ConfigIn>& config_in_response =
      config_in_result.value();
  if (config_in_response.is_error()) {
    zxlogf(ERROR, "Failed to configure hpd gpio to input: %s",
           zx_status_get_string(config_in_response.error_value()));
    return config_in_response.take_error();
  }

  zx_status_t status = pin_gpio_irq_handler_.Begin(irq_handler_loop_.dispatcher());
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to bind the GPIO IRQ to the loop dispatcher: %s",
           zx_status_get_string(status));
    return zx::error(status);
  }

  status = irq_handler_loop_.StartThread("hot-plug-detection-interrupt-thread");
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to start a thread for the hotplug detection loop: %s",
           zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok();
}

// static
HotPlugDetectionState HotPlugDetection::GpioValueToState(uint8_t gpio_value) {
  return gpio_value ? HotPlugDetectionState::kDetected : HotPlugDetectionState::kNotDetected;
}

// static
fuchsia_hardware_gpio::wire::GpioPolarity HotPlugDetection::GpioPolarityForStateChange(
    HotPlugDetectionState current_state) {
  return (current_state == HotPlugDetectionState::kDetected)
             ? fuchsia_hardware_gpio::GpioPolarity::kLow
             : fuchsia_hardware_gpio::GpioPolarity::kHigh;
}

zx::result<> HotPlugDetection::UpdateState() {
  zx::result<HotPlugDetectionState> pin_state_result = ReadPinGpioState();
  if (pin_state_result.is_error()) {
    // ReadPinGpioState() already logged the error.
    return pin_state_result.take_error();
  }

  HotPlugDetectionState current_pin_state = pin_state_result.value();
  zx::result<> polarity_change_result;
  {
    fbl::AutoLock lock(&mutex_);
    if (current_pin_state == current_pin_state_) {
      return zx::ok();
    }
    current_pin_state_ = current_pin_state;

    polarity_change_result = SetPinGpioPolarity(GpioPolarityForStateChange(current_pin_state));
  }

  // We call the state change handler after setting the GPIO polarity so that
  // we don't miss a GPIO state change even if running the state change
  // handler takes a long time.
  on_state_change_(current_pin_state);

  return polarity_change_result;
}

void HotPlugDetection::InterruptHandler(async_dispatcher_t* dispatcher, async::IrqBase* irq,
                                        zx_status_t status,
                                        const zx_packet_interrupt_t* interrupt) {
  if (status == ZX_ERR_CANCELED) {
    zxlogf(INFO, "Hotplug interrupt wait is cancelled.");
    return;
  }
  if (status != ZX_OK) {
    zxlogf(ERROR, "Hotplug interrupt wait failed: %s", zx_status_get_string(status));
    // A failed async interrupt wait doesn't remove the interrupt from the
    // async loop, so we have to manually cancel it.
    irq->Cancel();
    return;
  }

  // Undocumented magic. Probably a very simple approximation of debouncing.
  usleep(500000);

  [[maybe_unused]] zx::result<> result = UpdateState();
  // UpdateState() already logged the error.

  // For interrupts bound to ports (including those bound to async loops), the
  // interrupt must be re-armed using zx_interrupt_ack() for each incoming
  // interrupt request. This is best done after the interrupt has been fully
  // processed.
  zx::unowned_interrupt(irq->object())->ack();
}

zx::result<HotPlugDetectionState> HotPlugDetection::ReadPinGpioState() {
  fidl::WireResult<fuchsia_hardware_gpio::Gpio::Read> read_result = pin_gpio_->Read();
  if (!read_result.ok()) {
    zxlogf(ERROR, "Failed to send Read request to pin GPIO: %s", read_result.status_string());
    return zx::error(read_result.status());
  }

  fidl::WireResultUnwrapType<fuchsia_hardware_gpio::Gpio::Read>& read_response =
      read_result.value();
  if (read_response.is_error()) {
    zxlogf(ERROR, "Failed to read pin GPIO: %s", zx_status_get_string(read_response.error_value()));
    return read_response.take_error();
  }
  return zx::ok(GpioValueToState(read_response->value));
}

zx::result<> HotPlugDetection::SetPinGpioPolarity(fuchsia_hardware_gpio::GpioPolarity polarity) {
  fidl::WireResult result = pin_gpio_->SetPolarity(polarity);
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send SetPolarity request to hpd gpio: %s", result.status_string());
    return result->take_error();
  }

  fidl::WireResultUnwrapType<fuchsia_hardware_gpio::Gpio::SetPolarity>& response = result.value();
  if (response.is_error()) {
    zxlogf(ERROR, "Failed to set polarity of hpd gpio: %s",
           zx_status_get_string(response.error_value()));
    return response.take_error();
  }
  return zx::ok();
}

}  // namespace amlogic_display
