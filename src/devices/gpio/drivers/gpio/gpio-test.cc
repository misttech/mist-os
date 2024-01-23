// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gpio.h"

#include <fuchsia/hardware/gpioimpl/cpp/banjo-mock.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>

#include <optional>

#include <ddk/metadata/gpio.h>
#include <fbl/alloc_checker.h>

#include "sdk/lib/async_patterns/testing/cpp/dispatcher_bound.h"
#include "sdk/lib/driver/testing/cpp/driver_runtime.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace gpio {

namespace {

class GpioDeviceWrapper {
 public:
  zx::result<fidl::ClientEnd<fuchsia_hardware_gpio::Gpio>> Connect(async_dispatcher_t* dispatcher) {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_gpio::Gpio>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }
    fidl::BindServer(dispatcher, std::move(endpoints->server), &device_);
    return zx::ok(std::move(endpoints->client));
  }

  explicit GpioDeviceWrapper()
      : device_(nullptr, ddk::GpioImplProtocolClient(gpio_impl_.GetProto()), 0, "GPIO_0") {}

  ddk::MockGpioImpl gpio_impl_;
  GpioDevice device_;
};

class MockGpioImplFidl : public fdf::WireServer<fuchsia_hardware_gpioimpl::GpioImpl>,
                         public ddk::MockGpioImpl {
 public:
  MockGpioImplFidl() : MockGpioImplFidl(0) {}
  explicit MockGpioImplFidl(uint32_t controller) : controller_(controller) {}

  // Helper method to make this easier to use from a DispatcherBound.
  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> CreateOutgoingAndServe() {
    fdf::Unowned dispatcher = fdf::Dispatcher::GetCurrent();

    outgoing_.emplace(fdf::OutgoingDirectory::Create(dispatcher->get()));

    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }

    auto service = outgoing_->AddService<fuchsia_hardware_gpioimpl::Service>(
        fuchsia_hardware_gpioimpl::Service::InstanceHandler({
            .device = bind_handler(dispatcher->get()),
        }));
    if (service.is_error()) {
      return service.take_error();
    }

    if (auto result = outgoing_->Serve(std::move(endpoints->server)); result.is_error()) {
      return result.take_error();
    }

    return zx::ok(std::move(endpoints->client));
  }

 private:
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_gpioimpl::GpioImpl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  void ConfigIn(fuchsia_hardware_gpioimpl::wire::GpioImplConfigInRequest* request,
                fdf::Arena& arena, ConfigInCompleter::Sync& completer) override {}
  void ConfigOut(fuchsia_hardware_gpioimpl::wire::GpioImplConfigOutRequest* request,
                 fdf::Arena& arena, ConfigOutCompleter::Sync& completer) override {}
  void SetAltFunction(fuchsia_hardware_gpioimpl::wire::GpioImplSetAltFunctionRequest* request,
                      fdf::Arena& arena, SetAltFunctionCompleter::Sync& completer) override {}
  void Read(fuchsia_hardware_gpioimpl::wire::GpioImplReadRequest* request, fdf::Arena& arena,
            ReadCompleter::Sync& completer) override {}

  void Write(fuchsia_hardware_gpioimpl::wire::GpioImplWriteRequest* request, fdf::Arena& arena,
             WriteCompleter::Sync& completer) override {
    zx_status_t status = GpioImplWrite(request->index, request->value);
    if (status == ZX_OK) {
      completer.buffer(arena).ReplySuccess();
    } else {
      completer.buffer(arena).ReplyError(status);
    }
  }

  void SetPolarity(fuchsia_hardware_gpioimpl::wire::GpioImplSetPolarityRequest* request,
                   fdf::Arena& arena, SetPolarityCompleter::Sync& completer) override {}
  void SetDriveStrength(fuchsia_hardware_gpioimpl::wire::GpioImplSetDriveStrengthRequest* request,
                        fdf::Arena& arena, SetDriveStrengthCompleter::Sync& completer) override {}
  void GetDriveStrength(fuchsia_hardware_gpioimpl::wire::GpioImplGetDriveStrengthRequest* request,
                        fdf::Arena& arena, GetDriveStrengthCompleter::Sync& completer) override {}
  void GetInterrupt(fuchsia_hardware_gpioimpl::wire::GpioImplGetInterruptRequest* request,
                    fdf::Arena& arena, GetInterruptCompleter::Sync& completer) override {}
  void ReleaseInterrupt(fuchsia_hardware_gpioimpl::wire::GpioImplReleaseInterruptRequest* request,
                        fdf::Arena& arena, ReleaseInterruptCompleter::Sync& completer) override {}
  void GetPins(fdf::Arena& arena, GetPinsCompleter::Sync& completer) override {}
  void GetInitSteps(fdf::Arena& arena, GetInitStepsCompleter::Sync& completer) override {}
  void GetControllerId(fdf::Arena& arena, GetControllerIdCompleter::Sync& completer) override {
    completer.buffer(arena).Reply(controller_);
  }

  const uint32_t controller_;
  std::optional<fdf::OutgoingDirectory> outgoing_;
};

