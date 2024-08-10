// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "buttons.h"

#include <fidl/fuchsia.hardware.gpio/cpp/wire_test_base.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/driver/testing/cpp/test_environment.h>

#include <set>

#include <gtest/gtest.h>

#include "src/devices/gpio/testing/fake-gpio/fake-gpio.h"
#include "src/lib/testing/predicates/status.h"

namespace {
static const buttons_button_config_t buttons_direct[] = {
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_VOLUME_UP, 0, 0, 0},
};

static const buttons_gpio_config_t gpios_direct[] = {
    {BUTTONS_GPIO_TYPE_INTERRUPT,
     0,
     {.interrupt = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kNoPull)}}}};

static const buttons_gpio_config_t gpios_wakeable[] = {
    {BUTTONS_GPIO_TYPE_INTERRUPT,
     BUTTONS_GPIO_FLAG_WAKE_VECTOR,
     {.interrupt = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kNoPull)}}},
};

static const buttons_button_config_t buttons_multiple[] = {
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_VOLUME_UP, 0, 0, 0},
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_MIC_MUTE, 1, 0, 0},
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_CAM_MUTE, 2, 0, 0},
};

static const buttons_gpio_config_t gpios_multiple[] = {
    {BUTTONS_GPIO_TYPE_INTERRUPT,
     0,
     {.interrupt = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kNoPull)}}},
    {BUTTONS_GPIO_TYPE_INTERRUPT,
     0,
     {.interrupt = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kNoPull)}}},
    {BUTTONS_GPIO_TYPE_INTERRUPT,
     0,
     {.interrupt = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kNoPull)}}},
};

static const buttons_gpio_config_t gpios_multiple_one_polled[] = {
    {BUTTONS_GPIO_TYPE_INTERRUPT,
     0,
     {.interrupt = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kNoPull)}}},
    {BUTTONS_GPIO_TYPE_POLL,
     0,
     {.poll = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kNoPull),
               zx::msec(20).get()}}},
    {BUTTONS_GPIO_TYPE_INTERRUPT,
     0,
     {.interrupt = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kNoPull)}}},
};

static const buttons_button_config_t buttons_matrix[] = {
    {BUTTONS_TYPE_MATRIX, BUTTONS_ID_VOLUME_UP, 0, 2, 0},
    {BUTTONS_TYPE_MATRIX, BUTTONS_ID_KEY_A, 1, 2, 0},
    {BUTTONS_TYPE_MATRIX, BUTTONS_ID_KEY_M, 0, 3, 0},
    {BUTTONS_TYPE_MATRIX, BUTTONS_ID_PLAY_PAUSE, 1, 3, 0},
};

static const buttons_gpio_config_t gpios_matrix[] = {
    {BUTTONS_GPIO_TYPE_INTERRUPT,
     0,
     {.interrupt = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kPullUp)}}},
    {BUTTONS_GPIO_TYPE_INTERRUPT,
     0,
     {.interrupt = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kPullUp)}}},
    {BUTTONS_GPIO_TYPE_MATRIX_OUTPUT, 0, {.matrix = {0}}},
    {BUTTONS_GPIO_TYPE_MATRIX_OUTPUT, 0, {.matrix = {0}}},
};

static const buttons_button_config_t buttons_duplicate[] = {
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_VOLUME_UP, 0, 0, 0},
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_VOLUME_DOWN, 1, 0, 0},
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_FDR, 2, 0, 0},
};

static const buttons_gpio_config_t gpios_duplicate[] = {
    {BUTTONS_GPIO_TYPE_INTERRUPT,
     0,
     {.interrupt = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kNoPull)}}},
    {BUTTONS_GPIO_TYPE_INTERRUPT,
     0,
     {.interrupt = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kNoPull)}}},
    {BUTTONS_GPIO_TYPE_INTERRUPT,
     0,
     {.interrupt = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kNoPull)}}},
};
}  // namespace

namespace buttons {

static constexpr size_t kMaxGpioServers = 4;

enum MetadataVersion : uint8_t {
  kMetadataSingleButtonDirect = 0,
  kMetadataWakeable,
  kMetadataMultiple,
  kMetadataDuplicate,
  kMetadataMatrix,
  kMetadataPolled,
};

class LocalFakeGpio : public fake_gpio::FakeGpio {
 public:
  LocalFakeGpio() = default;
  void SetExpectedInterruptFlags(uint32_t flags) { expected_interrupt_flags_ = flags; }

