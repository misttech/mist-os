// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gpio.h"

#include <fidl/fuchsia.scheduler/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>

#include <bind/fuchsia/cpp/bind.h>
#include <ddk/metadata/gpio.h>

#include "sdk/lib/driver/testing/cpp/driver_runtime.h"
#include "src/lib/testing/predicates/status.h"

namespace gpio {

namespace {

class MockGpioImpl : public fdf::WireServer<fuchsia_hardware_gpioimpl::GpioImpl> {
 public:
  static constexpr uint32_t kMaxInitStepPinIndex = 10;

  struct PinState {
    enum Mode { kUnknown, kIn, kOut };

    Mode mode = kUnknown;
    uint8_t value = UINT8_MAX;
    fuchsia_hardware_gpio::GpioFlags flags;
    uint64_t alt_function = UINT64_MAX;
    uint64_t drive_strength = UINT64_MAX;
  };

  PinState pin_state(uint32_t index) { return pin_state_internal(index); }
  void set_pin_state(uint32_t index, PinState state) { pin_state_internal(index) = state; }

  zx_status_t Serve(fdf::OutgoingDirectory& to_driver_vfs) {
    return to_driver_vfs
        .AddService<fuchsia_hardware_gpioimpl::Service>(
            fuchsia_hardware_gpioimpl::Service::InstanceHandler({
                .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                                  fidl::kIgnoreBindingClosure),
            }))
        .status_value();
  }

 private:
  PinState& pin_state_internal(uint32_t index) {
    if (index >= pins_.size()) {
      pins_.resize(index + 1);
    }
    return pins_[index];
  }

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_gpioimpl::GpioImpl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  void ConfigIn(fuchsia_hardware_gpioimpl::wire::GpioImplConfigInRequest* request,
                fdf::Arena& arena, ConfigInCompleter::Sync& completer) override {
    if (request->index > kMaxInitStepPinIndex) {
      return completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
    }
    pin_state_internal(request->index).mode = PinState::Mode::kIn;
    pin_state_internal(request->index).flags = request->flags;
    completer.buffer(arena).ReplySuccess();
  }
  void ConfigOut(fuchsia_hardware_gpioimpl::wire::GpioImplConfigOutRequest* request,
                 fdf::Arena& arena, ConfigOutCompleter::Sync& completer) override {
    if (request->index > kMaxInitStepPinIndex) {
      return completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
    }
    pin_state_internal(request->index).mode = PinState::Mode::kOut;
    pin_state_internal(request->index).value = request->initial_value;
    completer.buffer(arena).ReplySuccess();
  }
  void SetAltFunction(fuchsia_hardware_gpioimpl::wire::GpioImplSetAltFunctionRequest* request,
                      fdf::Arena& arena, SetAltFunctionCompleter::Sync& completer) override {
    if (request->index > kMaxInitStepPinIndex) {
      return completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
    }
    pin_state_internal(request->index).alt_function = request->function;
    completer.buffer(arena).ReplySuccess();
  }
  void Read(fuchsia_hardware_gpioimpl::wire::GpioImplReadRequest* request, fdf::Arena& arena,
            ReadCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess(pin_state_internal(request->index).value);
  }
  void Write(fuchsia_hardware_gpioimpl::wire::GpioImplWriteRequest* request, fdf::Arena& arena,
             WriteCompleter::Sync& completer) override {
    if (request->index > kMaxInitStepPinIndex) {
      return completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
    }
    pin_state_internal(request->index).value = request->value;
    completer.buffer(arena).ReplySuccess();
  }
  void SetPolarity(fuchsia_hardware_gpioimpl::wire::GpioImplSetPolarityRequest* request,
                   fdf::Arena& arena, SetPolarityCompleter::Sync& completer) override {}
  void SetDriveStrength(fuchsia_hardware_gpioimpl::wire::GpioImplSetDriveStrengthRequest* request,
                        fdf::Arena& arena, SetDriveStrengthCompleter::Sync& completer) override {
    if (request->index > kMaxInitStepPinIndex) {
      return completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
    }
    pin_state_internal(request->index).drive_strength = request->ds_ua;
    completer.buffer(arena).ReplySuccess(request->ds_ua);
  }
  void GetDriveStrength(fuchsia_hardware_gpioimpl::wire::GpioImplGetDriveStrengthRequest* request,
                        fdf::Arena& arena, GetDriveStrengthCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess(pin_state_internal(request->index).drive_strength);
  }
  void GetInterrupt(fuchsia_hardware_gpioimpl::wire::GpioImplGetInterruptRequest* request,
                    fdf::Arena& arena, GetInterruptCompleter::Sync& completer) override {}
  void ReleaseInterrupt(fuchsia_hardware_gpioimpl::wire::GpioImplReleaseInterruptRequest* request,
                        fdf::Arena& arena, ReleaseInterruptCompleter::Sync& completer) override {}
  void GetPins(fdf::Arena& arena, GetPinsCompleter::Sync& completer) override {}
  void GetInitSteps(fdf::Arena& arena, GetInitStepsCompleter::Sync& completer) override {}
  void GetControllerId(fdf::Arena& arena, GetControllerIdCompleter::Sync& completer) override {}