class GpioTest : public zxtest::Test {
 public:
  GpioTest() : runtime_(mock_ddk::GetDriverRuntime()) {}

  void SetUp() override {
    zx::result gpio_client = gpio_.Connect(fdf::Dispatcher::GetCurrent()->async_dispatcher());
    ASSERT_TRUE(gpio_client.is_ok());
    gpio_client_.Bind(std::move(*gpio_client), fdf::Dispatcher::GetCurrent()->async_dispatcher());
  }

  void TearDown() override { gpio_.gpio_impl_.VerifyAndClear(); }

 protected:
  std::shared_ptr<fdf_testing::DriverRuntime> runtime_;
  GpioDeviceWrapper gpio_;
  fidl::WireClient<fuchsia_hardware_gpio::Gpio> gpio_client_;
};

TEST_F(GpioTest, TestFidlAll) {
  gpio_.gpio_impl_.ExpectRead(ZX_OK, 0, 20);
  gpio_client_->Read().ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::Read>& result) {
        EXPECT_OK(result.status());
        EXPECT_EQ(result->value()->value, 20);
        runtime_->Quit();
      });
  runtime_->Run();
  runtime_->ResetQuit();

  gpio_.gpio_impl_.ExpectWrite(ZX_OK, 0, 11);
  gpio_client_->Write(11).ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::Write>& result) {
        EXPECT_OK(result.status());
        runtime_->Quit();
      });
  runtime_->Run();
  runtime_->ResetQuit();

  gpio_.gpio_impl_.ExpectConfigIn(ZX_OK, 0, 0);
  gpio_client_->ConfigIn(fuchsia_hardware_gpio::wire::GpioFlags::kPullDown)
      .ThenExactlyOnce([&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::ConfigIn>& result) {
        EXPECT_OK(result.status());
        runtime_->Quit();
      });
  runtime_->Run();
  runtime_->ResetQuit();

  gpio_.gpio_impl_.ExpectConfigOut(ZX_OK, 0, 5);
  gpio_client_->ConfigOut(5).ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::ConfigOut>& result) {
        EXPECT_OK(result.status());
        runtime_->Quit();
      });
  runtime_->Run();
  runtime_->ResetQuit();

  gpio_.gpio_impl_.ExpectSetDriveStrength(ZX_OK, 0, 2000, 2000);
  gpio_client_->SetDriveStrength(2000).ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::SetDriveStrength>& result) {
        EXPECT_OK(result.status());
        EXPECT_EQ(result->value()->actual_ds_ua, 2000);
        runtime_->Quit();
      });
  runtime_->Run();
  runtime_->ResetQuit();

  gpio_.gpio_impl_.ExpectGetDriveStrength(ZX_OK, 0, 2000);
  gpio_client_->GetDriveStrength().ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_gpio::Gpio::GetDriveStrength>& result) {
        EXPECT_OK(result.status());
        EXPECT_EQ(result->value()->result_ua, 2000);
        runtime_->Quit();
      });
  runtime_->Run();
  runtime_->ResetQuit();
}