 private:
  void GetInterrupt(GetInterruptRequestView request,
                    GetInterruptCompleter::Sync& completer) override {
    EXPECT_EQ(request->flags, expected_interrupt_flags_);
    FakeGpio::GetInterrupt(request, completer);
  }
  uint32_t expected_interrupt_flags_ = ZX_INTERRUPT_MODE_EDGE_HIGH;
};

class ButtonsTestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    device_server_.Init(component::kDefaultInstance, "");
    device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &to_driver_vfs);

    // Serve fake GPIO servers.
    size_t n_gpios = gpios_.size_bytes() / sizeof(buttons_gpio_config_t);
    EXPECT_LE(n_gpios, kMaxGpioServers);
    for (size_t i = 0; i < n_gpios; ++i) {
      EXPECT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL,
                                      &fake_gpio_interrupts_[i]));
      zx::interrupt interrupt;
      EXPECT_OK(fake_gpio_interrupts_[i].duplicate(ZX_RIGHT_SAME_RIGHTS, &interrupt));
      fake_gpio_servers_[i].SetInterrupt(zx::ok(std::move(interrupt)));
      auto gpio_handler = fake_gpio_servers_[i].CreateInstanceHandler();
      auto result = to_driver_vfs.AddService<fuchsia_hardware_gpio::Service>(
          std::move(gpio_handler), buttons_names_[i].c_str());
      if (result.is_error()) {
        return result;
      }

      fake_gpio_servers_[i].SetDefaultReadResponse(zx::ok(uint8_t{0u}));
    }

    return zx::ok();
  }

  void Init(MetadataVersion metadata_version) {
    // Serve metadata.
    cpp20::span<const buttons_button_config_t> buttons;
    switch (metadata_version) {
      case kMetadataSingleButtonDirect: {
        buttons_names_ = {"volume-up"};
        buttons = {buttons_direct, std::size(buttons_direct)};
        gpios_ = {gpios_direct, std::size(gpios_direct)};
      } break;
      case kMetadataWakeable: {
        buttons_names_ = {"volume-up"};
        buttons = {buttons_direct, std::size(buttons_direct)};
        gpios_ = {gpios_wakeable, std::size(gpios_wakeable)};
      } break;
      case kMetadataMultiple: {
        buttons_names_ = {"volume-up", "mic-privacy", "cam-mute"};
        buttons = {buttons_multiple, std::size(buttons_multiple)};
        gpios_ = {gpios_multiple, std::size(gpios_multiple)};
      } break;
      case kMetadataDuplicate: {
        buttons_names_ = {"volume-up", "volume-down", "volume-both"};
        buttons = {buttons_duplicate, std::size(buttons_duplicate)};
        gpios_ = {gpios_duplicate, std::size(gpios_duplicate)};
      } break;
      case kMetadataMatrix: {
        buttons_names_ = {"volume-up", "key-a", "key-m", "play-pause"};
        buttons = {buttons_matrix, std::size(buttons_matrix)};
        gpios_ = {gpios_matrix, std::size(gpios_matrix)};
      } break;
      case kMetadataPolled: {
        buttons_names_ = {"volume-up", "mic-privacy", "cam-mute"};
        buttons = {buttons_multiple, std::size(buttons_multiple)};
        gpios_ = {gpios_multiple_one_polled, std::size(gpios_multiple_one_polled)};
      } break;
      default:
        ASSERT_TRUE(0);
    }
    zx_status_t status;
    status =
        device_server_.AddMetadata(DEVICE_METADATA_BUTTONS_GPIOS, &gpios_[0], gpios_.size_bytes());
    ASSERT_OK(status);
    status = device_server_.AddMetadata(DEVICE_METADATA_BUTTONS_BUTTONS, &buttons[0],
                                        buttons.size_bytes());
    ASSERT_OK(status);
  }

  void SetGpioReadResponse(size_t gpio_index, uint8_t read_data) {
    fake_gpio_servers_[gpio_index].PushReadResponse(zx::ok(read_data));
  }

  void SetDefaultGpioReadResponse(size_t gpio_index, uint8_t read_data) {
    fake_gpio_servers_[gpio_index].SetDefaultReadResponse(zx::ok(read_data));
  }

  void SetExpectedInterruptFlags(uint32_t flags) {
    fake_gpio_servers_[0].SetExpectedInterruptFlags(flags);
  }

  zx::interrupt fake_gpio_interrupts_[kMaxGpioServers];
  LocalFakeGpio fake_gpio_servers_[kMaxGpioServers];

 private:
  compat::DeviceServer device_server_;
  std::vector<std::string> buttons_names_;
  cpp20::span<const buttons_gpio_config_t> gpios_;
};