  fdf::ServerBindingGroup<fuchsia_hardware_gpioimpl::GpioImpl> bindings_;
  std::vector<PinState> pins_;
};

class GpioTestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    compat_.Init(component::kDefaultInstance, "root");
    EXPECT_OK(compat_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &to_driver_vfs));
    EXPECT_OK(gpioimpl_.Serve(to_driver_vfs));
    return zx::ok();
  }

  compat::DeviceServer& compat() { return compat_; }
  MockGpioImpl& gpioimpl() { return gpioimpl_; }

 private:
  compat::DeviceServer compat_;
  MockGpioImpl gpioimpl_;
};

class FixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = false;
  static constexpr bool kAutoStopDriver = false;

  using DriverType = GpioRootDevice;
  using EnvironmentType = GpioTestEnvironment;
};

class GpioTest : public fdf_testing::DriverTestFixture<FixtureConfig> {
 protected:
  MockGpioImpl::PinState pin_state(uint32_t index) {
    return RunInEnvironmentTypeContext<MockGpioImpl::PinState>(
        [index](GpioTestEnvironment& env) { return env.gpioimpl().pin_state(index); });
  }

  void set_pin_state(uint32_t index, MockGpioImpl::PinState state) {
    RunInEnvironmentTypeContext(
        [index, state](GpioTestEnvironment& env) { env.gpioimpl().set_pin_state(index, state); });
  }
};

TEST_F(GpioTest, TestFidlAll) {
  RunInEnvironmentTypeContext([](GpioTestEnvironment& env) {
    constexpr gpio_pin_t pins[] = {
        DECL_GPIO_PIN(1),
        DECL_GPIO_PIN(2),
        DECL_GPIO_PIN(3),
    };

    EXPECT_OK(env.compat().AddMetadata(DEVICE_METADATA_GPIO_PINS, pins,
                                       std::size(pins) * sizeof(gpio_pin_t)));
  });

  EXPECT_TRUE(StartDriver().is_ok());

  zx::result client_end = Connect<fuchsia_hardware_gpio::Service::Device>("gpio-1");
  EXPECT_TRUE(client_end.is_ok());

  fidl::WireClient<fuchsia_hardware_gpio::Gpio> gpio_client(
      *std::move(client_end), fdf::Dispatcher::GetCurrent()->async_dispatcher());

  set_pin_state(1, MockGpioImpl::PinState{.value = 20});
  gpio_client->Read().ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::Read>& result) {
        ASSERT_OK(result.status());
        EXPECT_EQ(result->value()->value, 20);
        runtime().Quit();
      });
  runtime().Run();
  runtime().ResetQuit();

  gpio_client->Write(11).ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::Write>& result) {
        EXPECT_OK(result.status());
        runtime().Quit();
      });
  runtime().Run();
  runtime().ResetQuit();

  EXPECT_EQ(pin_state(1).value, 11);

  gpio_client->ConfigIn(fuchsia_hardware_gpio::GpioFlags::kPullDown)
      .ThenExactlyOnce([&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::ConfigIn>& result) {
        EXPECT_OK(result.status());
        runtime().Quit();
      });
  runtime().Run();
  runtime().ResetQuit();

  EXPECT_EQ(pin_state(1).flags, fuchsia_hardware_gpio::GpioFlags::kPullDown);

  gpio_client->ConfigOut(5).ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::ConfigOut>& result) {
        EXPECT_OK(result.status());
        runtime().Quit();
      });
  runtime().Run();
  runtime().ResetQuit();

  EXPECT_EQ(pin_state(1).value, 5);

  gpio_client->SetDriveStrength(2000).ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::SetDriveStrength>& result) {
        ASSERT_OK(result.status());
        EXPECT_EQ(result->value()->actual_ds_ua, 2000ul);
        runtime().Quit();
      });
  runtime().Run();
  runtime().ResetQuit();

  EXPECT_EQ(pin_state(1).drive_strength, 2000ul);

  gpio_client->GetDriveStrength().ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::GetDriveStrength>& result) {
        ASSERT_OK(result.status());
        EXPECT_EQ(result->value()->result_ua, 2000ul);
        runtime().Quit();
      });
  runtime().Run();
  runtime().ResetQuit();

  EXPECT_TRUE(StopDriver().is_ok());
}

