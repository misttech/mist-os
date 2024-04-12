// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "buttons.h"

#include <fidl/fuchsia.hardware.gpio/cpp/wire_test_base.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <set>

#include <zxtest/zxtest.h>

#include "src/devices/gpio/testing/fake-gpio/fake-gpio.h"

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
struct IncomingNamespace {
  fdf_testing::TestNode node_{std::string("root")};
  fdf_testing::TestEnvironment env_{fdf::Dispatcher::GetCurrent()->get()};
  compat::DeviceServer device_server_;
  LocalFakeGpio fake_gpio_servers_[kMaxGpioServers];
};

class ButtonsTest : public zxtest::Test {
 public:
  void Init(MetadataVersion metadata_version) {
    fuchsia_driver_framework::DriverStartArgs start_args;
    incoming_.SyncCall([&](IncomingNamespace* incoming) {
      auto start_args_result = incoming->node_.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());
      start_args = std::move(start_args_result->start_args);

      auto init_result =
          incoming->env_.Initialize(std::move(start_args_result->incoming_directory_server));
      ASSERT_TRUE(init_result.is_ok());
      incoming->device_server_.Init(component::kDefaultInstance, "");

      // Serve metadata.
      std::vector<std::string> buttons_names;
      cpp20::span<const buttons_button_config_t> buttons;
      cpp20::span<const buttons_gpio_config_t> gpios;
      switch (metadata_version) {
        case kMetadataSingleButtonDirect: {
          buttons_names = {"volume-up"};
          buttons = {buttons_direct, std::size(buttons_direct)};
          gpios = {gpios_direct, std::size(gpios_direct)};
        } break;
        case kMetadataWakeable: {
          buttons_names = {"volume-up"};
          buttons = {buttons_direct, std::size(buttons_direct)};
          gpios = {gpios_wakeable, std::size(gpios_wakeable)};
        } break;
        case kMetadataMultiple: {
          buttons_names = {"volume-up", "mic-privacy", "cam-mute"};
          buttons = {buttons_multiple, std::size(buttons_multiple)};
          gpios = {gpios_multiple, std::size(gpios_multiple)};
        } break;
        case kMetadataDuplicate: {
          buttons_names = {"volume-up", "volume-down", "volume-both"};
          buttons = {buttons_duplicate, std::size(buttons_duplicate)};
          gpios = {gpios_duplicate, std::size(gpios_duplicate)};
        } break;
        case kMetadataMatrix: {
          buttons_names = {"volume-up", "key-a", "key-m", "play-pause"};
          buttons = {buttons_matrix, std::size(buttons_matrix)};
          gpios = {gpios_matrix, std::size(gpios_matrix)};
        } break;
        case kMetadataPolled: {
          buttons_names = {"volume-up", "mic-privacy", "cam-mute"};
          buttons = {buttons_multiple, std::size(buttons_multiple)};
          gpios = {gpios_multiple_one_polled, std::size(gpios_multiple_one_polled)};
        } break;
        default:
          ASSERT_TRUE(0);
      }
      zx_status_t status;
      status = incoming->device_server_.AddMetadata(DEVICE_METADATA_BUTTONS_GPIOS, &gpios[0],
                                                    gpios.size_bytes());
      ASSERT_OK(status);
      status = incoming->device_server_.AddMetadata(DEVICE_METADATA_BUTTONS_BUTTONS, &buttons[0],
                                                    buttons.size_bytes());
      ASSERT_OK(status);
      status = incoming->device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                              &incoming->env_.incoming_directory());
      ASSERT_OK(status);

      // Serve fake GPIO servers.
      size_t n_gpios = gpios.size_bytes() / sizeof(buttons_gpio_config_t);
      ASSERT_LE(n_gpios, kMaxGpioServers);
      for (size_t i = 0; i < n_gpios; ++i) {
        ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL,
                                        &fake_gpio_interrupts_[i]));
        zx::interrupt interrupt;
        ASSERT_OK(fake_gpio_interrupts_[i].duplicate(ZX_RIGHT_SAME_RIGHTS, &interrupt));
        incoming->fake_gpio_servers_[i].SetInterrupt(zx::ok(std::move(interrupt)));
        auto gpio_handler = incoming->fake_gpio_servers_[i].CreateInstanceHandler();
        auto result =
            incoming->env_.incoming_directory().AddService<fuchsia_hardware_gpio::Service>(
                std::move(gpio_handler), buttons_names[i].c_str());
        ASSERT_TRUE(result.is_ok());