class ButtonsTestConfig final {
 public:
  using DriverType = buttons::Buttons;
  using EnvironmentType = ButtonsTestEnvironment;
};

class ButtonsTest : public ::testing::Test {
 public:
  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  zx::result<> Init(MetadataVersion metadata_version) {
    driver_test().RunInEnvironmentTypeContext(
        [metadata_version](ButtonsTestEnvironment& env) { env.Init(metadata_version); });

    return driver_test().StartDriver();
  }

  fidl::ClientEnd<fuchsia_input_report::InputDevice> GetClient() {
    // Connect to InputDevice.
    zx::result connect_result =
        driver_test().ConnectThroughDevfs<fuchsia_input_report::InputDevice>("buttons");
    EXPECT_EQ(ZX_OK, connect_result.status_value());
    return std::move(connect_result.value());
  }

  fdf_testing::BackgroundDriverTest<ButtonsTestConfig>& driver_test() { return driver_test_; }

  void DrainInitialReport(fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>& reader) {
    auto result = reader->ReadInputReports();
    ASSERT_OK(result.status());
    ASSERT_FALSE(result.value().is_error());
    auto& reports = result.value().value()->reports;

    ASSERT_EQ(1U, reports.count());
    auto report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
  }

  fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> GetReader() {
    auto endpoints = fidl::Endpoints<fuchsia_input_report::InputReportsReader>::Create();
    fidl::ClientEnd<fuchsia_input_report::InputDevice> client = GetClient();
    auto result = fidl::WireCall(client)->GetInputReportsReader(std::move(endpoints.server));
    ZX_ASSERT(result.ok());
    ZX_ASSERT(fidl::WireCall(client)->GetDescriptor().ok());
    auto reader =
        fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(std::move(endpoints.client));
    ZX_ASSERT(reader.is_valid());

    DrainInitialReport(reader);

    return reader;
  }

 private:
  fdf_testing::BackgroundDriverTest<ButtonsTestConfig> driver_test_;
};

TEST_F(ButtonsTest, DirectButtonInit) {
  auto result = Init(kMetadataSingleButtonDirect);
  ASSERT_TRUE(result.is_ok());
}

TEST_F(ButtonsTest, DirectButtonPush) {
  auto result = Init(kMetadataSingleButtonDirect);
  ASSERT_TRUE(result.is_ok());

  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  });
}

TEST_F(ButtonsTest, DirectButtonPushReleaseReport) {
  auto result = Init(kMetadataSingleButtonDirect);
  ASSERT_TRUE(result.is_ok());

  auto reader = GetReader();

  // Push.
  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.SetDefaultGpioReadResponse(0, 1);

    env.fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  });

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1U);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 1U);
    EXPECT_EQ(consumer_control.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);
  }

  // Release.
  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.SetDefaultGpioReadResponse(0, 0);
    env.fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  });

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1U);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 0U);
  }
}

TEST_F(ButtonsTest, DirectButtonPushReleasePush) {
  auto result = Init(kMetadataSingleButtonDirect);
  ASSERT_TRUE(result.is_ok());

  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.SetGpioReadResponse(0, 0);
    env.fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());

    env.SetGpioReadResponse(0, 1);
    env.fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());

    env.SetGpioReadResponse(0, 0);
    env.fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  });
}

TEST_F(ButtonsTest, DirectButtonFlaky) {
  auto init_result = Init(kMetadataSingleButtonDirect);
  ASSERT_TRUE(init_result.is_ok());

  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.SetGpioReadResponse(0, 1);
    env.SetGpioReadResponse(0, 0);
    env.SetDefaultGpioReadResponse(0, 1);  // Stabilizes.

    env.fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  });

  auto reader = GetReader();
  auto result = reader->ReadInputReports();
  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_ok());
  auto& reports = result->value()->reports;

  ASSERT_EQ(reports.count(), 1U);
  auto& report = reports[0];

  ASSERT_TRUE(report.has_event_time());
  ASSERT_TRUE(report.has_consumer_control());
  auto& consumer_control = report.consumer_control();

  ASSERT_TRUE(consumer_control.has_pressed_buttons());
  ASSERT_EQ(consumer_control.pressed_buttons().count(), 1U);
  EXPECT_EQ(consumer_control.pressed_buttons()[0],
            fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);
}

TEST_F(ButtonsTest, MatrixButtonInit) {
  auto result = Init(kMetadataMatrix);
  ASSERT_TRUE(result.is_ok());
}