TEST_F(GpioTest, TestBanjoSetDriveStrength) {
  uint64_t actual = 0;
  gpio_.gpio_impl_.ExpectSetDriveStrength(ZX_OK, 0, 3000, 3000);
  EXPECT_OK(gpio_.device_.GpioSetDriveStrength(3000, &actual));
  EXPECT_EQ(actual, 3000);
}

TEST_F(GpioTest, TestBanjoGetDriveStrength) {
  uint64_t result = 0;
  gpio_.gpio_impl_.ExpectGetDriveStrength(ZX_OK, 0, 3000);
  EXPECT_OK(gpio_.device_.GpioGetDriveStrength(&result));
  EXPECT_EQ(result, 3000);
}

TEST(GpioTest, ValidateMetadataOk) {
  constexpr gpio_pin_t pins[] = {
      DECL_GPIO_PIN(0),
      DECL_GPIO_PIN(1),
      DECL_GPIO_PIN(2),
  };

  auto parent = MockDevice::FakeRootParent();

  ddk::MockGpioImpl gpio_impl;
  parent->AddProtocol(ZX_PROTOCOL_GPIO_IMPL, gpio_impl.GetProto()->ops, gpio_impl.GetProto()->ctx);
  parent->SetMetadata(DEVICE_METADATA_GPIO_PINS, pins, std::size(pins) * sizeof(gpio_pin_t));

  ASSERT_OK(GpioRootDevice::Create(nullptr, parent.get()));
}

