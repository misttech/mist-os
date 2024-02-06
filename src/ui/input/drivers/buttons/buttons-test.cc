// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <sys/types.h>
#include <unistd.h>
#include <zircon/assert.h>

#include <cstddef>
#include <set>

#include <ddk/metadata/buttons.h>
#include <zxtest/zxtest.h>

#include "buttons-device.h"
#include "src/devices/gpio/testing/fake-gpio/fake-gpio.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {
static const buttons_button_config_t buttons_direct[] = {
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_VOLUME_UP, 0, 0, 0},
};

static const buttons_gpio_config_t gpios_direct[] = {
    {BUTTONS_GPIO_TYPE_INTERRUPT,
     0,
     {.interrupt = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kNoPull)}}}};

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

class ButtonsDeviceTest : public ButtonsDevice {
 public:
  explicit ButtonsDeviceTest(zx_device_t* fake_parent, async_dispatcher_t* dispatcher)
      : ButtonsDevice(fake_parent, dispatcher) {}

  void FakeInterrupt(uint32_t i = 0) {
    // Issue the first interrupt.
    zx_port_packet packet = {kPortKeyInterruptStart + i, ZX_PKT_TYPE_USER, ZX_OK, {}};
    zx_status_t status = port_.queue(&packet);
    ZX_ASSERT(status == ZX_OK);
  }

  void DebounceWait() {
    sync_completion_wait(&debounce_threshold_passed_, ZX_TIME_INFINITE);
    sync_completion_reset(&debounce_threshold_passed_);
  }

  void Notify(uint32_t type) override {
    ButtonsDevice::Notify(type);
    sync_completion_signal(&debounce_threshold_passed_);
  }

 private:
  sync_completion_t debounce_threshold_passed_;
};

class ButtonsTest : public zxtest::Test {
 public:
  void SetUp() override { ASSERT_OK(gpio_loop_.StartThread("gpios")); }

  void TearDown() override {
    // If your device has an unbind function:
    device().zxdev()->UnbindOp();
    EXPECT_TRUE(device().zxdev()->UnbindReplyCalled());
  }

 protected:
  void BindDevice(const buttons_gpio_config_t* gpios_config, size_t gpios_config_size,
                  const buttons_button_config_t* buttons_config, size_t buttons_config_size) {
    gpios_.clear();
    for (size_t i = 0; i < gpios_config_size; i++) {
      auto gpio = std::make_unique<async_patterns::TestDispatcherBound<fake_gpio::FakeGpio>>(
          gpio_loop_.dispatcher(), std::in_place);
      SetupGpio(*gpio, gpios_config[i]);
      gpios_.emplace_back(std::move(gpio));
    }

    const size_t n_gpios = gpios_config_size;
    auto gpios = fbl::Array(new ButtonsDevice::Gpio[n_gpios], n_gpios);
    const size_t n_buttons = buttons_config_size;
    auto buttons = fbl::Array(new buttons_button_config_t[n_buttons], n_buttons);
    for (size_t i = 0; i < n_gpios; ++i) {
      gpios[i].client.Bind(gpios_[i]->SyncCall(&fake_gpio::FakeGpio::Connect));
      gpios[i].config = gpios_config[i];
    }

    for (size_t i = 0; i < n_buttons; ++i) {
      buttons[i] = buttons_config[i];
      switch (buttons_config[i].type)
      case BUTTONS_TYPE_DIRECT:
      case BUTTONS_TYPE_MATRIX: {
        gpios_[buttons[i].gpioA_idx]->SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                                               zx::ok<uint8_t>(0));
        break;
        default:
          FAIL();
      }
    }

