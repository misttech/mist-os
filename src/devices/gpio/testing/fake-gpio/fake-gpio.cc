// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fake-gpio.h"

#include <fidl/fuchsia.hardware.gpio/cpp/common_types.h>
#include <lib/async/default.h>

#include <variant>

namespace fake_gpio {

bool WriteSubState::operator==(const WriteSubState& other) const { return value == other.value; }

bool ReadSubState::operator==(const ReadSubState& other) const { return flags == other.flags; }

bool AltFunctionSubState::operator==(const AltFunctionSubState& other) const {
  return function == other.function;
}

zx_status_t DefaultWriteCallback(FakeGpio& gpio) { return ZX_OK; }

FakeGpio::FakeGpio() : write_callback_(DefaultWriteCallback) {
  zx::interrupt interrupt;
  ZX_ASSERT(zx::interrupt::create(zx::resource(ZX_HANDLE_INVALID), 0, ZX_INTERRUPT_VIRTUAL,
                                  &interrupt) == ZX_OK);
  interrupt_ = zx::ok(std::move(interrupt));
}

void FakeGpio::GetInterrupt(GetInterruptRequestView request,
                            GetInterruptCompleter::Sync& completer) {
  if (interrupt_.is_ok()) {
    zx::interrupt interrupt;
    ZX_ASSERT(interrupt_.value().duplicate(ZX_RIGHT_SAME_RIGHTS, &interrupt) == ZX_OK);
    completer.ReplySuccess(std::move(interrupt));
  } else {
    completer.ReplyError(interrupt_.error_value());
  }
}

void FakeGpio::SetAltFunction(SetAltFunctionRequestView request,
                              SetAltFunctionCompleter::Sync& completer) {
  state_log_.emplace_back(State{.polarity = GetCurrentPolarity(),
                                .sub_state = AltFunctionSubState{.function = request->function}});
  completer.ReplySuccess();
}

void FakeGpio::ConfigIn(ConfigInRequestView request, ConfigInCompleter::Sync& completer) {
  if (state_log_.empty() || !std::holds_alternative<ReadSubState>(state_log_.back().sub_state)) {
    state_log_.emplace_back(State{.polarity = GetCurrentPolarity(),
                                  .sub_state = ReadSubState{.flags = request->flags}});
  } else {
    auto& state = std::get<ReadSubState>(state_log_.back().sub_state);
    state.flags = request->flags;
  }
  completer.ReplySuccess();
}

void FakeGpio::ConfigOut(ConfigOutRequestView request, ConfigOutCompleter::Sync& completer) {
  state_log_.emplace_back(State{.polarity = GetCurrentPolarity(),
                                .sub_state = WriteSubState{.value = request->initial_value}});
  completer.ReplySuccess();
}

void FakeGpio::Write(WriteRequestView request, WriteCompleter::Sync& completer) {
  // Gpio must be configured to output in order to be written to.
  if (state_log_.empty() || !std::holds_alternative<WriteSubState>(state_log_.back().sub_state)) {
    completer.ReplyError(ZX_ERR_BAD_STATE);
    return;
  }

  state_log_.emplace_back(
      State{.polarity = GetCurrentPolarity(), .sub_state = WriteSubState{.value = request->value}});
  zx_status_t response = write_callback_(*this);
  if (response == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(response);
  }
}

void FakeGpio::Read(ReadCompleter::Sync& completer) {
  ZX_ASSERT(std::holds_alternative<ReadSubState>(state_log_.back().sub_state));
  zx::result<uint8_t> response;
  if (read_callbacks_.empty()) {
    ZX_ASSERT(default_read_response_.has_value());
    response = default_read_response_.value();
  } else {
    response = read_callbacks_.front()(*this);
    read_callbacks_.pop();
  }
  if (response.is_ok()) {
    completer.ReplySuccess(response.value());
  } else {
    completer.ReplyError(response.error_value());
  }
}

void FakeGpio::ReleaseInterrupt(ReleaseInterruptCompleter::Sync& completer) {
  completer.ReplySuccess();
}

void FakeGpio::SetPolarity(SetPolarityRequestView request, SetPolarityCompleter::Sync& completer) {
  auto sub_state = state_log_.empty()
                       ? ReadSubState{.flags = fuchsia_hardware_gpio::GpioFlags::kNoPull}
                       : state_log_.back().sub_state;
  state_log_.emplace_back(State{
      .polarity = request->polarity,
      .sub_state = std::move(sub_state),
  });
  completer.ReplySuccess();
}

uint64_t FakeGpio::GetAltFunction() const {
  const auto& state = std::get<AltFunctionSubState>(state_log_.back().sub_state);
  return state.function;
}

uint8_t FakeGpio::GetWriteValue() const {
  ZX_ASSERT(!state_log_.empty());
  const auto& state = std::get<WriteSubState>(state_log_.back().sub_state);
  return state.value;
}

fuchsia_hardware_gpio::GpioFlags FakeGpio::GetReadFlags() const {
  const auto& state = std::get<ReadSubState>(state_log_.back().sub_state);
  return state.flags;
}

fuchsia_hardware_gpio::GpioPolarity FakeGpio::GetPolarity() const {
  return state_log_.back().polarity;
}

void FakeGpio::SetInterrupt(zx::result<zx::interrupt> interrupt) {
  interrupt_ = std::move(interrupt);
}

void FakeGpio::PushReadCallback(ReadCallback callback) {
  read_callbacks_.push(std::move(callback));
}

void FakeGpio::PushReadResponse(zx::result<uint8_t> response) {
  read_callbacks_.push([response](FakeGpio& gpio) { return response; });
}

void FakeGpio::SetDefaultReadResponse(std::optional<zx::result<uint8_t>> response) {
  default_read_response_ = response;
}

void FakeGpio::SetWriteCallback(WriteCallback write_callback) {
  write_callback_ = std::move(write_callback);
}

void FakeGpio::SetCurrentState(State state) { state_log_.push_back(std::move(state)); }

std::vector<State> FakeGpio::GetStateLog() { return state_log_; }

fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> FakeGpio::Connect() {
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_gpio::Gpio>();
  ZX_ASSERT(endpoints.is_ok());
  bindings_.AddBinding(async_get_default_dispatcher(), std::move(endpoints->server), this,
                       fidl::kIgnoreBindingClosure);
  return std::move(endpoints->client);
}

fuchsia_hardware_gpio::Service::InstanceHandler FakeGpio::CreateInstanceHandler() {
  auto* dispatcher = async_get_default_dispatcher();
  Handler device_handler = [impl = this, dispatcher = dispatcher](
                               ::fidl::ServerEnd<::fuchsia_hardware_gpio::Gpio> request) {
    impl->bindings_.AddBinding(dispatcher, std::move(request), impl, fidl::kIgnoreBindingClosure);
  };

  return fuchsia_hardware_gpio::Service::InstanceHandler({.device = std::move(device_handler)});
}

fuchsia_hardware_gpio::GpioPolarity FakeGpio::GetCurrentPolarity() {
  if (state_log_.empty()) {
    return fuchsia_hardware_gpio::GpioPolarity::kHigh;
  }
  return state_log_.back().polarity;
}

}  // namespace fake_gpio
