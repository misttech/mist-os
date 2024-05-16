// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gpio.h"

#include <fidl/fuchsia.scheduler/cpp/fidl.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>

#include <optional>

#include <ddk/metadata/gpio.h>
#include <fbl/alloc_checker.h>
#include <zxtest/zxtest.h>

#include "sdk/lib/async_patterns/testing/cpp/dispatcher_bound.h"
#include "sdk/lib/driver/testing/cpp/driver_runtime.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

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

  explicit MockGpioImpl(fdf::UnownedSynchronizedDispatcher dispatcher, uint32_t controller = 0)
      : dispatcher_(std::move(dispatcher)),
        controller_(controller),
        outgoing_(dispatcher_->get()) {}

  // Helper method to make this easier to use from a DispatcherBound.
  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> CreateOutgoingAndServe() {
    fdf::Unowned dispatcher = fdf::Dispatcher::GetCurrent();

    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }

    auto service = outgoing_.AddService<fuchsia_hardware_gpioimpl::Service>(
        fuchsia_hardware_gpioimpl::Service::InstanceHandler({
            .device = bind_handler(dispatcher->get()),
        }));
    if (service.is_error()) {
      return service.take_error();
    }

    if (auto result = outgoing_.Serve(std::move(endpoints->server)); result.is_error()) {
      return result.take_error();
    }

    return zx::ok(std::move(endpoints->client));
  }

  PinState pin_state(uint32_t index) { return pin_state_internal(index); }
  void set_pin_state(uint32_t index, PinState state) { pin_state_internal(index) = state; }

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
  void GetControllerId(fdf::Arena& arena, GetControllerIdCompleter::Sync& completer) override {
    completer.buffer(arena).Reply(controller_);
  }

  const fdf::UnownedSynchronizedDispatcher dispatcher_;
  const uint32_t controller_;
  fdf::OutgoingDirectory outgoing_;

  std::vector<PinState> pins_;
};

class GpioTest : public zxtest::Test {
 public:
  GpioTest()
      : runtime_(mock_ddk::GetDriverRuntime()),
        gpioimpl_dispatcher_(runtime_->StartBackgroundDispatcher()),
        parent_(MockDevice::FakeRootParent()),
        gpioimpl_(gpioimpl_dispatcher_->async_dispatcher(), std::in_place,
                  gpioimpl_dispatcher_->borrow()) {}

  void SetUp() override {
    auto client = gpioimpl_.SyncCall(&MockGpioImpl::CreateOutgoingAndServe);
    ASSERT_TRUE(client.is_ok());
    parent_->AddFidlService(fuchsia_hardware_gpioimpl::Service::Name, *std::move(client));
  }

 protected:
  std::shared_ptr<fdf_testing::DriverRuntime> runtime_;
  fdf::UnownedSynchronizedDispatcher gpioimpl_dispatcher_;
  std::shared_ptr<MockDevice> parent_;
  async_patterns::TestDispatcherBound<MockGpioImpl> gpioimpl_;
};