        incoming->fake_gpio_servers_[i].SetDefaultReadResponse(zx::ok(uint8_t{0u}));
      }
    });

    // Start dut_.
    auto result = runtime_.RunToCompletion(dut_.SyncCall(
        &fdf_testing::DriverUnderTest<buttons::Buttons>::Start, std::move(start_args)));
    ASSERT_TRUE(result.is_ok());

    // Connect to InputDevice.
    zx::result connect_result = incoming_.SyncCall([](IncomingNamespace* incoming) {
      return incoming->node_.children().at("buttons").ConnectToDevice();
    });
    ASSERT_OK(connect_result.status_value());
    client_.Bind(
        fidl::ClientEnd<fuchsia_input_report::InputDevice>(std::move(connect_result.value())));
  }

  void TearDown() override {
    // Stop dut_.
    auto result = runtime_.RunToCompletion(
        dut_.SyncCall(&fdf_testing::DriverUnderTest<buttons::Buttons>::PrepareStop));
    ASSERT_TRUE(result.is_ok());
  }

  void DrainInitialReport(fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>& reader) {
    auto result = reader->ReadInputReports();
    ASSERT_OK(result.status());
    ASSERT_FALSE(result.value().is_error());
    auto& reports = result.value().value()->reports;

    ASSERT_EQ(1, reports.count());
    auto report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
  }

  fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> GetReader() {
    auto endpoints = fidl::Endpoints<fuchsia_input_report::InputReportsReader>::Create();
    auto result = client_->GetInputReportsReader(std::move(endpoints.server));
    ZX_ASSERT(result.ok());
    ZX_ASSERT(client_->GetDescriptor().ok());
    auto reader =
        fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(std::move(endpoints.client));
    ZX_ASSERT(reader.is_valid());

    DrainInitialReport(reader);

    return reader;
  }

  void SetGpioReadResponse(size_t gpio_index, uint8_t read_data) {
    incoming_.SyncCall([&](IncomingNamespace* incoming) {
      incoming->fake_gpio_servers_[gpio_index].PushReadResponse(zx::ok(read_data));
    });
  }

  void SetDefaultGpioReadResponse(size_t gpio_index, uint8_t read_data) {
    incoming_.SyncCall([&](IncomingNamespace* incoming) {
      incoming->fake_gpio_servers_[gpio_index].SetDefaultReadResponse(zx::ok(read_data));
    });
  }

  void SetExpectedInterruptFlags(uint32_t flags) {
    incoming_.SyncCall([&](IncomingNamespace* incoming) {
      incoming->fake_gpio_servers_[0].SetExpectedInterruptFlags(flags);
    });
  }

 private:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_ = runtime_.StartBackgroundDispatcher();

 protected:
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{
      env_dispatcher_->async_dispatcher(), std::in_place};
  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<buttons::Buttons>> dut_{
      driver_dispatcher_->async_dispatcher(), std::in_place};

  fidl::WireSyncClient<fuchsia_input_report::InputDevice> client_;
  zx::interrupt fake_gpio_interrupts_[kMaxGpioServers];
};

TEST_F(ButtonsTest, DirectButtonInit) { Init(kMetadataSingleButtonDirect); }

TEST_F(ButtonsTest, DirectButtonPush) {
  Init(kMetadataSingleButtonDirect);

  fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
}

TEST_F(ButtonsTest, DirectButtonPushReleaseReport) {
  Init(kMetadataSingleButtonDirect);
  auto reader = GetReader();

  // Push.
  SetDefaultGpioReadResponse(0, 1);

  fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 1);
    EXPECT_EQ(consumer_control.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);
  }

  // Release.
  SetDefaultGpioReadResponse(0, 0);
  fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 0);
  }
}