TEST_F(GpioTest, ValidateMetadataOk) {
  RunInEnvironmentTypeContext([](GpioTestEnvironment& env) {
    constexpr gpio_pin_t pins[] = {
        DECL_GPIO_PIN(1),
        DECL_GPIO_PIN(2),
        DECL_GPIO_PIN(3),
    };

    EXPECT_OK(env.compat().AddMetadata(DEVICE_METADATA_GPIO_PINS, pins,
                                       std::size(pins) * sizeof(gpio_pin_t)));
  });

  EXPECT_TRUE(StartDriver().is_ok());

  RunInNodeContext([](fdf_testing::TestNode& node) {
    ASSERT_EQ(node.children().count("gpio"), 1ul);
    EXPECT_EQ(node.children().at("gpio").children().count("gpio-1"), 1ul);
    EXPECT_EQ(node.children().at("gpio").children().count("gpio-2"), 1ul);
    EXPECT_EQ(node.children().at("gpio").children().count("gpio-3"), 1ul);
  });

  EXPECT_TRUE(StopDriver().is_ok());
}

TEST_F(GpioTest, ValidateMetadataRejectDuplicates) {
  RunInEnvironmentTypeContext([](GpioTestEnvironment& env) {
    constexpr gpio_pin_t pins[] = {
        DECL_GPIO_PIN(2),
        DECL_GPIO_PIN(1),
        DECL_GPIO_PIN(2),
        DECL_GPIO_PIN(0),
    };

    EXPECT_OK(env.compat().AddMetadata(DEVICE_METADATA_GPIO_PINS, pins,
                                       std::size(pins) * sizeof(gpio_pin_t)));
  });

  ASSERT_FALSE(StartDriver().is_ok());
}

