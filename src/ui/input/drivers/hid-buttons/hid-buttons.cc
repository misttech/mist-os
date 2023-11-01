// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "hid-buttons.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/zx/clock.h>
#include <string.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/port.h>
#include <zircon/types.h>

#include <cstdint>
#include <memory>

#include <ddk/metadata/buttons.h>
#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>

namespace buttons {

namespace {

uint32_t to_bit_mask(ButtonType type) { return 1 << static_cast<uint8_t>(type); }

// Takes in a BUTTON_ID_ value and returns a bitmask of ButtonTypes that are associated with this
// button id. Bit position corresponds to ButtonType e.g (1 << ButtonType::kVolumeUp) is the bit
// for the volume_up button type.
uint32_t ButtonIdToButtonTypeBitMask(uint8_t button_id) {
  switch (button_id) {
    case BUTTONS_ID_VOLUME_UP:
      return to_bit_mask(ButtonType::kVolumeUp);
    case BUTTONS_ID_VOLUME_DOWN:
      return to_bit_mask(ButtonType::kVolumeDown);
    case BUTTONS_ID_FDR:
      return to_bit_mask(ButtonType::kReset);
    case BUTTONS_ID_MIC_MUTE:
      return to_bit_mask(ButtonType::kMute);
    case BUTTONS_ID_PLAY_PAUSE:
      return to_bit_mask(ButtonType::kPlayPause);
    case BUTTONS_ID_KEY_A:
      return to_bit_mask(ButtonType::kKeyA);
    case BUTTONS_ID_KEY_M:
      return to_bit_mask(ButtonType::kKeyM);
    case BUTTONS_ID_CAM_MUTE:
      return to_bit_mask(ButtonType::kCamMute);
    case BUTTONS_ID_MIC_AND_CAM_MUTE:
      return to_bit_mask(ButtonType::kCamMute) | to_bit_mask(ButtonType::kMute);
    default:
      return 0;
  }
}

}  // namespace

void HidButtonsDevice::ButtonsInputReport::ToFidlInputReport(
    fidl::WireTableBuilder<::fuchsia_input_report::wire::InputReport>& input_report,
    fidl::AnyArena& allocator) {
  fidl::VectorView<fuchsia_input_report::wire::ConsumerControlButton> buttons_rpt(
      allocator, fuchsia_input_report::wire::kConsumerControlMaxNumButtons);
  size_t count = 0;
  bool mic_mute = false;
  bool cam_mute = false;
  for (uint32_t id = 0; id < buttons.size(); id++) {
    if (!buttons[id]) {
      continue;
    }

    switch (id) {
      case BUTTONS_ID_VOLUME_UP:
        buttons_rpt[count] = fuchsia_input_report::ConsumerControlButton::kVolumeUp;
        count++;
        break;
      case BUTTONS_ID_VOLUME_DOWN:
        buttons_rpt[count] = fuchsia_input_report::ConsumerControlButton::kVolumeDown;
        count++;
        break;
      case BUTTONS_ID_FDR:
        buttons_rpt[count] = fuchsia_input_report::ConsumerControlButton::kFactoryReset;
        count++;
        break;
      case BUTTONS_ID_MIC_MUTE:
        if (!mic_mute) {
          buttons_rpt[count] = fuchsia_input_report::ConsumerControlButton::kMicMute;
          count++;
          mic_mute = true;
        }
        break;
      case BUTTONS_ID_CAM_MUTE:
        if (!cam_mute) {
          buttons_rpt[count] = fuchsia_input_report::ConsumerControlButton::kCameraDisable;
          count++;
          cam_mute = true;
        }
        break;
      case BUTTONS_ID_MIC_AND_CAM_MUTE:
        if (!mic_mute) {
          buttons_rpt[count] = fuchsia_input_report::ConsumerControlButton::kMicMute;
          count++;
          mic_mute = true;
        }
        if (!cam_mute) {
          buttons_rpt[count] = fuchsia_input_report::ConsumerControlButton::kCameraDisable;
          count++;
          cam_mute = true;
        }
        break;
      default:
        zxlogf(ERROR, "Invalid Button ID encountered %d", id);
    }
  }
  buttons_rpt.set_count(count);

  auto consumer_control =
      fuchsia_input_report::wire::ConsumerControlInputReport::Builder(allocator).pressed_buttons(
          buttons_rpt);
  input_report.event_time(event_time.get()).consumer_control(consumer_control.Build());
}

void HidButtonsDevice::Notify(uint32_t button_index) {
  // HID Report
  auto result = GetInputReport();
  if (result.is_error()) {
    zxlogf(ERROR, "HidbusGetReport failed %d", result.error_value());
  } else if (last_report_ != result.value()) {
    readers_.SendReportToAllReaders(result.value());
    last_report_ = result.value();

    if (debounce_states_[button_index].timestamp != zx::time::infinite_past()) {
      const zx::duration latency =
          zx::clock::get_monotonic() - debounce_states_[button_index].timestamp;

      total_latency_ += latency;
      report_count_++;
      average_latency_usecs_.Set(total_latency_.to_usecs() / report_count_);

      if (latency > max_latency_) {
        max_latency_ = latency;
        max_latency_usecs_.Set(max_latency_.to_usecs());
      }
    }

    if (!last_report_.empty()) {
      total_report_count_.Add(1);
      last_event_timestamp_.Set(last_report_.event_time.get());
    }
  }
  if (buttons_[button_index].id == BUTTONS_ID_FDR) {
    zxlogf(INFO, "FDR (up and down buttons) pressed");
  }

  // Notify anyone registered for this ButtonType.
  {
    fbl::AutoLock lock(&channels_lock_);
    uint32_t types = ButtonIdToButtonTypeBitMask(buttons_[button_index].id);
    bool button_value = debounce_states_[button_index].value;
    // Go through each ButtonType and send notifications.
    for (uint8_t raw_type = 0; raw_type < static_cast<uint8_t>(ButtonType::kMax); raw_type++) {
      if ((types & (1 << raw_type)) == 0) {
        continue;
      }

      ButtonType type = static_cast<ButtonType>(raw_type);
      for (ButtonsNotifyInterface* interface : registered_notifiers_[type]) {
        auto result = fidl::WireSendEvent(interface->binding())->OnNotify(type, button_value);
        if (!result.ok())
          zxlogf(ERROR, "OnNotify() failed: %s", result.FormatDescription().c_str());
      }
    }
  }

  debounce_states_[button_index].enqueued = false;
  debounce_states_[button_index].timestamp = zx::time::infinite_past();
}

int HidButtonsDevice::Thread() {
  thread_started_.Signal();
  if (poll_period_ != zx::duration::infinite()) {
    poll_timer_.set(zx::deadline_after(poll_period_), zx::duration(0));
    poll_timer_.wait_async(port_, kPortKeyPollTimer, ZX_TIMER_SIGNALED, 0);
  }

  while (1) {
    zx_port_packet_t packet;
    zx_status_t status = port_.wait(zx::time::infinite(), &packet);
    zxlogf(DEBUG, "msg received on port key %lu", packet.key);
    if (status != ZX_OK) {
      zxlogf(ERROR, "port wait failed %d", status);
      return thrd_error;
    }

    if (packet.key == kPortKeyShutDown) {
      zxlogf(INFO, "shutting down");
      return thrd_success;
    }

    if (packet.key >= kPortKeyInterruptStart &&
        packet.key < (kPortKeyInterruptStart + buttons_.size())) {
      uint32_t type = static_cast<uint32_t>(packet.key - kPortKeyInterruptStart);
      if (gpios_[type].config.type == BUTTONS_GPIO_TYPE_INTERRUPT) {
        // We need to reconfigure the GPIO to catch the opposite polarity.
        debounce_states_[type].value = ReconfigurePolarity(type, packet.key);

        // Notify
        debounce_states_[type].timer.set(zx::deadline_after(zx::duration(kDebounceThresholdNs)),
                                         zx::duration(0));
        if (!debounce_states_[type].enqueued) {
          debounce_states_[type].timer.wait_async(port_, kPortKeyTimerStart + type,
                                                  ZX_TIMER_SIGNALED, 0);
          debounce_states_[type].timestamp = zx::time(packet.interrupt.timestamp);
        }
        debounce_states_[type].enqueued = true;
      }

      gpios_[type].irq.ack();
    }

    if (packet.key >= kPortKeyTimerStart && packet.key < (kPortKeyTimerStart + buttons_.size())) {
      Notify(static_cast<uint32_t>(packet.key - kPortKeyTimerStart));
    }

    if (packet.key == kPortKeyPollTimer) {
      for (size_t i = 0; i < gpios_.size(); i++) {
        if (gpios_[i].config.type != BUTTONS_GPIO_TYPE_POLL) {
          continue;
        }

        fidl::WireResult read_result = gpios_[i].client->Read();
        if (!read_result.ok()) {
          zxlogf(ERROR, "Failed to send Read request to gpio %lu: %s", i,
                 read_result.status_string());
          return read_result.status();
        }
        if (read_result->is_error()) {
          zxlogf(ERROR, "Failed to read gpio %lu: %s", i,
                 zx_status_get_string(read_result->error_value()));
          return read_result->error_value();
        }
        if (!!read_result.value()->value != debounce_states_[i].value) {
          Notify(i);
        }
        debounce_states_[i].value = read_result.value()->value;
      }

      poll_timer_.set(zx::deadline_after(poll_period_), zx::duration(0));
      poll_timer_.wait_async(port_, kPortKeyPollTimer, ZX_TIMER_SIGNALED, 0);
    }
  }
  return thrd_success;
}

void HidButtonsDevice::GetInputReportsReader(GetInputReportsReaderRequestView request,
                                             GetInputReportsReaderCompleter::Sync& completer) {
  auto status = readers_.CreateReader(dispatcher_, std::move(request->reader));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to create a reader %d", status);
  }
}