TEST_F(ButtonsTest, DirectButtonPushReleasePush) {
  Init(kMetadataSingleButtonDirect);

  SetGpioReadResponse(0, 0);
  fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());

  SetGpioReadResponse(0, 1);
  fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());

  SetGpioReadResponse(0, 0);
  fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
}

TEST_F(ButtonsTest, DirectButtonFlaky) {
  Init(kMetadataSingleButtonDirect);

  SetGpioReadResponse(0, 1);
  SetGpioReadResponse(0, 0);
  SetDefaultGpioReadResponse(0, 1);  // Stabilizes.

  fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());

  auto reader = GetReader();
  auto result = reader->ReadInputReports();
  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_ok());
  auto& reports = result->value()->reports;

  ASSERT_EQ(reports.count(), 1);
  auto& report = reports[0];

  ASSERT_TRUE(report.has_event_time());
  ASSERT_TRUE(report.has_consumer_control());
  auto& consumer_control = report.consumer_control();

  ASSERT_TRUE(consumer_control.has_pressed_buttons());
  ASSERT_EQ(consumer_control.pressed_buttons().count(), 1);
  EXPECT_EQ(consumer_control.pressed_buttons()[0],
            fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);
}

TEST_F(ButtonsTest, MatrixButtonInit) { Init(kMetadataMatrix); }

TEST_F(ButtonsTest, MatrixButtonPush) {
  Init(kMetadataMatrix);

  auto reader = GetReader();

  // Initial reads.
  SetGpioReadResponse(0, 0);
  SetGpioReadResponse(0, 0);
  SetGpioReadResponse(1, 0);
  SetGpioReadResponse(1, 0);

  SetGpioReadResponse(0, 1);  // Read row. Matrix Scan for 0.
  SetGpioReadResponse(0, 0);  // Read row. Matrix Scan for 2.
  SetGpioReadResponse(1, 0);  // Read row. Matrix Scan for 1.
  SetGpioReadResponse(1, 0);  // Read row. Matrix Scan for 3.

  fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());

  auto result = reader->ReadInputReports();
  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_ok());
  auto& reports = result->value()->reports;

  ASSERT_EQ(reports.count(), 1);
  auto& report = reports[0];

  ASSERT_TRUE(report.has_event_time());
  ASSERT_TRUE(report.has_consumer_control());
  auto& consumer_control = report.consumer_control();

  ASSERT_TRUE(consumer_control.has_pressed_buttons());
  ASSERT_EQ(consumer_control.pressed_buttons().count(), 1);
  EXPECT_EQ(consumer_control.pressed_buttons()[0],
            fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);

  incoming_.SyncCall([&](IncomingNamespace* incoming) {
    auto gpio_2_states = incoming->fake_gpio_servers_[2].GetStateLog();
    ASSERT_GE(gpio_2_states.size(), 4);
    ASSERT_EQ(fake_gpio::ReadSubState{.flags = fuchsia_hardware_gpio::GpioFlags::kNoPull},
              (gpio_2_states.end() - 4)->sub_state);  // Float column.
    ASSERT_EQ(fake_gpio::WriteSubState{.value = gpios_matrix[2].matrix.output_value},
              (gpio_2_states.end() - 3)->sub_state);  // Restore column.
    ASSERT_EQ(fake_gpio::ReadSubState{.flags = fuchsia_hardware_gpio::GpioFlags::kNoPull},
              (gpio_2_states.end() - 2)->sub_state);  // Float column.
    ASSERT_EQ(fake_gpio::WriteSubState{.value = gpios_matrix[2].matrix.output_value},
              (gpio_2_states.end() - 1)->sub_state);  // Restore column.

    auto gpio_3_states = incoming->fake_gpio_servers_[3].GetStateLog();
    ASSERT_GE(gpio_3_states.size(), 4);
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
  Init(kMetadataDuplicate);

  auto reader = GetReader();

  // Holding FDR (VOL_UP and VOL_DOWN), then release VOL_UP, should only get one report
  // for the FDR and one report for the VOL_UP. When FDR is released, there is no
  // new report generated since the reported values do not change.

  // Push FDR (both VOL_UP and VOL_DOWN).
  SetGpioReadResponse(2, 1);
  SetGpioReadResponse(2, 1);

  // Report.
  SetGpioReadResponse(0, 1);
  SetGpioReadResponse(1, 1);
  SetGpioReadResponse(2, 1);

  // Release VOL_UP.
  SetGpioReadResponse(0, 0);
  SetGpioReadResponse(0, 0);

  // Report.
  SetGpioReadResponse(0, 0);
  SetGpioReadResponse(1, 1);
  SetGpioReadResponse(2, 0);

  // Release FDR (both VOL_UP and VOL_DOWN).
  SetGpioReadResponse(2, 0);
  SetGpioReadResponse(2, 0);

  // Report (same as before).
  SetGpioReadResponse(0, 0);
  SetGpioReadResponse(1, 1);
  SetGpioReadResponse(2, 0);

  fake_gpio_interrupts_[2].trigger(0, zx::clock::get_monotonic());
  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 3);
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

  fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1);
    auto& report = reports[0];
    report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 1);
    EXPECT_EQ(consumer_control.pressed_buttons()[0],
              fuchsia_input_report::ConsumerControlButton::kVolumeDown);
  }

  fake_gpio_interrupts_[2].trigger(0, zx::clock::get_monotonic());
}