    auto device =
        std::make_unique<ButtonsDeviceTest>(fake_parent_.get(), input_report_loop_.dispatcher());
    ASSERT_OK(device->Bind(std::move(gpios), std::move(buttons)));
    // devmgr is now in charge of the memory for dev.
    device_ = device.release();
    EXPECT_OK(input_report_loop_.RunUntilIdle());
  }

  fidl::WireClient<fuchsia_input_report::InputReportsReader> GetReader() {
    auto endpoints = fidl::CreateEndpoints<fuchsia_input_report::InputDevice>();
    EXPECT_OK(endpoints);
    fidl::BindServer(input_report_loop_.dispatcher(), std::move(endpoints->server), &device());
    fidl::WireSyncClient<fuchsia_input_report::InputDevice> client(std::move(endpoints->client));

    auto reader_endpoints = fidl::CreateEndpoints<fuchsia_input_report::InputReportsReader>();
    EXPECT_OK(reader_endpoints.status_value());
    auto result = client->GetInputReportsReader(std::move(reader_endpoints->server));
    EXPECT_TRUE(result.ok());
    auto reader = fidl::WireClient<fuchsia_input_report::InputReportsReader>(
        std::move(reader_endpoints->client), input_report_loop_.dispatcher());

    EXPECT_OK(input_report_loop_.RunUntilIdle());

    DrainInitialReport(reader);

    return reader;
  }

  void ReconfigureGpioPolarity(size_t gpio_index, uint8_t read_value,
                               fuchsia_hardware_gpio::GpioPolarity expected_polarity) {
    GetGpio(gpio_index).SyncCall([read_value, expected_polarity](fake_gpio::FakeGpio* gpio) {
      gpio->PushReadResponse(zx::ok(read_value));
      gpio->PushReadCallback(
          [read_value, expected_polarity](fake_gpio::FakeGpio& gpio) -> zx::result<uint8_t> {
            EXPECT_EQ(gpio.GetPolarity(), expected_polarity);
            return zx::ok(read_value);
          });
    });
  }

  async_patterns::TestDispatcherBound<fake_gpio::FakeGpio>& GetGpio(size_t index) {
    return *gpios_[index];
  }
  ButtonsDeviceTest& device() { return *device_; }

  async::Loop input_report_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};

 private:
  static void SetupGpio(async_patterns::TestDispatcherBound<fake_gpio::FakeGpio>& gpio,
                        const buttons_gpio_config_t& gpio_config) {
    if (gpio_config.type == BUTTONS_GPIO_TYPE_INTERRUPT) {
      gpio.SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                    zx::ok<uint8_t>(0));  // Not pushed, low.
      gpio.SyncCall(&fake_gpio::FakeGpio::PushReadResponse, zx::ok<uint8_t>(0));  // Not pushed.
      gpio.SyncCall(&fake_gpio::FakeGpio::PushReadCallback, [](fake_gpio::FakeGpio& gpio) {
        ZX_ASSERT(gpio.GetPolarity() ==
                  fuchsia_hardware_gpio::GpioPolarity::kHigh);  // Set correct polarity.
        return zx::ok<uint8_t>(0);                              // Still not pushed.
      });
    } else if (gpio_config.type == BUTTONS_GPIO_TYPE_POLL) {
      gpio.SyncCall(&fake_gpio::FakeGpio::SetDefaultReadResponse, zx::ok<uint8_t>(0));
    }
  }

  void DrainInitialReport(fidl::WireClient<fuchsia_input_report::InputReportsReader>& reader) {
    reader->ReadInputReports().Then([&](auto& result) {
      ASSERT_TRUE(result.ok());
      ASSERT_TRUE(result->is_ok());
      auto& reports = result->value()->reports;

      ASSERT_EQ(reports.count(), 1);
      auto& report = reports[0];

      ASSERT_TRUE(report.has_event_time());
      ASSERT_TRUE(report.has_consumer_control());
      auto& consumer_control = report.consumer_control();

      ASSERT_TRUE(consumer_control.has_pressed_buttons());

      input_report_loop_.Quit();
    });
    EXPECT_EQ(input_report_loop_.Run(), ZX_ERR_CANCELED);
    EXPECT_OK(input_report_loop_.ResetQuit());
  }

  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();
  async::Loop gpio_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  std::vector<std::unique_ptr<async_patterns::TestDispatcherBound<fake_gpio::FakeGpio>>> gpios_;
  ButtonsDeviceTest* device_;
};

TEST_F(ButtonsTest, DirectButtonBind) {
  BindDevice(gpios_direct, std::size(gpios_direct), buttons_direct, std::size(buttons_direct));
}

TEST_F(ButtonsTest, DirectButtonPush) {
  BindDevice(gpios_direct, std::size(gpios_direct), buttons_direct, std::size(buttons_direct));

  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(0, 1, fuchsia_hardware_gpio::GpioPolarity::kLow);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse, zx::ok<uint8_t>(1));
  device().FakeInterrupt();
  device().DebounceWait();
}

