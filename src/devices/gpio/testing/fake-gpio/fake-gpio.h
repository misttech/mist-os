// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_GPIO_TESTING_FAKE_GPIO_FAKE_GPIO_H_
#define SRC_DEVICES_GPIO_TESTING_FAKE_GPIO_FAKE_GPIO_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire_test_base.h>
#include <lib/zx/interrupt.h>

#include <vector>

namespace fake_gpio {

// Contains information specific to when a GPIO has been configured for
// output.
struct WriteState {
  // Value that the GPIO has been set to output.
  uint8_t value;

  bool operator==(const WriteState& other) const;
};

// Contains information specific to when a GPIO has been configured for input.
struct ReadState {
  fuchsia_hardware_gpio::GpioFlags flags;

  bool operator==(const ReadState& other) const;
};

// Contains information specific to when a GPIO has been configured to perform
// an alternative function.
struct AltFunctionState {
  uint64_t function;

  bool operator==(const AltFunctionState& other) const;
};

// Represents every possible state of a GPIO.
using State = std::variant<WriteState, ReadState, AltFunctionState>;

template <typename T>
bool operator==(const T& state1, const State& state2) {
  const T* alternative = std::get_if<T>(&state2);
  return alternative != nullptr && state1 == *alternative;
}

template <typename T>
bool operator==(const State& state1, const T& state2) {
  return state2 == state1;
}

class FakeGpio;

using ReadCallback = std::function<zx::result<uint8_t>(FakeGpio&)>;

class FakeGpio : public fidl::testing::WireTestBase<fuchsia_hardware_gpio::Gpio> {
 public:
  FakeGpio();

  // fidl::testing::WireTestBase<fuchsia_hardware_gpu::Gpio>
  void GetInterrupt(GetInterruptRequestView request,
                    GetInterruptCompleter::Sync& completer) override;
  void SetAltFunction(SetAltFunctionRequestView request,
                      SetAltFunctionCompleter::Sync& completer) override;
  void ConfigIn(ConfigInRequestView request, ConfigInCompleter::Sync& completer) override;
  void ConfigOut(ConfigOutRequestView request, ConfigOutCompleter::Sync& completer) override;
  void Write(WriteRequestView request, WriteCompleter::Sync& completer) override;
  void Read(ReadCompleter::Sync& completer) override;
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  // Get the function set by `SetAltFunction`. Will fail if the current state
  // isn't `AltFunction.`
  uint64_t GetAltFunction() const;

  // Get the value being written by the gpio. Will fail if the current state
  // isn't `Write`.
  uint8_t GetWriteValue() const;

  // Get the read flags set by `ConfigIn`. Will fail if the current state isn't
  // `Read`.
  fuchsia_hardware_gpio::GpioFlags GetReadFlags() const;

  // Set the interrupt used for GetInterrupt requests to `interrupt`.
  void SetInterrupt(zx::result<zx::interrupt> interrupt);

  // Set the callback used for handling Read requests to `read_callback`.
  void SetReadCallback(ReadCallback read_callback);

  // Set the current state to `state`.
  void SetCurrentState(State state);

  // Return the states the gpio has been set to in chronological order.
  std::vector<State> GetStateLog();

  // Serve the gpio FIDL protocol on the current dispatcher and return a client
  // end that can communicate with the server.
  fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> Connect();

 private:
  // Contains the states that the gpio has been set to in chronological order.
  std::vector<State> state_log_;

  // Callback that provides the value to respond to `Read` requests with.
  ReadCallback read_callback_;

  // Interrupt used for GetInterrupt requests.
  zx::result<zx::interrupt> interrupt_;

  fidl::ServerBindingGroup<fuchsia_hardware_gpio::Gpio> bindings_;
};

}  // namespace fake_gpio

#endif  // SRC_DEVICES_GPIO_TESTING_FAKE_GPIO_FAKE_GPIO_H_