TEST_F(ButtonsTest, CamMute) {
  Init(kMetadataMultiple);

  auto reader = GetReader();

  // Push camera mute.
  SetGpioReadResponse(2, 1);
  SetGpioReadResponse(2, 1);

  // Report.
  SetGpioReadResponse(0, 0);
  SetGpioReadResponse(1, 0);
  SetGpioReadResponse(2, 1);

  fake_gpio_interrupts_[2].trigger(0, zx::clock::get_monotonic());

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 1);
    EXPECT_EQ(consumer_control.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kCameraDisable);
  }

  // Release camera mute.
  SetGpioReadResponse(2, 0);
  SetGpioReadResponse(2, 0);

  // Report.
  SetGpioReadResponse(0, 0);
  SetGpioReadResponse(1, 0);
  SetGpioReadResponse(2, 0);

  fake_gpio_interrupts_[2].trigger(0, zx::clock::get_monotonic());

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 0);
  }
}

TEST_F(ButtonsTest, DirectButtonWakeable) {
  SetExpectedInterruptFlags(ZX_INTERRUPT_MODE_EDGE_HIGH | ZX_INTERRUPT_WAKE_VECTOR);

  Init(kMetadataWakeable);

  auto reader = GetReader();

  // Push.
  SetDefaultGpioReadResponse(0, 1);

  fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 1);
    EXPECT_EQ(consumer_control.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);
  }

  // Release.
  SetDefaultGpioReadResponse(0, 0);
  fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 0);
  }
}

TEST_F(ButtonsTest, PollOneButton) {
  Init(kMetadataPolled);

  auto reader = GetReader();

  // All GPIOs must have a default read value if polling is being used, as they are all ready
  // every poll period.
  SetDefaultGpioReadResponse(0, 1);

  fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 1);
    EXPECT_EQ(consumer_control.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);
  }

  SetDefaultGpioReadResponse(1, 1);

  fake_gpio_interrupts_[0].trigger(0, zx::clock::get_monotonic());
  fake_gpio_interrupts_[1].trigger(0, zx::clock::get_monotonic());
  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 2);
    std::set<fuchsia_input_report::wire::ConsumerControlButton> pressed_buttons;
    for (const auto& button : consumer_control.pressed_buttons()) {
      pressed_buttons.insert(button);
    }
    const std::set<fuchsia_input_report::wire::ConsumerControlButton> expected_buttons = {
        fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp,
        fuchsia_input_report::wire::ConsumerControlButton::kMicMute};
    EXPECT_EQ(expected_buttons, pressed_buttons);
  }
  SetDefaultGpioReadResponse(0, 0);

  fake_gpio_interrupts_[1].trigger(0, zx::clock::get_monotonic());
  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 1);
    EXPECT_EQ(consumer_control.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kMicMute);
  }

  SetDefaultGpioReadResponse(1, 0);

  {
    auto result = reader->ReadInputReports();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 1);
    auto& report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 0);
  }
}

}  // namespace buttons