TEST_F(ButtonsTest, MatrixButtonPush) {
  auto init_result = Init(kMetadataMatrix);
  ASSERT_TRUE(init_result.is_ok());

  auto reader = GetReader();

  // Initial reads.
  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.SetGpioReadResponse(0, 0);
    env.SetGpioReadResponse(0, 0);
    env.SetGpioReadResponse(1, 0);
    env.SetGpioReadResponse(1, 0);

    env.SetGpioReadResponse(0, 1);  // Read row. Matrix Scan for 0.
    env.SetGpioReadResponse(0, 0);  // Read row. Matrix Scan for 2.
    env.SetGpioReadResponse(1, 0);  // Read row. Matrix Scan for 1.
    env.SetGpioReadResponse(1, 0);  // Read row. Matrix Scan for 3.

    env.fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  });

  auto result = reader->ReadInputReports();
  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_ok());
  auto& reports = result->value()->reports;

  ASSERT_EQ(reports.count(), 1U);
  auto& report = reports[0];

  ASSERT_TRUE(report.has_event_time());
  ASSERT_TRUE(report.has_consumer_control());
  auto& consumer_control = report.consumer_control();

  ASSERT_TRUE(consumer_control.has_pressed_buttons());
  ASSERT_EQ(consumer_control.pressed_buttons().count(), 1U);
  EXPECT_EQ(consumer_control.pressed_buttons()[0],
            fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);

  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    auto gpio_2_states = env.fake_gpio_servers_[2].GetStateLog();
    ASSERT_GE(gpio_2_states.size(), 4U);
    ASSERT_EQ(fake_gpio::ReadSubState{.flags = fuchsia_hardware_gpio::GpioFlags::kNoPull},
              (gpio_2_states.end() - 4)->sub_state);  // Float column.
    ASSERT_EQ(fake_gpio::WriteSubState{.value = gpios_matrix[2].matrix.output_value},
              (gpio_2_states.end() - 3)->sub_state);  // Restore column.
    ASSERT_EQ(fake_gpio::ReadSubState{.flags = fuchsia_hardware_gpio::GpioFlags::kNoPull},
              (gpio_2_states.end() - 2)->sub_state);  // Float column.
    ASSERT_EQ(fake_gpio::WriteSubState{.value = gpios_matrix[2].matrix.output_value},
              (gpio_2_states.end() - 1)->sub_state);  // Restore column.

    auto gpio_3_states = env.fake_gpio_servers_[3].GetStateLog();
    ASSERT_GE(gpio_3_states.size(), 4U);
    ASSERT_EQ(fake_gpio::ReadSubState{.flags = fuchsia_hardware_gpio::GpioFlags::kNoPull},
              (gpio_3_states.end() - 4)->sub_state);  // Float column.
    ASSERT_EQ(fake_gpio::WriteSubState{.value = gpios_matrix[3].matrix.output_value},
              (gpio_3_states.end() - 3)->sub_state);  // Restore column.
    ASSERT_EQ(fake_gpio::ReadSubState{.flags = fuchsia_hardware_gpio::GpioFlags::kNoPull},
              (gpio_3_states.end() - 2)->sub_state);  // Float column.
    ASSERT_EQ(fake_gpio::WriteSubState{.value = gpios_matrix[3].matrix.output_value},
              (gpio_3_states.end() - 1)->sub_state);  // Restore column.
  });
}

TEST_F(ButtonsTest, DuplicateReports) {
  auto result = Init(kMetadataDuplicate);
  ASSERT_TRUE(result.is_ok());

  auto reader = GetReader();

  // Holding FDR (VOL_UP and VOL_DOWN), then release VOL_UP, should only get one report
  // for the FDR and one report for the VOL_UP. When FDR is released, there is no
  // new report generated since the reported values do not change.

  // Push FDR (both VOL_UP and VOL_DOWN).
  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.SetGpioReadResponse(2, 1);
    env.SetGpioReadResponse(2, 1);

    // Report.
    env.SetGpioReadResponse(0, 1);
    env.SetGpioReadResponse(1, 1);
    env.SetGpioReadResponse(2, 1);

    // Release VOL_UP.
    env.SetGpioReadResponse(0, 0);
    env.SetGpioReadResponse(0, 0);

    // Report.
    env.SetGpioReadResponse(0, 0);
    env.SetGpioReadResponse(1, 1);
    env.SetGpioReadResponse(2, 0);

    // Release FDR (both VOL_UP and VOL_DOWN).
    env.SetGpioReadResponse(2, 0);
    env.SetGpioReadResponse(2, 0);

    // Report (same as before).
    env.SetGpioReadResponse(0, 0);
    env.SetGpioReadResponse(1, 1);
    env.SetGpioReadResponse(2, 0);

    env.fake_gpio_interrupts_[2].trigger(0, zx::clock::get_monotonic());
  });

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1U);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 3U);
    std::set<fuchsia_input_report::wire::ConsumerControlButton> pressed_buttons;
    for (const auto& button : consumer_control.pressed_buttons()) {
      pressed_buttons.insert(button);
    }
    const std::set<fuchsia_input_report::wire::ConsumerControlButton> expected_buttons = {
        fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp,
        fuchsia_input_report::wire::ConsumerControlButton::kVolumeDown,
        fuchsia_input_report::wire::ConsumerControlButton::kFactoryReset};
    EXPECT_EQ(expected_buttons, pressed_buttons);
  }

  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  });

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1U);
    auto& report = reports[0];
    report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 1U);
    EXPECT_EQ(consumer_control.pressed_buttons()[0],
              fuchsia_input_report::ConsumerControlButton::kVolumeDown);
  }

  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.fake_gpio_interrupts_[2].trigger(0, zx::clock::get_monotonic());
  });
}