TEST_F(GpioTest, ValidateGpioNameGeneration) {
  constexpr gpio_pin_t pins_digit[] = {
      DECL_GPIO_PIN(2),
      DECL_GPIO_PIN(5),
      DECL_GPIO_PIN((11)),
  };
  EXPECT_EQ(pins_digit[0].pin, 2u);
  EXPECT_STREQ(pins_digit[0].name, "2");
  EXPECT_EQ(pins_digit[1].pin, 5u);
  EXPECT_STREQ(pins_digit[1].name, "5");
  EXPECT_EQ(pins_digit[2].pin, 11u);
  EXPECT_STREQ(pins_digit[2].name, "(11)");

#define GPIO_TEST_NAME1 5
#define GPIO_TEST_NAME2 (6)
#define GPIO_TEST_NAME3_OF_63_CHRS_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890 7
  constexpr uint32_t GPIO_TEST_NAME4 = 8;  // constexpr should work too
#define GEN_GPIO0(x) ((x) + 1)
#define GEN_GPIO1(x) ((x) + 2)
  constexpr gpio_pin_t pins[] = {
      DECL_GPIO_PIN(GPIO_TEST_NAME1),
      DECL_GPIO_PIN(GPIO_TEST_NAME2),
      DECL_GPIO_PIN(GPIO_TEST_NAME3_OF_63_CHRS_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890),
      DECL_GPIO_PIN(GPIO_TEST_NAME4),
      DECL_GPIO_PIN(GEN_GPIO0(9)),
      DECL_GPIO_PIN(GEN_GPIO1(18)),
  };
  EXPECT_EQ(pins[0].pin, 5u);
  EXPECT_STREQ(pins[0].name, "GPIO_TEST_NAME1");
  EXPECT_EQ(pins[1].pin, 6u);
  EXPECT_STREQ(pins[1].name, "GPIO_TEST_NAME2");
  EXPECT_EQ(pins[2].pin, 7u);
  EXPECT_STREQ(pins[2].name, "GPIO_TEST_NAME3_OF_63_CHRS_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890");
  EXPECT_EQ(strlen(pins[2].name), GPIO_NAME_MAX_LENGTH - 1ul);
  EXPECT_EQ(pins[3].pin, 8u);
  EXPECT_STREQ(pins[3].name, "GPIO_TEST_NAME4");
  EXPECT_EQ(pins[4].pin, 10u);
  EXPECT_STREQ(pins[4].name, "GEN_GPIO0(9)");
  EXPECT_EQ(pins[5].pin, 20u);
  EXPECT_STREQ(pins[5].name, "GEN_GPIO1(18)");
#undef GPIO_TEST_NAME1
#undef GPIO_TEST_NAME2
#undef GPIO_TEST_NAME3_OF_63_CHRS_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890
#undef GEN_GPIO0
#undef GEN_GPIO1
}

TEST_F(GpioTest, Init) {
  namespace fhgpio = fuchsia_hardware_gpio::wire;
  namespace fhgpioimpl = fuchsia_hardware_gpioimpl::wire;

  constexpr gpio_pin_t kGpioPins[] = {
      DECL_GPIO_PIN(1),
      DECL_GPIO_PIN(2),
      DECL_GPIO_PIN(3),
  };

  fidl::Arena arena;

  fhgpioimpl::InitMetadata metadata;
  metadata.steps = fidl::VectorView<fhgpioimpl::InitStep>(arena, 18);

  metadata.steps[0].index = 1;
  metadata.steps[0].call = fhgpioimpl::InitCall::WithInputFlags(fhgpio::GpioFlags::kPullDown);

  metadata.steps[1].index = 1;
  metadata.steps[1].call = fhgpioimpl::InitCall::WithOutputValue(1);

  metadata.steps[2].index = 1;
  metadata.steps[2].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 4000);

  metadata.steps[3].index = 2;
  metadata.steps[3].call = fhgpioimpl::InitCall::WithInputFlags(fhgpio::GpioFlags::kNoPull);

  metadata.steps[4].index = 2;
  metadata.steps[4].call = fhgpioimpl::InitCall::WithAltFunction(arena, 5);

  metadata.steps[5].index = 2;
  metadata.steps[5].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 2000);

  metadata.steps[6].index = 3;
  metadata.steps[6].call = fhgpioimpl::InitCall::WithOutputValue(0);

  metadata.steps[7].index = 3;
  metadata.steps[7].call = fhgpioimpl::InitCall::WithOutputValue(1);

  metadata.steps[8].index = 3;
  metadata.steps[8].call = fhgpioimpl::InitCall::WithInputFlags(fhgpio::GpioFlags::kPullUp);

  metadata.steps[9].index = 2;
  metadata.steps[9].call = fhgpioimpl::InitCall::WithAltFunction(arena, 0);

  metadata.steps[10].index = 2;
  metadata.steps[10].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 1000);

  metadata.steps[11].index = 2;
  metadata.steps[11].call = fhgpioimpl::InitCall::WithOutputValue(1);

  metadata.steps[12].index = 1;
  metadata.steps[12].call = fhgpioimpl::InitCall::WithInputFlags(fhgpio::GpioFlags::kPullUp);

  metadata.steps[13].index = 1;
  metadata.steps[13].call = fhgpioimpl::InitCall::WithAltFunction(arena, 0);

  metadata.steps[14].index = 1;
  metadata.steps[14].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 4000);

  metadata.steps[15].index = 1;
  metadata.steps[15].call = fhgpioimpl::InitCall::WithOutputValue(1);

  metadata.steps[16].index = 3;
  metadata.steps[16].call = fhgpioimpl::InitCall::WithAltFunction(arena, 3);

  metadata.steps[17].index = 3;
  metadata.steps[17].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 2000);

  fit::result encoded = fidl::Persist(metadata);
  ASSERT_TRUE(encoded.is_ok());

  std::vector<uint8_t>& message = encoded.value();

  RunInEnvironmentTypeContext([&](GpioTestEnvironment& env) {
    EXPECT_OK(env.compat().AddMetadata(DEVICE_METADATA_GPIO_INIT, message.data(), message.size()));
    EXPECT_OK(env.compat().AddMetadata(DEVICE_METADATA_GPIO_PINS, kGpioPins, sizeof(kGpioPins)));
  });

  EXPECT_TRUE(StartDriver().is_ok());

  // Validate the final state of the pins with the init steps applied.
  EXPECT_EQ(pin_state(1).mode, MockGpioImpl::PinState::Mode::kOut);
  EXPECT_EQ(pin_state(1).flags, fuchsia_hardware_gpio::GpioFlags::kPullUp);
  EXPECT_EQ(pin_state(1).value, 1);
  EXPECT_EQ(pin_state(1).alt_function, 0ul);
  EXPECT_EQ(pin_state(1).drive_strength, 4000ul);

  EXPECT_EQ(pin_state(2).mode, MockGpioImpl::PinState::Mode::kOut);
  EXPECT_EQ(pin_state(2).flags, fuchsia_hardware_gpio::GpioFlags::kNoPull);
  EXPECT_EQ(pin_state(2).value, 1);
  EXPECT_EQ(pin_state(2).alt_function, 0ul);
  EXPECT_EQ(pin_state(2).drive_strength, 1000ul);

  EXPECT_EQ(pin_state(3).mode, MockGpioImpl::PinState::Mode::kIn);
  EXPECT_EQ(pin_state(3).flags, fuchsia_hardware_gpio::GpioFlags::kPullUp);
  EXPECT_EQ(pin_state(3).value, 1);
  EXPECT_EQ(pin_state(3).alt_function, 3ul);
  EXPECT_EQ(pin_state(3).drive_strength, 2000ul);

  EXPECT_TRUE(StopDriver().is_ok());
}