TEST_F(GpioTest, TestFidlAll) {
  constexpr gpio_pin_t pins[] = {
      DECL_GPIO_PIN(1),
      DECL_GPIO_PIN(2),
      DECL_GPIO_PIN(3),
  };
  parent_->SetMetadata(DEVICE_METADATA_GPIO_PINS, pins, std::size(pins) * sizeof(gpio_pin_t));

  EXPECT_OK(GpioRootDevice::Create(nullptr, parent_.get()));
  ASSERT_EQ(parent_->child_count(), 1);
  ASSERT_EQ(parent_->GetLatestChild()->child_count(), 3);

  const auto path =
      std::string("svc/") +
      component::MakeServiceMemberPath<fuchsia_hardware_gpio::Service::Device>("default");
  auto client_end = component::ConnectAt<fuchsia_hardware_gpio::Gpio>(
      (*parent_->GetLatestChild()->children().begin())->outgoing(), path);
  EXPECT_TRUE(client_end.is_ok());

  fidl::WireClient<fuchsia_hardware_gpio::Gpio> gpio_client(
      *std::move(client_end), fdf::Dispatcher::GetCurrent()->async_dispatcher());

  gpioimpl_.SyncCall(&MockGpioImpl::set_pin_state, 1, MockGpioImpl::PinState{.value = 20});
  gpio_client->Read().ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::Read>& result) {
        ASSERT_OK(result.status());
        EXPECT_EQ(result->value()->value, 20);
        runtime_->Quit();
      });
  runtime_->Run();
  runtime_->ResetQuit();

  gpio_client->Write(11).ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::Write>& result) {
        EXPECT_OK(result.status());
        runtime_->Quit();
      });
  runtime_->Run();
  runtime_->ResetQuit();

  EXPECT_EQ(gpioimpl_.SyncCall(&MockGpioImpl::pin_state, 1).value, 11);

  gpio_client->ConfigIn(fuchsia_hardware_gpio::GpioFlags::kPullDown)
      .ThenExactlyOnce([&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::ConfigIn>& result) {
        EXPECT_OK(result.status());
        runtime_->Quit();
      });
  runtime_->Run();
  runtime_->ResetQuit();

  EXPECT_EQ(gpioimpl_.SyncCall(&MockGpioImpl::pin_state, 1).flags,
            fuchsia_hardware_gpio::GpioFlags::kPullDown);

  gpio_client->ConfigOut(5).ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::ConfigOut>& result) {
        EXPECT_OK(result.status());
        runtime_->Quit();
      });
  runtime_->Run();
  runtime_->ResetQuit();

  EXPECT_EQ(gpioimpl_.SyncCall(&MockGpioImpl::pin_state, 1).value, 5);

  gpio_client->SetDriveStrength(2000).ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::SetDriveStrength>& result) {
        ASSERT_OK(result.status());
        EXPECT_EQ(result->value()->actual_ds_ua, 2000);
        runtime_->Quit();
      });
  runtime_->Run();
  runtime_->ResetQuit();

  EXPECT_EQ(gpioimpl_.SyncCall(&MockGpioImpl::pin_state, 1).drive_strength, 2000);

  gpio_client->GetDriveStrength().ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::GetDriveStrength>& result) {
        ASSERT_OK(result.status());
        EXPECT_EQ(result->value()->result_ua, 2000);
        runtime_->Quit();
      });
  runtime_->Run();
  runtime_->ResetQuit();

  device_async_remove(parent_->GetLatestChild());
  runtime_->PerformBlockingWork(
      [&, dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher()]() {
        EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(parent_->GetLatestChild(), dispatcher));
      });
}

TEST_F(GpioTest, ValidateMetadataOk) {
  constexpr gpio_pin_t pins[] = {
      DECL_GPIO_PIN(0),
      DECL_GPIO_PIN(1),
      DECL_GPIO_PIN(2),
  };
  parent_->SetMetadata(DEVICE_METADATA_GPIO_PINS, pins, std::size(pins) * sizeof(gpio_pin_t));

  EXPECT_OK(GpioRootDevice::Create(nullptr, parent_.get()));
  ASSERT_EQ(parent_->child_count(), 1);
  EXPECT_EQ(parent_->GetLatestChild()->child_count(), 3);

  device_async_remove(parent_->GetLatestChild());
  runtime_->PerformBlockingWork(
      [&, dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher()]() {
        EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(parent_->GetLatestChild(), dispatcher));
      });
}

TEST_F(GpioTest, ValidateMetadataRejectDuplicates) {
  constexpr gpio_pin_t pins[] = {
      DECL_GPIO_PIN(2),
      DECL_GPIO_PIN(1),
      DECL_GPIO_PIN(2),
      DECL_GPIO_PIN(0),
  };
  parent_->SetMetadata(DEVICE_METADATA_GPIO_PINS, pins, std::size(pins) * sizeof(gpio_pin_t));

  ASSERT_NOT_OK(GpioRootDevice::Create(nullptr, parent_.get()));
}