void HidButtonsDevice::GetDescriptor(GetDescriptorCompleter::Sync& completer) {
  fidl::Arena<kFeatureAndDescriptorBufferSize> arena;

  fuchsia_input_report::wire::DeviceInfo device_info;
  device_info.vendor_id = static_cast<uint32_t>(fuchsia_input_report::VendorId::kGoogle);
  device_info.product_id =
      static_cast<uint32_t>(fuchsia_input_report::VendorGoogleProductId::kHidButtons);

  const std::vector<fuchsia_input_report::ConsumerControlButton> buttons = {
      fuchsia_input_report::ConsumerControlButton::kVolumeUp,
      fuchsia_input_report::ConsumerControlButton::kVolumeDown,
      fuchsia_input_report::ConsumerControlButton::kFactoryReset,
      fuchsia_input_report::ConsumerControlButton::kCameraDisable,
      fuchsia_input_report::ConsumerControlButton::kMicMute};

  const auto input = fuchsia_input_report::wire::ConsumerControlInputDescriptor::Builder(arena)
                         .buttons(buttons)
                         .Build();

  const auto consumer_control =
      fuchsia_input_report::wire::ConsumerControlDescriptor::Builder(arena).input(input).Build();

  completer.Reply(fuchsia_input_report::wire::DeviceDescriptor::Builder(arena)
                      .device_info(device_info)
                      .consumer_control(consumer_control)
                      .Build());
}