TEST_F(GpioTest, InitWithoutPins) {
  namespace fhgpio = fuchsia_hardware_gpio::wire;
  namespace fhgpioimpl = fuchsia_hardware_gpioimpl::wire;

  fidl::Arena arena;

  fhgpioimpl::InitMetadata metadata;
  metadata.steps = fidl::VectorView<fhgpioimpl::InitStep>(arena, 1);

  metadata.steps[0].index = 1;
  metadata.steps[0].call = fhgpioimpl::InitCall::WithInputFlags(fhgpio::GpioFlags::kPullDown);

  fit::result encoded = fidl::Persist(metadata);
  ASSERT_TRUE(encoded.is_ok());

  std::vector<uint8_t>& message = encoded.value();
  RunInEnvironmentTypeContext([&](GpioTestEnvironment& env) {
    EXPECT_OK(env.compat().AddMetadata(DEVICE_METADATA_GPIO_INIT, message.data(), message.size()));
  });

  EXPECT_TRUE(StartDriver().is_ok());

  EXPECT_EQ(pin_state(1).flags, fuchsia_hardware_gpio::GpioFlags::kPullDown);

  RunInNodeContext([](fdf_testing::TestNode& node) {
    EXPECT_EQ(node.children().count("gpio"), 1ul);
    EXPECT_EQ(node.children().count("gpio-init"), 1ul);
  });

  EXPECT_TRUE(StopDriver().is_ok());
}