TEST(GpioTest, ValidateGpioNameGeneration) {
  constexpr gpio_pin_t pins_digit[] = {
      DECL_GPIO_PIN(2),
      DECL_GPIO_PIN(5),
      DECL_GPIO_PIN((11)),
  };
  EXPECT_EQ(pins_digit[0].pin, 2);
  EXPECT_STREQ(pins_digit[0].name, "2");
  EXPECT_EQ(pins_digit[1].pin, 5);
  EXPECT_STREQ(pins_digit[1].name, "5");
  EXPECT_EQ(pins_digit[2].pin, 11);
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
  EXPECT_EQ(pins[0].pin, 5);
  EXPECT_STREQ(pins[0].name, "GPIO_TEST_NAME1");
  EXPECT_EQ(pins[1].pin, 6);
  EXPECT_STREQ(pins[1].name, "GPIO_TEST_NAME2");
  EXPECT_EQ(pins[2].pin, 7);
  EXPECT_STREQ(pins[2].name, "GPIO_TEST_NAME3_OF_63_CHRS_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890");
  EXPECT_EQ(strlen(pins[2].name), GPIO_NAME_MAX_LENGTH - 1);
  EXPECT_EQ(pins[3].pin, 8);
  EXPECT_STREQ(pins[3].name, "GPIO_TEST_NAME4");
  EXPECT_EQ(pins[4].pin, 10);
  EXPECT_STREQ(pins[4].name, "GEN_GPIO0(9)");
  EXPECT_EQ(pins[5].pin, 20);
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
  ASSERT_TRUE(encoded.is_ok(), "%s", encoded.error_value().FormatDescription().c_str());

  std::vector<uint8_t>& message = encoded.value();
  parent_->SetMetadata(DEVICE_METADATA_GPIO_INIT, message.data(), message.size());
  parent_->SetMetadata(DEVICE_METADATA_GPIO_PINS, kGpioPins, sizeof(kGpioPins));

  EXPECT_OK(GpioRootDevice::Create(nullptr, parent_.get()));

  // Validate the final state of the pins with the init steps applied.
  gpioimpl_.SyncCall([&](MockGpioImpl* gpioimpl) {
    EXPECT_EQ(gpioimpl->pin_state(1).mode, MockGpioImpl::PinState::Mode::kOut);
    EXPECT_EQ(gpioimpl->pin_state(1).flags, fuchsia_hardware_gpio::GpioFlags::kPullUp);
    EXPECT_EQ(gpioimpl->pin_state(1).value, 1);
    EXPECT_EQ(gpioimpl->pin_state(1).alt_function, 0);
    EXPECT_EQ(gpioimpl->pin_state(1).drive_strength, 4000);

    EXPECT_EQ(gpioimpl->pin_state(2).mode, MockGpioImpl::PinState::Mode::kOut);
    EXPECT_EQ(gpioimpl->pin_state(2).flags, fuchsia_hardware_gpio::GpioFlags::kNoPull);
    EXPECT_EQ(gpioimpl->pin_state(2).value, 1);
    EXPECT_EQ(gpioimpl->pin_state(2).alt_function, 0);
    EXPECT_EQ(gpioimpl->pin_state(2).drive_strength, 1000);

    EXPECT_EQ(gpioimpl->pin_state(3).mode, MockGpioImpl::PinState::Mode::kIn);
    EXPECT_EQ(gpioimpl->pin_state(3).flags, fuchsia_hardware_gpio::GpioFlags::kPullUp);
    EXPECT_EQ(gpioimpl->pin_state(3).value, 1);
    EXPECT_EQ(gpioimpl->pin_state(3).alt_function, 3);
    EXPECT_EQ(gpioimpl->pin_state(3).drive_strength, 2000);
  });

  // GPIO init and root devices.
  EXPECT_EQ(parent_->child_count(), 2);
  for (auto& child : parent_->children()) {
    device_async_remove(child.get());
  }

  // ReleaseFlaggedDevices blocks, so it must be run on another thread while the foreground
  // dispatcher runs on this one. Pass the foreground dispatcher to it so that the unbind hooks run
  // on a foreground dispatcher thread as the driver expects.
  runtime_->PerformBlockingWork(
      [&, dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher()]() {
        EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(parent_.get(), dispatcher));
      });
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
  ASSERT_TRUE(encoded.is_ok(), "%s", encoded.error_value().FormatDescription().c_str());

  std::vector<uint8_t>& message = encoded.value();
  parent_->SetMetadata(DEVICE_METADATA_GPIO_INIT, message.data(), message.size());

  EXPECT_OK(GpioRootDevice::Create(nullptr, parent_.get()));

  gpioimpl_.SyncCall([&](MockGpioImpl* gpioimpl) {
    EXPECT_EQ(gpioimpl->pin_state(1).flags, fuchsia_hardware_gpio::GpioFlags::kPullDown);
  });

  // GPIO init and root devices.
  EXPECT_EQ(parent_->child_count(), 2);
  for (auto& child : parent_->children()) {
    device_async_remove(child.get());
  }

  // ReleaseFlaggedDevices blocks, so it must be run on another thread while the foreground
  // dispatcher runs on this one. Pass the foreground dispatcher to it so that the unbind hooks run
  // on a foreground dispatcher thread as the driver expects.
  runtime_->PerformBlockingWork(
      [&, dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher()]() {
        EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(parent_.get(), dispatcher));
      });
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
  ASSERT_TRUE(encoded.is_ok(), "%s", encoded.error_value().FormatDescription().c_str());

  std::vector<uint8_t>& message = encoded.value();
  parent_->SetMetadata(DEVICE_METADATA_GPIO_INIT, message.data(), message.size());
  parent_->SetMetadata(DEVICE_METADATA_GPIO_PINS, kGpioPins, sizeof(kGpioPins));

  EXPECT_OK(GpioRootDevice::Create(nullptr, parent_.get()));

  gpioimpl_.SyncCall([&](MockGpioImpl* gpioimpl) {
    EXPECT_EQ(gpioimpl->pin_state(2).mode, MockGpioImpl::PinState::Mode::kIn);
    EXPECT_EQ(gpioimpl->pin_state(2).flags, fuchsia_hardware_gpio::GpioFlags::kNoPull);
    EXPECT_EQ(gpioimpl->pin_state(2).alt_function, 5);
    EXPECT_EQ(gpioimpl->pin_state(2).drive_strength, 2000);

    EXPECT_EQ(gpioimpl->pin_state(4).mode, MockGpioImpl::PinState::Mode::kOut);
    EXPECT_EQ(gpioimpl->pin_state(4).flags, fuchsia_hardware_gpio::GpioFlags::kPullDown);
    EXPECT_EQ(gpioimpl->pin_state(4).value, 1);
    EXPECT_EQ(gpioimpl->pin_state(4).drive_strength, 4000);
  });

  // GPIO root device (init device should not be added due to errors).
  EXPECT_EQ(parent_->child_count(), 1);
  device_async_remove(parent_->GetLatestChild());
  runtime_->PerformBlockingWork(
      [&, dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher()]() {
        EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(parent_.get(), dispatcher));
      });
}