TEST(GpioTest, ValidateMetadataRejectDuplicates) {
  constexpr gpio_pin_t pins[] = {
      DECL_GPIO_PIN(2),
      DECL_GPIO_PIN(1),
      DECL_GPIO_PIN(2),
      DECL_GPIO_PIN(0),
  };

  ddk::MockGpioImpl gpio_impl;
  auto parent = MockDevice::FakeRootParent();

  parent->AddProtocol(ZX_PROTOCOL_GPIO_IMPL, gpio_impl.GetProto()->ops, gpio_impl.GetProto()->ctx);
  parent->SetMetadata(DEVICE_METADATA_GPIO_PINS, pins, std::size(pins) * sizeof(gpio_pin_t));

  ASSERT_NOT_OK(GpioRootDevice::Create(nullptr, parent.get()));
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

TEST(GpioTest, Init) {
  namespace fhgpio = fuchsia_hardware_gpio::wire;
  namespace fhgpioimpl = fuchsia_hardware_gpioimpl::wire;

  constexpr gpio_pin_t kGpioPins[] = {
      DECL_GPIO_PIN(1),
      DECL_GPIO_PIN(2),
      DECL_GPIO_PIN(3),
  };

  std::shared_ptr<zx_device> fake_root = MockDevice::FakeRootParent();

  ddk::MockGpioImpl gpio;

  fake_root->AddProtocol(ZX_PROTOCOL_GPIO_IMPL, gpio.GetProto()->ops, gpio.GetProto()->ctx);

  fidl::Arena arena;

  fhgpioimpl::InitMetadata metadata;
  metadata.steps = fidl::VectorView<fhgpioimpl::InitStep>(arena, 18);

  metadata.steps[0].index = 1;
  metadata.steps[0].call = fhgpioimpl::InitCall::WithInputFlags(fhgpio::GpioFlags::kPullDown);

  metadata.steps[1].index = 1;
  metadata.steps[1].call = fhgpioimpl::InitCall::WithOutputValue(1);

  metadata.steps[2].index = 1;
  metadata.steps[2].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 4000);

  gpio.ExpectConfigIn(ZX_OK, 1, GPIO_PULL_DOWN)
      .ExpectConfigOut(ZX_OK, 1, 1)
      .ExpectSetDriveStrength(ZX_OK, 1, 4000, 4000);

  metadata.steps[3].index = 2;
  metadata.steps[3].call = fhgpioimpl::InitCall::WithInputFlags(fhgpio::GpioFlags::kNoPull);

  metadata.steps[4].index = 2;
  metadata.steps[4].call = fhgpioimpl::InitCall::WithAltFunction(arena, 5);

  metadata.steps[5].index = 2;
  metadata.steps[5].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 2000);

  gpio.ExpectConfigIn(ZX_OK, 2, GPIO_NO_PULL)
      .ExpectSetAltFunction(ZX_OK, 2, 5)
      .ExpectSetDriveStrength(ZX_OK, 2, 2000, 2000);

  metadata.steps[6].index = 3;
  metadata.steps[6].call = fhgpioimpl::InitCall::WithOutputValue(0);
  gpio.ExpectConfigOut(ZX_OK, 3, 0);

  metadata.steps[7].index = 3;
  metadata.steps[7].call = fhgpioimpl::InitCall::WithOutputValue(1);
  gpio.ExpectConfigOut(ZX_OK, 3, 1);

  metadata.steps[8].index = 3;
  metadata.steps[8].call = fhgpioimpl::InitCall::WithInputFlags(fhgpio::GpioFlags::kPullUp);
  gpio.ExpectConfigIn(ZX_OK, 3, GPIO_PULL_UP);

  metadata.steps[9].index = 2;
  metadata.steps[9].call = fhgpioimpl::InitCall::WithAltFunction(arena, 0);

  metadata.steps[10].index = 2;
  metadata.steps[10].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 1000);

  gpio.ExpectSetAltFunction(ZX_OK, 2, 0).ExpectSetDriveStrength(ZX_OK, 2, 1000, 1000);

  metadata.steps[11].index = 2;
  metadata.steps[11].call = fhgpioimpl::InitCall::WithOutputValue(1);
  gpio.ExpectConfigOut(ZX_OK, 2, 1);

  metadata.steps[12].index = 1;
  metadata.steps[12].call = fhgpioimpl::InitCall::WithInputFlags(fhgpio::GpioFlags::kPullUp);

  metadata.steps[13].index = 1;
  metadata.steps[13].call = fhgpioimpl::InitCall::WithAltFunction(arena, 0);

  metadata.steps[14].index = 1;
  metadata.steps[14].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 4000);

  gpio.ExpectConfigIn(ZX_OK, 1, GPIO_PULL_UP)
      .ExpectSetAltFunction(ZX_OK, 1, 0)
      .ExpectSetDriveStrength(ZX_OK, 1, 4000, 4000);

  metadata.steps[15].index = 1;
  metadata.steps[15].call = fhgpioimpl::InitCall::WithOutputValue(1);
  gpio.ExpectConfigOut(ZX_OK, 1, 1);

  metadata.steps[16].index = 3;
  metadata.steps[16].call = fhgpioimpl::InitCall::WithAltFunction(arena, 3);

  metadata.steps[17].index = 3;
  metadata.steps[17].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 2000);

  gpio.ExpectSetAltFunction(ZX_OK, 3, 3).ExpectSetDriveStrength(ZX_OK, 3, 2000, 2000);

  fit::result encoded = fidl::Persist(metadata);
  ASSERT_TRUE(encoded.is_ok(), "%s", encoded.error_value().FormatDescription().c_str());

  std::vector<uint8_t>& message = encoded.value();
  fake_root->SetMetadata(DEVICE_METADATA_GPIO_INIT, message.data(), message.size());
  fake_root->SetMetadata(DEVICE_METADATA_GPIO_PINS, kGpioPins, sizeof(kGpioPins));

  EXPECT_OK(GpioRootDevice::Create(nullptr, fake_root.get()));

  // GPIO init and root devices.
  EXPECT_EQ(fake_root->child_count(), 2);
  for (auto& child : fake_root->children()) {
    device_async_remove(child.get());
  }
  mock_ddk::ReleaseFlaggedDevices(fake_root.get());

  EXPECT_NO_FAILURES(gpio.VerifyAndClear());
}