TEST_F(ButtonsTest, DirectButtonPushUnpushedReport) {
  BindDevice(gpios_direct, std::size(gpios_direct), buttons_direct, std::size(buttons_direct));

  auto reader = GetReader();

  // Push (must push first before testing unpush because the default last_report_ is nothing pushed)
  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(0, 1, fuchsia_hardware_gpio::GpioPolarity::kLow);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse, zx::ok<uint8_t>(1));
  device().FakeInterrupt();
  device().DebounceWait();

  reader->ReadInputReports().Then([&](auto& result) {
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

    input_report_loop_.Quit();
  });
  EXPECT_EQ(input_report_loop_.Run(), ZX_ERR_CANCELED);
  EXPECT_OK(input_report_loop_.ResetQuit());

  // Unpush
  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(0, 0, fuchsia_hardware_gpio::GpioPolarity::kHigh);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse, zx::ok<uint8_t>(0));
  device().FakeInterrupt();
  device().DebounceWait();

  reader->ReadInputReports().Then([&](auto& result) {
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

    input_report_loop_.Quit();
  });
  EXPECT_EQ(input_report_loop_.Run(), ZX_ERR_CANCELED);
}

TEST_F(ButtonsTest, DirectButtonPushedReport) {
  BindDevice(gpios_direct, std::size(gpios_direct), buttons_direct, std::size(buttons_direct));

  auto reader = GetReader();

  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(0, 1, fuchsia_hardware_gpio::GpioPolarity::kLow);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse, zx::ok<uint8_t>(1));
  device().FakeInterrupt();
  device().DebounceWait();

  reader->ReadInputReports().Then([&](auto& result) {
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

    input_report_loop_.Quit();
  });
  EXPECT_EQ(input_report_loop_.Run(), ZX_ERR_CANCELED);
}

TEST_F(ButtonsTest, DirectButtonPushUnpushPush) {
  BindDevice(gpios_direct, std::size(gpios_direct), buttons_direct, std::size(buttons_direct));

  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(0, 1, fuchsia_hardware_gpio::GpioPolarity::kLow);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse, zx::ok<uint8_t>(1));
  device().FakeInterrupt();
  device().DebounceWait();

  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(0, 0, fuchsia_hardware_gpio::GpioPolarity::kHigh);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse, zx::ok<uint8_t>(0));
  device().FakeInterrupt();
  device().DebounceWait();

  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(0, 1, fuchsia_hardware_gpio::GpioPolarity::kLow);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse, zx::ok<uint8_t>(1));
  device().FakeInterrupt();
  device().DebounceWait();
}

TEST_F(ButtonsTest, DirectButtonFlaky) {
  BindDevice(gpios_direct, std::size(gpios_direct), buttons_direct, std::size(buttons_direct));

  // Reconfigure Polarity due to interrupt and keep checking until correct.
  GetGpio(0).SyncCall([](fake_gpio::FakeGpio* gpio) {
    gpio->PushReadResponse(zx::ok<uint8_t>(1));  // Pushed.
    gpio->PushReadCallback([](fake_gpio::FakeGpio& gpio) -> zx::result<uint8_t> {
      ZX_ASSERT(gpio.GetPolarity() ==
                fuchsia_hardware_gpio::GpioPolarity::kLow);  // Turn the polarity.
      return zx::ok<uint8_t>(0);                             // Oops now not pushed! not ok, retry.
    });
    gpio->PushReadCallback([](fake_gpio::FakeGpio& gpio) -> zx::result<uint8_t> {
      ZX_ASSERT(gpio.GetPolarity() ==
                fuchsia_hardware_gpio::GpioPolarity::kHigh);  // Turn the polarity.
      return zx::ok<uint8_t>(1);                              // Oops pushed! not ok, retry.
    });
    gpio->PushReadCallback([](fake_gpio::FakeGpio& gpio) -> zx::result<uint8_t> {
      ZX_ASSERT(gpio.GetPolarity() ==
                fuchsia_hardware_gpio::GpioPolarity::kLow);  // Turn the polarity.
      return zx::ok<uint8_t>(0);                             // Oops now not pushed! not ok, retry.
    });
    gpio->PushReadCallback([](fake_gpio::FakeGpio& gpio) -> zx::result<uint8_t> {
      ZX_ASSERT(gpio.GetPolarity() ==
                fuchsia_hardware_gpio::GpioPolarity::kHigh);  // Turn the polarity.
      return zx::ok<uint8_t>(1);                              // Oops pushed again! not ok, retry.
    });
    gpio->PushReadCallback([](fake_gpio::FakeGpio& gpio) -> zx::result<uint8_t> {
      ZX_ASSERT(gpio.GetPolarity() ==
                fuchsia_hardware_gpio::GpioPolarity::kLow);  // Turn the polarity.
      return zx::ok<uint8_t>(1);                             // Now pushed and polarity set low, ok.
    });
    gpio->PushReadResponse(zx::ok<uint8_t>(1));  // Pushed.
  });
  // Read value to generate report.
  device().FakeInterrupt();
  device().DebounceWait();
}