TEST_F(GpioTest, InitErrorHandling) {
  namespace fhgpio = fuchsia_hardware_gpio::wire;
  namespace fhgpioimpl = fuchsia_hardware_gpioimpl::wire;

  constexpr gpio_pin_t kGpioPins[] = {
      DECL_GPIO_PIN(1),
      DECL_GPIO_PIN(2),
      DECL_GPIO_PIN(3),
  };

  fidl::Arena arena;

  fuchsia_hardware_gpioimpl::wire::InitMetadata metadata;
  metadata.steps = fidl::VectorView<fuchsia_hardware_gpioimpl::wire::InitStep>(arena, 9);

  metadata.steps[0].index = 4;
  metadata.steps[0].call = fhgpioimpl::InitCall::WithInputFlags(fhgpio::GpioFlags::kPullDown);

  metadata.steps[1].index = 4;
  metadata.steps[1].call = fhgpioimpl::InitCall::WithOutputValue(1);

  metadata.steps[2].index = 4;
  metadata.steps[2].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 4000);

  metadata.steps[3].index = 2;
  metadata.steps[3].call = fhgpioimpl::InitCall::WithInputFlags(fhgpio::GpioFlags::kNoPull);

  metadata.steps[4].index = 2;
  metadata.steps[4].call = fhgpioimpl::InitCall::WithAltFunction(arena, 5);

  metadata.steps[5].index = 2;
  metadata.steps[5].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 2000);

  // Using an index of 11 should cause the fake gpioimpl device to return an error.
  metadata.steps[6].index = MockGpioImpl::kMaxInitStepPinIndex + 1;
  metadata.steps[6].call = fhgpioimpl::InitCall::WithOutputValue(0);

  // Processing should not continue after the above error.

  metadata.steps[7].index = 2;
  metadata.steps[7].call = fhgpioimpl::InitCall::WithAltFunction(arena, 0);

  metadata.steps[8].index = 2;
  metadata.steps[8].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 1000);

  fit::result encoded = fidl::Persist(metadata);
  ASSERT_TRUE(encoded.is_ok());

  std::vector<uint8_t>& message = encoded.value();

  RunInEnvironmentTypeContext([&](GpioTestEnvironment& env) {
    EXPECT_OK(env.compat().AddMetadata(DEVICE_METADATA_GPIO_INIT, message.data(), message.size()));
    EXPECT_OK(env.compat().AddMetadata(DEVICE_METADATA_GPIO_PINS, kGpioPins, sizeof(kGpioPins)));
  });

  EXPECT_TRUE(StartDriver().is_ok());

  EXPECT_EQ(pin_state(2).mode, MockGpioImpl::PinState::Mode::kIn);
  EXPECT_EQ(pin_state(2).flags, fuchsia_hardware_gpio::GpioFlags::kNoPull);
  EXPECT_EQ(pin_state(2).alt_function, 5ul);
  EXPECT_EQ(pin_state(2).drive_strength, 2000ul);

  EXPECT_EQ(pin_state(4).mode, MockGpioImpl::PinState::Mode::kOut);
  EXPECT_EQ(pin_state(4).flags, fuchsia_hardware_gpio::GpioFlags::kPullDown);
  EXPECT_EQ(pin_state(4).value, 1);
  EXPECT_EQ(pin_state(4).drive_strength, 4000ul);

  // GPIO root device (init device should not be added due to errors).
  RunInNodeContext([](fdf_testing::TestNode& node) {
    EXPECT_EQ(node.children().count("gpio"), 1ul);
    EXPECT_EQ(node.children().count("gpio-init"), 0ul);
  });

  EXPECT_TRUE(StopDriver().is_ok());
}