// Requires interrupts to be disabled for all rows/cols.
bool HidButtonsDevice::MatrixScan(uint32_t row, uint32_t col, zx_duration_t delay) {
  auto& gpio_col = gpios_[col];
  {
    fidl::WireResult result = gpio_col.client->ConfigIn(
        fuchsia_hardware_gpio::GpioFlags::kNoPull);  // Float column to find row in use.
    if (!result.ok()) {
      zxlogf(ERROR, "Failed to send ConfigIn request to gpio %u: %s", col, result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "Failed to configuire gpio %u to input: %s", col,
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }
  zx::nanosleep(zx::deadline_after(zx::duration(delay)));

  fidl::WireResult read_result = gpios_[row].client->Read();
  if (!read_result.ok()) {
    zxlogf(ERROR, "Failed to send Read request to gpio %u: %s", row, read_result.status_string());
    return read_result.status();
  }
  if (read_result->is_error()) {
    zxlogf(ERROR, "Failed to read gpio %u: %s", row,
           zx_status_get_string(read_result->error_value()));
    return read_result->error_value();
  }

  {
    fidl::WireResult result = gpio_col.client->ConfigOut(gpio_col.config.matrix.output_value);
    if (!result.ok()) {
      zxlogf(ERROR, "Failed to send ConfigOut request to gpio %u: %s", col, result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "Failed to configuire gpio %u to output: %s", col,
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }
  zxlogf(DEBUG, "row %u col %u val %u", row, col, read_result.value()->value);
  return static_cast<bool>(read_result.value()->value);
}

zx::result<HidButtonsDevice::ButtonsInputReport> HidButtonsDevice::GetInputReport() {
  ButtonsInputReport input_rpt;

  for (size_t i = 0; i < buttons_.size(); ++i) {
    bool new_value = false;  // A value true means a button is pressed.
    if (buttons_[i].type == BUTTONS_TYPE_MATRIX) {
      new_value = MatrixScan(buttons_[i].gpioA_idx, buttons_[i].gpioB_idx, buttons_[i].gpio_delay);
    } else if (buttons_[i].type == BUTTONS_TYPE_DIRECT) {
      auto gpio_index = buttons_[i].gpioA_idx;
      fidl::WireResult read_result = gpios_[gpio_index].client->Read();
      if (!read_result.ok()) {
        zxlogf(ERROR, "Failed to send Read request to gpio %u: %s", gpio_index,
               read_result.status_string());
        return zx::error(read_result.status());
      }
      if (read_result->is_error()) {
        zxlogf(ERROR, "Failed to read gpio %u: %s", gpio_index,
               zx_status_get_string(read_result->error_value()));
        return zx::error(read_result->error_value());
      }

      new_value = read_result.value()->value;
      zxlogf(DEBUG, "GPIO direct read %u for button %lu", new_value, i);
    } else {
      zxlogf(ERROR, "unknown button type %u", buttons_[i].type);
      return zx::error(ZX_ERR_INTERNAL);
    }

    if (gpios_[i].config.flags & BUTTONS_GPIO_FLAG_INVERTED) {
      new_value = !new_value;
    }

    zxlogf(DEBUG, "GPIO new value %u for button %lu", new_value, i);
    input_rpt.set(buttons_[i].id, new_value);
  }
  input_rpt.event_time = zx::clock::get_monotonic();

  return zx::ok(input_rpt);
}

void HidButtonsDevice::GetInputReport(GetInputReportRequestView request,
                                      GetInputReportCompleter::Sync& completer) {
  if (request->device_type != fuchsia_input_report::DeviceType::kConsumerControl) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  auto result = GetInputReport();
  if (!result.is_ok()) {
    completer.ReplyError(result.error_value());
    return;
  }

  fidl::Arena<> arena;
  auto input_report = fuchsia_input_report::wire::InputReport::Builder(arena);
  result->ToFidlInputReport(input_report, arena);
  completer.ReplySuccess(input_report.Build());
}

uint8_t HidButtonsDevice::ReconfigurePolarity(uint32_t idx, uint64_t int_port) {
  zxlogf(DEBUG, "gpio %u port %lu", idx, int_port);
  uint8_t current = 0, old;
  auto& gpio = gpios_[idx];

  fidl::WireResult read_result1 = gpio.client->Read();
  if (!read_result1.ok()) {
    zxlogf(ERROR, "Failed to send Read request to gpio %u: %s", idx, read_result1.status_string());
    return read_result1.status();
  }
  if (read_result1->is_error()) {
    zxlogf(ERROR, "Failed to read gpio %u: %s", idx,
           zx_status_get_string(read_result1->error_value()));
    return read_result1->error_value();
  }
  current = read_result1.value()->value;

  do {
    {
      fidl::WireResult result =
          gpio.client->SetPolarity(current ? fuchsia_hardware_gpio::GpioPolarity::kLow
                                           : fuchsia_hardware_gpio::GpioPolarity::kHigh);
      if (!result.ok()) {
        zxlogf(ERROR, "Failed to send SetPolarity request to gpio %u: %s", idx,
               result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        zxlogf(ERROR, "Failed to set polarity of gpio %u: %s", idx,
               zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    }

    old = current;
    fidl::WireResult read_result2 = gpio.client->Read();
    if (!read_result2.ok()) {
      zxlogf(ERROR, "Failed to send Read request to gpio %u: %s", idx,
             read_result2.status_string());
      return read_result2.status();
    }
    if (read_result2->is_error()) {
      zxlogf(ERROR, "Failed to read gpio %u: %s", idx,
             zx_status_get_string(read_result2->error_value()));
      return read_result2->error_value();
    }
    current = read_result2.value()->value;
    zxlogf(TRACE, "old gpio %u new gpio %u", old, current);
    // If current switches after setup, we setup a new trigger for it (opposite edge).
  } while (current != old);
  return current;
}

zx_status_t HidButtonsDevice::ConfigureInterrupt(uint32_t idx, uint64_t int_port) {
  zxlogf(DEBUG, "gpio %u port %lu", idx, int_port);
  zx_status_t status;
  uint8_t current = 0;
  auto& gpio = gpios_[idx];

  fidl::WireResult read_result = gpio.client->Read();
  if (!read_result.ok()) {
    zxlogf(ERROR, "Failed to send Read request to gpio %u: %s", idx, read_result.status_string());
    return read_result.status();
  }
  if (read_result->is_error()) {
    zxlogf(ERROR, "Failed to read gpio %u: %s", idx,
           zx_status_get_string(read_result->error_value()));
    return read_result->error_value();
  }
  current = read_result.value()->value;

  {
    fidl::WireResult result = gpio.client->ReleaseInterrupt();
    if (!result.ok()) {
      zxlogf(ERROR, "Failed to send ReleaseInterrupt request to gpio %u: %s", idx,
             result.status_string());
      return result.status();
    }
    if (result->is_error() && result->error_value() != ZX_ERR_NOT_FOUND) {
      zxlogf(ERROR, "Failed to release interrupt for gpio %u: %s", idx,
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  // We setup a trigger for the opposite of the current GPIO value.
  fidl::WireResult interrupt_result =
      gpio.client->GetInterrupt(current ? ZX_INTERRUPT_MODE_EDGE_LOW : ZX_INTERRUPT_MODE_EDGE_HIGH);
  if (!interrupt_result.ok()) {
    zxlogf(ERROR, "Failed to send GetInterrupt request to gpio %u: %s", idx,
           interrupt_result.status_string());
    return interrupt_result.status();
  }
  if (interrupt_result->is_error()) {
    zxlogf(ERROR, "Failed to get interrupt for gpio %u: %s", idx,
           zx_status_get_string(interrupt_result->error_value()));
    return interrupt_result->error_value();
  }
  gpio.irq = std::move(interrupt_result.value()->irq);

  status = gpios_[idx].irq.bind(port_, int_port, 0);
  if (status != ZX_OK) {
    zxlogf(ERROR, "zx_interrupt_bind failed %d", status);
    return status;
  }
  // To make sure polarity is correct in case it changed during configuration.
  ReconfigurePolarity(idx, int_port);
  return ZX_OK;
}

zx_status_t HidButtonsDevice::Bind(fbl::Array<Gpio> gpios,
                                   fbl::Array<buttons_button_config_t> buttons) {
  {
    fbl::AutoLock lock(&channels_lock_);
    for (uint8_t raw_type = 0; raw_type < static_cast<uint8_t>(ButtonType::kMax); raw_type++) {
      ButtonType type = static_cast<ButtonType>(raw_type);
      registered_notifiers_[type] = std::set<ButtonsNotifyInterface*>();
    }
  }
  zx_status_t status;

  buttons_ = std::move(buttons);
  gpios_ = std::move(gpios);
  fbl::AllocChecker ac;
  fbl::AutoLock lock(&channels_lock_);

  status = zx::port::create(ZX_PORT_BIND_TO_INTERRUPT, &port_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "port_create failed %d", status);
    return status;
  }

  debounce_states_ = fbl::Array(new (&ac) debounce_state[buttons_.size()], buttons_.size());
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  for (auto& i : debounce_states_) {
    i.enqueued = false;
    zx::timer::create(0, ZX_CLOCK_MONOTONIC, &(i.timer));
    i.value = false;
  }

  zx::timer::create(0, ZX_CLOCK_MONOTONIC, &poll_timer_);

  // Check the metadata.
  for (uint32_t i = 0; i < buttons_.size(); ++i) {
    if (buttons_[i].gpioA_idx >= gpios_.size()) {
      zxlogf(ERROR, "invalid gpioA_idx %u", buttons_[i].gpioA_idx);
      return ZX_ERR_INTERNAL;
    }
    if (buttons_[i].gpioB_idx >= gpios_.size()) {
      zxlogf(ERROR, "invalid gpioB_idx %u", buttons_[i].gpioB_idx);
      return ZX_ERR_INTERNAL;
    }
    if (gpios_[buttons_[i].gpioA_idx].config.type != BUTTONS_GPIO_TYPE_INTERRUPT &&
        gpios_[buttons_[i].gpioA_idx].config.type != BUTTONS_GPIO_TYPE_POLL) {
      zxlogf(ERROR, "invalid gpioA type %u", gpios_[buttons_[i].gpioA_idx].config.type);
      return ZX_ERR_INTERNAL;
    }
    if (buttons_[i].type == BUTTONS_TYPE_MATRIX &&
        gpios_[buttons_[i].gpioB_idx].config.type != BUTTONS_GPIO_TYPE_MATRIX_OUTPUT) {
      zxlogf(ERROR, "invalid matrix gpioB type %u", gpios_[buttons_[i].gpioB_idx].config.type);
      return ZX_ERR_INTERNAL;
    }
    if (buttons_[i].id == BUTTONS_ID_FDR) {
      zxlogf(INFO, "FDR (up and down buttons) setup to GPIO %u", buttons_[i].gpioA_idx);
    }
    if (gpios_[buttons_[i].gpioA_idx].config.type == BUTTONS_GPIO_TYPE_POLL) {
      const auto button_poll_period =
          zx::duration(gpios_[buttons_[i].gpioA_idx].config.poll.period);
      if (poll_period_ == zx::duration::infinite()) {
        poll_period_ = button_poll_period;
      }
      if (button_poll_period != poll_period_) {
        zxlogf(ERROR, "GPIOs must have the same poll period");
        return ZX_ERR_INTERNAL;
      }
    }

    // Update the button_map_ array which maps ButtonTypes to the button.
    uint32_t types = ButtonIdToButtonTypeBitMask(buttons_[i].id);
    for (uint8_t raw_type = 0; raw_type < static_cast<uint8_t>(ButtonType::kMax); raw_type++) {
      if ((types & (1 << raw_type)) == 0) {
        continue;
      }

      ButtonType type = static_cast<ButtonType>(raw_type);
      button_map_[type] = i;
    }
  }

  // Setup.
  for (uint32_t i = 0; i < gpios_.size(); ++i) {
    auto& gpio = gpios_[i];
    {
      fidl::WireResult result = gpio.client->SetAltFunction(0);  // 0 means function GPIO.
      if (!result.ok()) {
        zxlogf(ERROR, "Failed to send SetAltFunction request to gpio %u: %s", i,
               result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        zxlogf(ERROR, "Failed to set alt function for gpio %u: %s", i,
               zx_status_get_string(result->error_value()));
        return ZX_ERR_NOT_SUPPORTED;
      }
    }

    if (gpio.config.type == BUTTONS_GPIO_TYPE_MATRIX_OUTPUT) {
      fidl::WireResult result = gpio.client->ConfigOut(gpio.config.matrix.output_value);
      if (!result.ok()) {
        zxlogf(ERROR, "Failed to send ConfigOut request to gpio %u: %s", i, result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        zxlogf(ERROR, "Failed to configure gpio %u to output: %s", i,
               zx_status_get_string(result->error_value()));
        return ZX_ERR_NOT_SUPPORTED;
      }
    } else if (gpio.config.type == BUTTONS_GPIO_TYPE_INTERRUPT) {
      fidl::WireResult result = gpio.client->ConfigIn(
          static_cast<fuchsia_hardware_gpio::GpioFlags>(gpio.config.interrupt.internal_pull));
      if (!result.ok()) {
        zxlogf(ERROR, "Failed to send ConfigIn request to gpio %u: %s", i, result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        zxlogf(ERROR, "Failed to configure gpio %u to input: %s", i,
               zx_status_get_string(result->error_value()));
        return ZX_ERR_NOT_SUPPORTED;
      }
      status = ConfigureInterrupt(i, kPortKeyInterruptStart + i);
      if (status != ZX_OK) {
        return status;
      }
    } else if (gpio.config.type == BUTTONS_GPIO_TYPE_POLL) {
      fidl::WireResult result = gpio.client->ConfigIn(
          static_cast<fuchsia_hardware_gpio::GpioFlags>(gpio.config.interrupt.internal_pull));
      if (!result.ok()) {
        zxlogf(ERROR, "Failed to send ConfigIn request to gpio %u: %s", i, result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        zxlogf(ERROR, "Failed to configure gpio %u to input: %s", i,
               zx_status_get_string(result->error_value()));
        return ZX_ERR_NOT_SUPPORTED;
      }
    }
  }

  auto result = GetInputReport();
  if (result.is_error()) {
    zxlogf(ERROR, "GetInputReport failed %d", status);
  } else {
    last_report_ = result.value();
  }

  auto f = [](void* arg) -> int { return reinterpret_cast<HidButtonsDevice*>(arg)->Thread(); };
  int rc = thrd_create_with_name(&thread_, f, this, "hid-buttons-thread");
  if (rc != thrd_success) {
    return ZX_ERR_INTERNAL;
  }

  status = DdkAdd(ddk::DeviceAddArgs("hid-buttons")
                      .set_inspect_vmo(inspector_.DuplicateVmo())
                      .set_flags(DEVICE_ADD_NON_BINDABLE));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed %d", status);
    ShutDown();
    return status;
  }

  return ZX_OK;
}

void HidButtonsDevice::ShutDown() {
  zx_port_packet packet = {kPortKeyShutDown, ZX_PKT_TYPE_USER, ZX_OK, {}};
  zx_status_t status = port_.queue(&packet);
  ZX_ASSERT(status == ZX_OK);
  thread_started_.Wait();
  thrd_join(thread_, NULL);
  for (auto& gpio : gpios_) {
    gpio.irq.destroy();
  }
}

void HidButtonsDevice::DdkUnbind(ddk::UnbindTxn txn) {
  ShutDown();
  txn.Reply();
}

void HidButtonsDevice::DdkRelease() { delete this; }

static zx_status_t hid_buttons_bind(void* ctx, zx_device_t* parent) {
  fbl::AllocChecker ac;
  auto dev = fbl::make_unique_checked<buttons::HidButtonsDevice>(
      &ac, parent, fdf_dispatcher_get_async_dispatcher(fdf_dispatcher_get_current_dispatcher()));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  // Get buttons metadata.
  auto temp_buttons =
      ddk::GetMetadataArray<buttons_button_config_t>(parent, DEVICE_METADATA_BUTTONS_BUTTONS);
  if (!temp_buttons.is_ok()) {
    return temp_buttons.error_value();
  }
  auto buttons = fbl::MakeArray<buttons_button_config_t>(&ac, temp_buttons->size());
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  std::copy(temp_buttons->begin(), temp_buttons->end(), buttons.begin());

  // Get gpios metadata.
  auto configs =
      ddk::GetMetadataArray<buttons_gpio_config_t>(parent, DEVICE_METADATA_BUTTONS_GPIOS);
  if (!configs.is_ok()) {
    return configs.error_value();
  }
  size_t n_gpios = configs->size();

  // Prepare gpios array.
  auto gpios = fbl::Array(new (&ac) HidButtonsDevice::Gpio[n_gpios], n_gpios);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  for (uint32_t i = 0; i < n_gpios; ++i) {
    const char* name;
    switch (buttons[i].id) {
      case BUTTONS_ID_VOLUME_UP:
        name = "volume-up";
        break;
      case BUTTONS_ID_VOLUME_DOWN:
        name = "volume-down";
        break;
      case BUTTONS_ID_FDR:
        name = "volume-both";
        break;
      case BUTTONS_ID_MIC_MUTE:
      case BUTTONS_ID_MIC_AND_CAM_MUTE:
        name = "mic-privacy";
        break;
      case BUTTONS_ID_CAM_MUTE:
        name = "cam-mute";
        break;
      default:
        return ZX_ERR_NOT_SUPPORTED;
    };
    zx::result gpio_client =
        ddk::Device<void>::DdkConnectFragmentFidlProtocol<fuchsia_hardware_gpio::Service::Device>(
            parent, name);
    if (gpio_client.is_error()) {
      zxlogf(ERROR, "Failed to get gpio protocol from fragment %s: %s", name,
             gpio_client.status_string());
      return ZX_ERR_INTERNAL;
    }
    gpios[i].client.Bind(std::move(gpio_client.value()));
    gpios[i].config = configs.value()[i];
  }

  zx_status_t status = dev->Bind(std::move(gpios), std::move(buttons));
  if (status == ZX_OK) {
    // devmgr is now in charge of the memory for dev.
    [[maybe_unused]] auto ptr = dev.release();
  }
  return status;
}

bool HidButtonsDevice::GetState(ButtonType type) {
  auto gpio_index = buttons_[button_map_[type]].gpioA_idx;
  auto& gpio = gpios_[gpio_index];
  fidl::WireResult result = gpio.client->Read();
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send Read request to gpio %u: %s", gpio_index, result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to read gpio %u: %s", gpio_index,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return static_cast<bool>(result.value()->value);
}

zx_status_t HidButtonsDevice::RegisterNotify(uint8_t types, ButtonsNotifyInterface* notify) {
  fbl::AutoLock lock(&channels_lock_);
  // Go through each type in ButtonType and update our registration.
  for (uint8_t raw_type = 0; raw_type < static_cast<uint8_t>(ButtonType::kMax); raw_type++) {
    ButtonType type = static_cast<ButtonType>(raw_type);
    auto& notify_set = registered_notifiers_[type];
    if ((types & (1 << raw_type)) == 0) {
      // Our type does not exist in the bitmask and so we should de-register.
      notify_set.erase(notify);
    } else {
      // Our type exists in the bitmask and so we should register.
      notify_set.insert(notify);
    }
  }
  return ZX_OK;
}

void HidButtonsDevice::ClosingChannel(ButtonsNotifyInterface* notify) {
  fbl::AutoLock lock(&channels_lock_);
  // Remove this notifier from anything it's registered to listen to.
  for (auto& [type, notify_set] : registered_notifiers_) {
    notify_set.erase(notify);
  }

  // release ownership
  for (auto iter = interfaces_.begin(); iter != interfaces_.end(); ++iter) {
    if (&(*iter) == notify) {
      interfaces_.erase(iter);
      return;
    }
  }
  zxlogf(ERROR, "interfaces_ could not find channel");
}

static constexpr zx_driver_ops_t hid_buttons_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = hid_buttons_bind;
  return ops;
}();

}  // namespace buttons

ZIRCON_DRIVER(hid_buttons, buttons::hid_buttons_driver_ops, "zircon", "0.1");
