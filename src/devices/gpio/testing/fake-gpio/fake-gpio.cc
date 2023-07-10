// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fake-gpio.h"

#include <lib/async/default.h>

#include <variant>

namespace fake_gpio {

bool WriteState::operator==(const WriteState& other) const { return value == other.value; }

bool ReadState::operator==(const ReadState& other) const { return flags == other.flags; }

bool AltFunctionState::operator==(const AltFunctionState& other) const {
  return function == other.function;
}

zx::result<uint8_t> DefaultReadCallback(FakeGpio& gpio) { return zx::error(ZX_ERR_NOT_SUPPORTED); }
zx_status_t DefaultWriteCallback(FakeGpio& gpio) { return ZX_OK; }

FakeGpio::FakeGpio() : read_callback_(DefaultReadCallback), write_callback_(DefaultWriteCallback) {
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
  state_log_.emplace_back(AltFunctionState{.function = request->function});
  completer.ReplySuccess();
}

void FakeGpio::ConfigIn(ConfigInRequestView request, ConfigInCompleter::Sync& completer) {
  if (state_log_.empty() || !std::holds_alternative<ReadState>(state_log_.back())) {
    state_log_.emplace_back(ReadState{.flags = request->flags});
  } else {
    auto& state = std::get<ReadState>(state_log_.back());
    state.flags = request->flags;
  }
  completer.ReplySuccess();
}

void FakeGpio::ConfigOut(ConfigOutRequestView request, ConfigOutCompleter::Sync& completer) {
  state_log_.emplace_back(WriteState{.value = request->initial_value});
  completer.ReplySuccess();
}

void FakeGpio::Write(WriteRequestView request, WriteCompleter::Sync& completer) {
  // Gpio must be configured to output in order to be written to.
  if (state_log_.empty() || !std::holds_alternative<WriteState>(state_log_.back())) {
    completer.ReplyError(ZX_ERR_BAD_STATE);
    return;
  }

  state_log_.emplace_back(WriteState{.value = request->value});
  zx_status_t response = write_callback_(*this);
  if (response == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(response);
  }
}

void FakeGpio::Read(ReadCompleter::Sync& completer) {
  ZX_ASSERT(std::holds_alternative<ReadState>(state_log_.back()));
  auto response = read_callback_(*this);
  if (response.is_ok()) {
    completer.ReplySuccess(response.value());
  } else {
    completer.ReplyError(response.error_value());
  }
}

void FakeGpio::ReleaseInterrupt(ReleaseInterruptCompleter::Sync& completer) {
  completer.ReplySuccess();
}

uint64_t FakeGpio::GetAltFunction() const {
  const auto& state = std::get<AltFunctionState>(state_log_.back());
  return state.function;
}

uint8_t FakeGpio::GetWriteValue() const {
  ZX_ASSERT(!state_log_.empty());
  const auto& state = std::get<WriteState>(state_log_.back());
  return state.value;
}

fuchsia_hardware_gpio::GpioFlags FakeGpio::GetReadFlags() const {
  const auto& state = std::get<ReadState>(state_log_.back());
  return state.flags;
}

void FakeGpio::SetInterrupt(zx::result<zx::interrupt> interrupt) {
  interrupt_ = std::move(interrupt);
}

void FakeGpio::SetReadCallback(ReadCallback read_callback) {
  read_callback_ = std::move(read_callback);
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

}  // namespace fake_gpio