TEST(GpioTest, ControllerId) {
  constexpr uint32_t kController = 5;

  constexpr gpio_pin_t pins[] = {
      DECL_GPIO_PIN(0),
      DECL_GPIO_PIN(1),
      DECL_GPIO_PIN(2),
  };

  std::shared_ptr runtime = mock_ddk::GetDriverRuntime();
  fdf::UnownedSynchronizedDispatcher background_dispatcher = runtime->StartBackgroundDispatcher();

  auto parent = MockDevice::FakeRootParent();
  parent->SetMetadata(DEVICE_METADATA_GPIO_PINS, pins, std::size(pins) * sizeof(gpio_pin_t));

  fuchsia_hardware_gpioimpl::wire::ControllerMetadata controller_metadata = {.id = kController};
  const fit::result encoded_controller_metadata = fidl::Persist(controller_metadata);
  ASSERT_TRUE(encoded_controller_metadata.is_ok());

  parent->SetMetadata(DEVICE_METADATA_GPIO_CONTROLLER, encoded_controller_metadata->data(),
                      encoded_controller_metadata->size());

  async_patterns::TestDispatcherBound<MockGpioImpl> gpioimpl_fidl(
      background_dispatcher->async_dispatcher(), std::in_place, background_dispatcher->borrow());

  {
    auto outgoing_client = gpioimpl_fidl.SyncCall(&MockGpioImpl::CreateOutgoingAndServe);
    ASSERT_TRUE(outgoing_client.is_ok());

    parent->AddFidlService(fuchsia_hardware_gpioimpl::Service::Name, std::move(*outgoing_client));
  }

  ASSERT_OK(GpioRootDevice::Create(nullptr, parent.get()));

  const auto path =
      std::string("svc/") +
      component::MakeServiceMemberPath<fuchsia_hardware_gpio::Service::Device>("default");

  ASSERT_EQ(parent->child_count(), 1);
  auto* const root_device = parent->GetLatestChild();

  ASSERT_EQ(root_device->child_count(), 3);
  for (const auto& child : root_device->children()) {
    const cpp20::span properties = child->GetProperties();
    ASSERT_EQ(properties.size(), 2);

    EXPECT_EQ(properties[0].id, BIND_GPIO_PIN);
    EXPECT_GE(properties[0].value, 0);
    EXPECT_LE(properties[0].value, 2);

    EXPECT_EQ(properties[1].id, BIND_GPIO_CONTROLLER);
    EXPECT_EQ(properties[1].value, kController);

    auto client_end = component::ConnectAt<fuchsia_hardware_gpio::Gpio>(child->outgoing(), path);
    ASSERT_TRUE(client_end.is_ok());

    fidl::WireClient<fuchsia_hardware_gpio::Gpio> gpio_client(
        *std::move(client_end), fdf::Dispatcher::GetCurrent()->async_dispatcher());

    // Make a call that results in a synchronous FIDL call to the mock GPIO. Without this, it is
    // possible that server binding has not completed by the time the driver runtime goes out of
    // scope.
    gpio_client->Write(0).Then([&](auto& result) {
      ASSERT_TRUE(result.ok());
      EXPECT_TRUE(result->is_ok());
      runtime->Quit();
    });
    runtime->Run();
    runtime->ResetQuit();
  }

  async_dispatcher_t* const driver_dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();

  device_async_remove(root_device);
  runtime->PerformBlockingWork(
      [&]() { EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(root_device, driver_dispatcher)); });
}