TEST(GpioTest, InitErrorHandling) {
  namespace fhgpio = fuchsia_hardware_gpio::wire;
  namespace fhgpioimpl = fuchsia_hardware_gpioimpl::wire;

  constexpr gpio_pin_t kGpioPins[] = {
      DECL_GPIO_PIN(1),
      DECL_GPIO_PIN(2),
      DECL_GPIO_PIN(3),
  };

  std::shared_ptr<zx_device> fake_root = MockDevice::FakeRootParent();

  ddk::MockGpioImpl gpio;

  fake_root->AddProtocol(ZX_PROTOCOL_GPIO_IMPL, gpio.GetProto()->ops, gpio.GetProto()->ctx);

  fidl::Arena arena;

  fuchsia_hardware_gpioimpl::wire::InitMetadata metadata;
  metadata.steps = fidl::VectorView<fuchsia_hardware_gpioimpl::wire::InitStep>(arena, 9);

  metadata.steps[0].index = 4;
  metadata.steps[0].call = fhgpioimpl::InitCall::WithInputFlags(fhgpio::GpioFlags::kPullDown);

  metadata.steps[1].index = 4;
  metadata.steps[1].call = fhgpioimpl::InitCall::WithOutputValue(1);

  metadata.steps[2].index = 4;
  metadata.steps[2].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 4000);

  gpio.ExpectConfigIn(ZX_OK, 4, GPIO_PULL_DOWN)
      .ExpectConfigOut(ZX_OK, 4, 1)
      .ExpectSetDriveStrength(ZX_OK, 4, 4000, 4000);

  metadata.steps[3].index = 2;
  metadata.steps[3].call = fhgpioimpl::InitCall::WithInputFlags(fhgpio::GpioFlags::kNoPull);

  metadata.steps[4].index = 2;
  metadata.steps[4].call = fhgpioimpl::InitCall::WithAltFunction(arena, 5);

  metadata.steps[5].index = 2;
  metadata.steps[5].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 2000);

  gpio.ExpectConfigIn(ZX_OK, 2, GPIO_NO_PULL)
      .ExpectSetAltFunction(ZX_OK, 2, 5)
      .ExpectSetDriveStrength(ZX_OK, 2, 2000, 2000);

  metadata.steps[6].index = 3;
  metadata.steps[6].call = fhgpioimpl::InitCall::WithOutputValue(0);
  gpio.ExpectConfigOut(ZX_ERR_NOT_FOUND, 3, 0);

  // Processing should not continue after the above error.

  metadata.steps[7].index = 2;
  metadata.steps[7].call = fhgpioimpl::InitCall::WithAltFunction(arena, 0);

  metadata.steps[8].index = 2;
  metadata.steps[8].call = fhgpioimpl::InitCall::WithDriveStrengthUa(arena, 1000);

  fit::result encoded = fidl::Persist(metadata);
  ASSERT_TRUE(encoded.is_ok(), "%s", encoded.error_value().FormatDescription().c_str());

  std::vector<uint8_t>& message = encoded.value();
  fake_root->SetMetadata(DEVICE_METADATA_GPIO_INIT, message.data(), message.size());
  fake_root->SetMetadata(DEVICE_METADATA_GPIO_PINS, kGpioPins, sizeof(kGpioPins));

  EXPECT_OK(GpioRootDevice::Create(nullptr, fake_root.get()));

  // GPIO root device (init device should not be added due to errors).
  EXPECT_EQ(fake_root->child_count(), 1);
  device_async_remove(fake_root->GetLatestChild());
  mock_ddk::ReleaseFlaggedDevices(fake_root.get());

  EXPECT_NO_FAILURES(gpio.VerifyAndClear());
}