TEST_F(ButtonsTest, CamMute) {
  auto result = Init(kMetadataMultiple);
  ASSERT_TRUE(result.is_ok());

  auto reader = GetReader();

  // Push camera mute.
  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.SetGpioReadResponse(2, 1);
    env.SetGpioReadResponse(2, 1);

    // Report.
    env.SetGpioReadResponse(0, 0);
    env.SetGpioReadResponse(1, 0);
    env.SetGpioReadResponse(2, 1);

    env.fake_gpio_interrupts_[2].trigger(0, zx::clock::get_monotonic());
  });

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1U);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 1U);
    EXPECT_EQ(consumer_control.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kCameraDisable);
  }

  // Release camera mute.
  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.SetGpioReadResponse(2, 0);
    env.SetGpioReadResponse(2, 0);

    // Report.
    env.SetGpioReadResponse(0, 0);
    env.SetGpioReadResponse(1, 0);
    env.SetGpioReadResponse(2, 0);

    env.fake_gpio_interrupts_[2].trigger(0, zx::clock::get_monotonic());
  });

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1U);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 0U);
  }
}

TEST_F(ButtonsTest, DirectButtonWakeable) {
  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.SetExpectedInterruptFlags(ZX_INTERRUPT_MODE_EDGE_HIGH | ZX_INTERRUPT_WAKE_VECTOR);
  });

  auto result = Init(kMetadataWakeable);
  ASSERT_TRUE(result.is_ok());

  auto reader = GetReader();

  // Push.
  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.SetDefaultGpioReadResponse(0, 1);

    env.fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  });

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1U);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 1U);
    EXPECT_EQ(consumer_control.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);
  }

  // Release.
  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.SetDefaultGpioReadResponse(0, 0);
    env.fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  });

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1U);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 0U);
  }
}

TEST_F(ButtonsTest, PollOneButton) {
  auto result = Init(kMetadataPolled);
  ASSERT_TRUE(result.is_ok());

  auto reader = GetReader();

  // All gpios_ must have a default read value if polling is being used, as they are all ready
  // every poll period.
  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.SetDefaultGpioReadResponse(0, 1);

    env.fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  });
  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1U);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 1U);
    EXPECT_EQ(consumer_control.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);
  }

  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.SetDefaultGpioReadResponse(1, 1);

    env.fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
    env.fake_gpio_interrupts_[1].trigger(0, zx::clock::get_monotonic());
  });

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1U);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 2U);
    std::set<fuchsia_input_report::wire::ConsumerControlButton> pressed_buttons;
    for (const auto& button : consumer_control.pressed_buttons()) {
      pressed_buttons.insert(button);
    }
    const std::set<fuchsia_input_report::wire::ConsumerControlButton> expected_buttons = {
        fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp,
        fuchsia_input_report::wire::ConsumerControlButton::kMicMute};
    EXPECT_EQ(expected_buttons, pressed_buttons);
  }

  driver_test().RunInEnvironmentTypeContext([](ButtonsTestEnvironment& env) {
    env.SetDefaultGpioReadResponse(0, 0);
    env.fake_gpio_interrupts_[1].trigger(0, zx::clock::get_monotonic());
  });

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1U);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 1U);
    EXPECT_EQ(consumer_control.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kMicMute);
  }

  driver_test().RunInEnvironmentTypeContext(
      [](ButtonsTestEnvironment& env) { env.SetDefaultGpioReadResponse(1, 0); });

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1U);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 0U);
  }
}

}  // namespace buttons