TEST(GpioTest, SchedulerRole) {
  constexpr gpio_pin_t pins[] = {
      DECL_GPIO_PIN(0),
      DECL_GPIO_PIN(1),
      DECL_GPIO_PIN(2),
  };

  std::shared_ptr runtime = mock_ddk::GetDriverRuntime();
  async_dispatcher_t* const driver_dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
  fdf::UnownedSynchronizedDispatcher const background_dispatcher =
      runtime->StartBackgroundDispatcher();

  auto parent = MockDevice::FakeRootParent();
  parent->SetMetadata(DEVICE_METADATA_GPIO_PINS, pins, std::size(pins) * sizeof(gpio_pin_t));

  {
    // Add scheduler role metadata that will cause the core driver to create a new driver
    // dispatcher. Verify that FIDL calls can still be made, and that dispatcher shutdown using the
    // unbind hook works.
    fuchsia_scheduler::RoleName role("no.such.scheduler.role");
    const auto result = fidl::Persist(role);
    ASSERT_TRUE(result.is_ok());
    parent->SetMetadata(DEVICE_METADATA_SCHEDULER_ROLE_NAME, result->data(), result->size());
  }

  async_patterns::TestDispatcherBound<MockGpioImpl> gpioimpl_fidl(
      background_dispatcher->async_dispatcher(), std::in_place, background_dispatcher->borrow(), 0);

  {
    auto outgoing_client = gpioimpl_fidl.SyncCall(&MockGpioImpl::CreateOutgoingAndServe);
    ASSERT_TRUE(outgoing_client.is_ok());

    parent->AddFidlService(fuchsia_hardware_gpioimpl::Service::Name, std::move(*outgoing_client));
  }

  ASSERT_OK(GpioRootDevice::Create(nullptr, parent.get()));

  runtime->RunUntil([&]() -> bool {
    return parent->child_count() == 1 && parent->GetLatestChild()->child_count() == 3;
  });

  ASSERT_EQ(parent->child_count(), 1);

  auto* const root_device = parent->GetLatestChild();
  EXPECT_EQ(root_device->child_count(), 3);

  const auto path =
      std::string("svc/") +
      component::MakeServiceMemberPath<fuchsia_hardware_gpio::Service::Device>("default");

  for (auto& child : root_device->children()) {
    auto client_end = component::ConnectAt<fuchsia_hardware_gpio::Gpio>(child->outgoing(), path);
    ASSERT_TRUE(client_end.is_ok());

    fidl::WireClient<fuchsia_hardware_gpio::Gpio> gpio_client(*std::move(client_end),
                                                              driver_dispatcher);
    gpio_client->Write(1).Then(
        [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::Write>& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
          runtime->Quit();
        });

    runtime->Run();
    runtime->ResetQuit();
  }

  device_async_remove(root_device);
  runtime->PerformBlockingWork(
      [&]() { EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(root_device, driver_dispatcher)); });
}

}  // namespace

}  // namespace gpio