TEST(GpioTest, BanjoPreferredOverFidl) {
  constexpr gpio_pin_t pins[] = {
      DECL_GPIO_PIN(100),
  };

  auto parent = MockDevice::FakeRootParent();
  std::shared_ptr<fdf_testing::DriverRuntime> runtime = mock_ddk::GetDriverRuntime();
  fdf::UnownedSynchronizedDispatcher background_dispatcher = runtime->StartBackgroundDispatcher();

  ddk::MockGpioImpl gpioimpl_banjo;
  parent->AddProtocol(ZX_PROTOCOL_GPIO_IMPL, gpioimpl_banjo.GetProto()->ops,
                      gpioimpl_banjo.GetProto()->ctx);

  async_patterns::TestDispatcherBound<MockGpioImplFidl> gpioimpl_fidl(
      background_dispatcher->async_dispatcher(), std::in_place, 0);

  {
    auto outgoing_client = gpioimpl_fidl.SyncCall(&MockGpioImplFidl::CreateOutgoingAndServe);
    ASSERT_TRUE(outgoing_client.is_ok());

    parent->AddFidlService(fuchsia_hardware_gpioimpl::Service::Name, std::move(*outgoing_client));
  }

  parent->SetMetadata(DEVICE_METADATA_GPIO_PINS, pins, std::size(pins) * sizeof(gpio_pin_t));

  EXPECT_OK(GpioRootDevice::Create(nullptr, parent.get()));

  ASSERT_EQ(parent->child_count(), 1);
  auto* const root_device = parent->GetLatestChild();

  ASSERT_EQ(root_device->child_count(), 1);

  // The dut was provided with Banjo and FIDL clients, but only the Banjo client should receive this
  // call.
  gpioimpl_banjo.ExpectWrite(ZX_OK, 100, 1);

  const ddk::GpioProtocolClient dut(root_device->GetLatestChild());
  EXPECT_OK(dut.Write(1));

  EXPECT_NO_FAILURES(gpioimpl_banjo.VerifyAndClear());
  EXPECT_NO_FAILURES(gpioimpl_fidl.SyncCall(&MockGpioImplFidl::VerifyAndClear));
}

TEST(GpioTest, FallBackToFidl) {
  constexpr gpio_pin_t pins[] = {
      DECL_GPIO_PIN(100),
  };

  auto parent = MockDevice::FakeRootParent();
  std::shared_ptr<fdf_testing::DriverRuntime> runtime = mock_ddk::GetDriverRuntime();
  fdf::UnownedSynchronizedDispatcher background_dispatcher = runtime->StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<MockGpioImplFidl> gpioimpl_fidl(
      background_dispatcher->async_dispatcher(), std::in_place, 0);

  {
    auto outgoing_client = gpioimpl_fidl.SyncCall(&MockGpioImplFidl::CreateOutgoingAndServe);
    ASSERT_TRUE(outgoing_client.is_ok());

    parent->AddFidlService(fuchsia_hardware_gpioimpl::Service::Name, std::move(*outgoing_client));
  }

  parent->SetMetadata(DEVICE_METADATA_GPIO_PINS, pins, std::size(pins) * sizeof(gpio_pin_t));

  EXPECT_OK(GpioRootDevice::Create(nullptr, parent.get()));

  ASSERT_EQ(parent->child_count(), 1);
  auto* const root_device = parent->GetLatestChild();

  ASSERT_EQ(root_device->child_count(), 1);

  const auto path =
      std::string("svc/") +
      component::MakeServiceMemberPath<fuchsia_hardware_gpio::Service::Device>("default");

  auto client_end = component::ConnectAt<fuchsia_hardware_gpio::Gpio>(
      root_device->GetLatestChild()->outgoing(), path);
  ASSERT_TRUE(client_end.is_ok());

  fidl::WireClient<fuchsia_hardware_gpio::Gpio> gpio_client(
      *std::move(client_end), fdf::Dispatcher::GetCurrent()->async_dispatcher());

  gpioimpl_fidl.SyncCall(
      [](MockGpioImplFidl* gpio) { gpio->ExpectWrite(ZX_OK, 100, static_cast<uint8_t>(1)); });

  gpio_client->Write(1).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime->Quit();
  });

  runtime->Run();

  EXPECT_NO_FAILURES(gpioimpl_fidl.SyncCall(&MockGpioImplFidl::VerifyAndClear));
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

  async_patterns::TestDispatcherBound<MockGpioImplFidl> gpioimpl_fidl(
      background_dispatcher->async_dispatcher(), std::in_place, kController);

  {
    auto outgoing_client = gpioimpl_fidl.SyncCall(&MockGpioImplFidl::CreateOutgoingAndServe);
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

    gpioimpl_fidl.SyncCall([pin = properties[0].value](MockGpioImplFidl* gpio) {
      gpio->ExpectWrite(ZX_OK, pin, uint8_t{0});
    });

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
}

}  // namespace

}  // namespace gpio