TEST_F(ButtonsTest, MatrixButtonBind) {
  BindDevice(gpios_matrix, std::size(gpios_matrix), buttons_matrix, std::size(buttons_matrix));
}

TEST_F(ButtonsTest, MatrixButtonPush) {
  BindDevice(gpios_matrix, std::size(gpios_matrix), buttons_matrix, std::size(buttons_matrix));

  auto reader = GetReader();

  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(0, 1, fuchsia_hardware_gpio::GpioPolarity::kLow);

  GetGpio(0).SyncCall([](fake_gpio::FakeGpio* gpio) {
    // Matrix Scan for 0.
    gpio->PushReadResponse(zx::ok<uint8_t>(1));  // Read row.

    // Matrix Scan for 2.
    gpio->PushReadResponse(zx::ok<uint8_t>(0));  // Read row.
  });

  GetGpio(1).SyncCall([](fake_gpio::FakeGpio* gpio) {
    // Matrix Scan for 1.
    gpio->PushReadResponse(zx::ok<uint8_t>(0));  // Read row.

    // Matrix Scan for 3.
    gpio->PushReadResponse(zx::ok<uint8_t>(0));  // Read row.
  });

  device().FakeInterrupt();
  device().DebounceWait();

  reader->ReadInputReports().Then([&](auto& result) {
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

    input_report_loop_.Quit();
  });
  EXPECT_EQ(input_report_loop_.Run(), ZX_ERR_CANCELED);

  auto gpio_2_states = GetGpio(2).SyncCall(&fake_gpio::FakeGpio::GetStateLog);
  ASSERT_GE(gpio_2_states.size(), 4);
  ASSERT_EQ(fake_gpio::ReadSubState{.flags = fuchsia_hardware_gpio::GpioFlags::kNoPull},
            (gpio_2_states.end() - 4)->sub_state);  // Float column.
  ASSERT_EQ(fake_gpio::WriteSubState{.value = gpios_matrix[2].matrix.output_value},
            (gpio_2_states.end() - 3)->sub_state);  // Restore column.
  ASSERT_EQ(fake_gpio::ReadSubState{.flags = fuchsia_hardware_gpio::GpioFlags::kNoPull},
            (gpio_2_states.end() - 2)->sub_state);  // Float column.
  ASSERT_EQ(fake_gpio::WriteSubState{.value = gpios_matrix[2].matrix.output_value},
            (gpio_2_states.end() - 1)->sub_state);  // Restore column.

  auto gpio_3_states = GetGpio(3).SyncCall(&fake_gpio::FakeGpio::GetStateLog);
  ASSERT_GE(gpio_3_states.size(), 4);
  ASSERT_EQ(fake_gpio::ReadSubState{.flags = fuchsia_hardware_gpio::GpioFlags::kNoPull},
            (gpio_3_states.end() - 4)->sub_state);  // Float column.
  ASSERT_EQ(fake_gpio::WriteSubState{.value = gpios_matrix[3].matrix.output_value},
            (gpio_3_states.end() - 3)->sub_state);  // Restore column.
  ASSERT_EQ(fake_gpio::ReadSubState{.flags = fuchsia_hardware_gpio::GpioFlags::kNoPull},
            (gpio_3_states.end() - 2)->sub_state);  // Float column.
  ASSERT_EQ(fake_gpio::WriteSubState{.value = gpios_matrix[3].matrix.output_value},
            (gpio_3_states.end() - 1)->sub_state);  // Restore column.
}