TEST_F(GpioTest, ControllerId) {
  constexpr uint32_t kController = 5;

  constexpr gpio_pin_t pins[] = {
      DECL_GPIO_PIN(0),
      DECL_GPIO_PIN(1),
      DECL_GPIO_PIN(2),
  };

  fuchsia_hardware_gpioimpl::wire::ControllerMetadata controller_metadata = {.id = kController};
  const fit::result encoded_controller_metadata = fidl::Persist(controller_metadata);
  ASSERT_TRUE(encoded_controller_metadata.is_ok());

  RunInEnvironmentTypeContext([&](GpioTestEnvironment& env) {
    EXPECT_OK(env.compat().AddMetadata(DEVICE_METADATA_GPIO_PINS, pins,
                                       std::size(pins) * sizeof(gpio_pin_t)));
    EXPECT_OK(env.compat().AddMetadata(DEVICE_METADATA_GPIO_CONTROLLER,
                                       encoded_controller_metadata->data(),
                                       encoded_controller_metadata->size()));
  });

  EXPECT_TRUE(StartDriver().is_ok());

  RunInNodeContext([](fdf_testing::TestNode& node) {
    ASSERT_EQ(node.children().count("gpio"), 1ul);
    EXPECT_EQ(node.children().at("gpio").children().count("gpio-0"), 1ul);
    EXPECT_EQ(node.children().at("gpio").children().count("gpio-1"), 1ul);
    EXPECT_EQ(node.children().at("gpio").children().count("gpio-2"), 1ul);
  });

  for (const gpio_pin_t& pin : pins) {
    RunInNodeContext([&](fdf_testing::TestNode& node) {
      ASSERT_EQ(node.children().at("gpio").children().count(std::string{"gpio-"} + pin.name), 1ul);
      std::vector<fuchsia_driver_framework::NodeProperty> properties =
          node.children().at("gpio").children().at(std::string{"gpio-"} + pin.name).GetProperties();

      ASSERT_EQ(properties.size(), 2ul);

      ASSERT_TRUE(properties[0].key().string_value().has_value());
      EXPECT_EQ(properties[0].key().string_value().value(), bind_fuchsia::GPIO_PIN);

      ASSERT_TRUE(properties[0].value().int_value().has_value());
      EXPECT_EQ(properties[0].value().int_value().value(), pin.pin);

      ASSERT_TRUE(properties[1].key().string_value().has_value());
      EXPECT_EQ(properties[1].key().string_value().value(), bind_fuchsia::GPIO_CONTROLLER);

      ASSERT_TRUE(properties[1].value().int_value().has_value());
      EXPECT_EQ(properties[1].value().int_value().value(), kController);
    });
  }

  EXPECT_TRUE(StopDriver().is_ok());
}

TEST_F(GpioTest, SchedulerRole) {
  constexpr gpio_pin_t pins[] = {
      DECL_GPIO_PIN(0),
      DECL_GPIO_PIN(1),
      DECL_GPIO_PIN(2),
  };

  RunInEnvironmentTypeContext([&](GpioTestEnvironment& env) {
    EXPECT_OK(env.compat().AddMetadata(DEVICE_METADATA_GPIO_PINS, pins,
                                       std::size(pins) * sizeof(gpio_pin_t)));

    // Add scheduler role metadata that will cause the core driver to create a new driver
    // dispatcher. Verify that FIDL calls can still be made, and that dispatcher shutdown using the
    // unbind hook works.
    fuchsia_scheduler::RoleName role("no.such.scheduler.role");
    const auto result = fidl::Persist(role);
    ASSERT_TRUE(result.is_ok());
    EXPECT_OK(env.compat().AddMetadata(DEVICE_METADATA_SCHEDULER_ROLE_NAME, result->data(),
                                       result->size()));
  });

  EXPECT_TRUE(StartDriver().is_ok());

  RunInNodeContext([](fdf_testing::TestNode& node) {
    ASSERT_EQ(node.children().count("gpio"), 1ul);
    EXPECT_EQ(node.children().at("gpio").children().count("gpio-0"), 1ul);
    EXPECT_EQ(node.children().at("gpio").children().count("gpio-1"), 1ul);
    EXPECT_EQ(node.children().at("gpio").children().count("gpio-2"), 1ul);
  });

  for (const gpio_pin_t& pin : pins) {
    zx::result client_end =
        Connect<fuchsia_hardware_gpio::Service::Device>(std::string{"gpio-"} + pin.name);
    EXPECT_TRUE(client_end.is_ok());

    // Run the dispatcher to allow the connection to be established.
    runtime().RunUntilIdle();

    // The GPIO driver should be bound on a different dispatcher, so a sync client should work here.
    fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> gpio_client(*std::move(client_end));
    auto result = gpio_client->Write(1);
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  }

  EXPECT_TRUE(StopDriver().is_ok());
}

}  // namespace

}  // namespace gpio
