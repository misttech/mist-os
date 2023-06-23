// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fake-gpio.h"

#include <lib/async/default.h>

#include <variant>

namespace fake_gpio {

zx::result<uint8_t> DefaultReadCallback(FakeGpio& gpio) { return zx::error(ZX_ERR_NOT_SUPPORTED); }

FakeGpio::FakeGpio() : read_callback_(DefaultReadCallback) {
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
  states_.emplace_back(AltFunctionState{.function = request->function});
  completer.ReplySuccess();
}

void FakeGpio::ConfigIn(ConfigInRequestView request, ConfigInCompleter::Sync& completer) {
  if (states_.empty() || !std::holds_alternative<ReadState>(states_.back())) {
    states_.emplace_back(ReadState{.flags = request->flags});
  } else {
    auto& state = std::get<ReadState>(states_.back());
    state.flags = request->flags;
  }
  completer.ReplySuccess();
}

void FakeGpio::ConfigOut(ConfigOutRequestView request, ConfigOutCompleter::Sync& completer) {
  auto& state = EnsureCurrentStateIsWrite();
  state.values.emplace_back(request->initial_value);
  completer.ReplySuccess();
}

void FakeGpio::Write(WriteRequestView request, WriteCompleter::Sync& completer) {
  auto& state = EnsureCurrentStateIsWrite();
  state.values.emplace_back(request->value);
  completer.ReplySuccess();
}

void FakeGpio::Read(ReadCompleter::Sync& completer) {
  ZX_ASSERT(std::holds_alternative<ReadState>(states_.back()));
  auto response = read_callback_(*this);
  if (response.is_ok()) {
    completer.ReplySuccess(response.value());
  } else {
    completer.ReplyError(response.error_value());
  }
}

uint64_t FakeGpio::GetAltFunction() const {
  const auto& state = std::get<AltFunctionState>(states_.back());
  return state.function;
}

std::vector<uint8_t> FakeGpio::GetWriteValues() const {
  const auto& state = std::get<WriteState>(states_.back());
  return state.values;
}

uint8_t FakeGpio::GetCurrentWriteValue() const {
  const auto& state = std::get<WriteState>(states_.back());
  ZX_ASSERT(!state.values.empty());
  return state.values.back();
}

fuchsia_hardware_gpio::GpioFlags FakeGpio::GetReadFlags() const {
  const auto& state = std::get<ReadState>(states_.back());
  return state.flags;
}

void FakeGpio::SetInterrupt(zx::result<zx::interrupt> interrupt) {
  interrupt_ = std::move(interrupt);
}

void FakeGpio::SetReadCallback(ReadCallback read_callback) {
  read_callback_ = std::move(read_callback);
}

void FakeGpio::SetCurrentState(State state) { states_.push_back(std::move(state)); }

fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> FakeGpio::Connect() {
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_gpio::Gpio>();
  ZX_ASSERT(endpoints.is_ok());
  bindings_.AddBinding(async_get_default_dispatcher(), std::move(endpoints->server), this,
                       fidl::kIgnoreBindingClosure);
  return std::move(endpoints->client);
}

WriteState& FakeGpio::EnsureCurrentStateIsWrite() {
  if (states_.empty() || !std::holds_alternative<WriteState>(states_.back())) {
    states_.emplace_back(WriteState());
  }
  return std::get<WriteState>(states_.back());
}

}  // namespace fake_gpio