TEST_F(ButtonsTest, DuplicateReports) {
  BindDevice(gpios_duplicate, std::size(gpios_duplicate), buttons_duplicate,
             std::size(buttons_duplicate));

  auto reader = GetReader();

  // Holding FDR (VOL_UP and VOL_DOWN), then release VOL_UP, should only get one report.
  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(2, 1, fuchsia_hardware_gpio::GpioPolarity::kLow);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(1));  // Read value to prepare report.
  GetGpio(1).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(1));  // Read value to prepare report.
  GetGpio(2).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(1));  // Read value to prepare report.
  device().FakeInterrupt(2);
  device().DebounceWait();

  ReconfigureGpioPolarity(0, 0, fuchsia_hardware_gpio::GpioPolarity::kHigh);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  GetGpio(1).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(1));  // Read value to prepare report.
  GetGpio(2).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  device().FakeInterrupt(0);
  device().DebounceWait();

  ReconfigureGpioPolarity(2, 0, fuchsia_hardware_gpio::GpioPolarity::kHigh);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  GetGpio(1).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(1));  // Read value to prepare report.
  GetGpio(2).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  device().FakeInterrupt(2);
  device().DebounceWait();

  reader->ReadInputReports().Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto& reports = result->value()->reports;

    ASSERT_EQ(reports.count(), 2);
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

    report = reports[1];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    ASSERT_EQ(consumer_control.pressed_buttons().count(), 1);
    EXPECT_EQ(consumer_control.pressed_buttons()[0],
              fuchsia_input_report::ConsumerControlButton::kVolumeDown);

    input_report_loop_.Quit();
  });
  EXPECT_EQ(input_report_loop_.Run(), ZX_ERR_CANCELED);
}

TEST_F(ButtonsTest, CamMute) {
  BindDevice(gpios_multiple, std::size(gpios_multiple), buttons_multiple,
             std::size(buttons_multiple));

  auto reader = GetReader();

  ReconfigureGpioPolarity(2, 1, fuchsia_hardware_gpio::GpioPolarity::kLow);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  GetGpio(1).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  GetGpio(2).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(1));  // Read value to prepare report.
  device().FakeInterrupt(2);
  device().DebounceWait();

  reader->ReadInputReports().Then([&](auto& result) {
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

    input_report_loop_.Quit();
  });
  EXPECT_EQ(input_report_loop_.Run(), ZX_ERR_CANCELED);
  EXPECT_OK(input_report_loop_.ResetQuit());

  ReconfigureGpioPolarity(2, 0, fuchsia_hardware_gpio::GpioPolarity::kHigh);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  GetGpio(1).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  GetGpio(2).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  device().FakeInterrupt(2);
  device().DebounceWait();

  reader->ReadInputReports().Then([&](auto& result) {
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
    input_report_loop_.Quit();
  });
  EXPECT_EQ(input_report_loop_.Run(), ZX_ERR_CANCELED);
}

TEST_F(ButtonsTest, PollOneButton) {
  BindDevice(gpios_multiple_one_polled, std::size(gpios_multiple_one_polled), buttons_multiple,
             std::size(buttons_multiple));

  auto reader = GetReader();

  // All GPIOs must have a default read value if polling is being used, as they are all ready
  // every poll period.
  GetGpio(2).SyncCall(&fake_gpio::FakeGpio::SetDefaultReadResponse, zx::ok<uint8_t>(0));

  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::SetDefaultReadResponse, zx::ok<uint8_t>(1));
  device().FakeInterrupt();
  device().DebounceWait();

  reader->ReadInputReports().Then([&](auto& result) {
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

    input_report_loop_.Quit();
  });
  EXPECT_EQ(input_report_loop_.Run(), ZX_ERR_CANCELED);
  EXPECT_OK(input_report_loop_.ResetQuit());

  GetGpio(1).SyncCall(&fake_gpio::FakeGpio::SetDefaultReadResponse, zx::ok<uint8_t>(1));
  device().DebounceWait();

  reader->ReadInputReports().Then([&](auto& result) {
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

    input_report_loop_.Quit();
  });
  EXPECT_EQ(input_report_loop_.Run(), ZX_ERR_CANCELED);
  EXPECT_OK(input_report_loop_.ResetQuit());

  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::SetDefaultReadResponse, zx::ok<uint8_t>(0));
  device().FakeInterrupt();
  device().DebounceWait();

  reader->ReadInputReports().Then([&](auto& result) {
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

    input_report_loop_.Quit();
  });
  EXPECT_EQ(input_report_loop_.Run(), ZX_ERR_CANCELED);
  EXPECT_OK(input_report_loop_.ResetQuit());

  GetGpio(1).SyncCall(&fake_gpio::FakeGpio::SetDefaultReadResponse, zx::ok<uint8_t>(0));
  device().DebounceWait();

  reader->ReadInputReports().Then([&](auto& result) {
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

    input_report_loop_.Quit();
  });
  EXPECT_EQ(input_report_loop_.Run(), ZX_ERR_CANCELED);
}

}  // namespace buttons
