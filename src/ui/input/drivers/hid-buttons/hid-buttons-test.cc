// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "hid-buttons.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <sys/types.h>
#include <unistd.h>
#include <zircon/assert.h>

#include <cstddef>

#include <ddk/metadata/buttons.h>
#include <zxtest/zxtest.h>

#include "src/devices/gpio/testing/fake-gpio/fake-gpio.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "zircon/errors.h"

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

class HidButtonsDeviceTest : public HidButtonsDevice {
 public:
  explicit HidButtonsDeviceTest(zx_device_t* fake_parent) : HidButtonsDevice(fake_parent) {}

  void FakeInterrupt() {
    // Issue the first interrupt.
    zx_port_packet packet = {kPortKeyInterruptStart + 0, ZX_PKT_TYPE_USER, ZX_OK, {}};
    zx_status_t status = port_.queue(&packet);
    ZX_ASSERT(status == ZX_OK);
  }

  void FakeInterrupt(ButtonType type) {
    // Issue the first interrupt.
    zx_port_packet packet = {
        kPortKeyInterruptStart + button_map_[type], ZX_PKT_TYPE_USER, ZX_OK, {}};
    zx_status_t status = port_.queue(&packet);
    ZX_ASSERT(status == ZX_OK);
  }

  void DebounceWait() {
    sync_completion_wait(&debounce_threshold_passed_, ZX_TIME_INFINITE);
    sync_completion_reset(&debounce_threshold_passed_);
  }

  void ClosingChannel(ButtonsNotifyInterface* interface) override {
    HidButtonsDevice::ClosingChannel(interface);
    sync_completion_signal(&test_channels_cleared_);
  }

  void Notify(uint32_t type) override {
    HidButtonsDevice::Notify(type);
    sync_completion_signal(&debounce_threshold_passed_);
  }

  void Wait() {
    sync_completion_wait(&test_channels_cleared_, ZX_TIME_INFINITE);
    sync_completion_reset(&test_channels_cleared_);
  }

 private:
  sync_completion_t test_channels_cleared_;
  sync_completion_t debounce_threshold_passed_;
};

class HidButtonsTest : public zxtest::Test {
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
    auto gpios = fbl::Array(new HidButtonsDevice::Gpio[n_gpios], n_gpios);
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

    auto device = std::make_unique<HidButtonsDeviceTest>(fake_parent_.get());
    ASSERT_OK(device->Bind(std::move(gpios), std::move(buttons)));
    // devmgr is now in charge of the memory for dev.
    device_ = device.release();
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
      gpio->PushReadResponse(zx::ok(read_value));
    });
  }

  async_patterns::TestDispatcherBound<fake_gpio::FakeGpio>& GetGpio(size_t index) {
    return *gpios_[index];
  }
  HidButtonsDeviceTest& device() { return *device_; }

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

  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();
  async::Loop gpio_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  std::vector<std::unique_ptr<async_patterns::TestDispatcherBound<fake_gpio::FakeGpio>>> gpios_;
  HidButtonsDeviceTest* device_;
};

TEST_F(HidButtonsTest, DirectButtonBind) {
  BindDevice(gpios_direct, std::size(gpios_direct), buttons_direct, std::size(buttons_direct));
}

TEST_F(HidButtonsTest, DirectButtonPush) {
  BindDevice(gpios_direct, std::size(gpios_direct), buttons_direct, std::size(buttons_direct));

  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(0, 1, fuchsia_hardware_gpio::GpioPolarity::kLow);
  device().FakeInterrupt();
  device().DebounceWait();
}

TEST_F(HidButtonsTest, DirectButtonUnpushedReport) {
  BindDevice(gpios_direct, std::size(gpios_direct), buttons_direct, std::size(buttons_direct));

  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(0, 0, fuchsia_hardware_gpio::GpioPolarity::kHigh);
  device().FakeInterrupt();
  device().DebounceWait();

  hidbus_ifc_protocol_ops_t ops = {};
  ops.io_queue = [](void* ctx, const uint8_t* buffer, size_t size, zx_time_t time) {
    buttons_input_rpt_t report_volume_up = {};
    report_volume_up.rpt_id = 1;
    report_volume_up.volume_up = 0;  // Unpushed.
    ASSERT_BYTES_EQ(buffer, &report_volume_up, size);
    EXPECT_EQ(size, sizeof(report_volume_up));
  };
  hidbus_ifc_protocol_t protocol = {};
  protocol.ops = &ops;
  device().HidbusStart(&protocol);
}

TEST_F(HidButtonsTest, DirectButtonPushedReport) {
  BindDevice(gpios_direct, std::size(gpios_direct), buttons_direct, std::size(buttons_direct));

  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(0, 1, fuchsia_hardware_gpio::GpioPolarity::kLow);
  device().FakeInterrupt();
  device().DebounceWait();

  hidbus_ifc_protocol_ops_t ops = {};
  ops.io_queue = [](void* ctx, const uint8_t* buffer, size_t size, zx_time_t time) {
    buttons_input_rpt_t report_volume_up = {};
    report_volume_up.rpt_id = 1;
    report_volume_up.volume_up = 1;  // Pushed
    ASSERT_BYTES_EQ(buffer, &report_volume_up, size);
    EXPECT_EQ(size, sizeof(report_volume_up));
  };
  hidbus_ifc_protocol_t protocol = {};
  protocol.ops = &ops;
  device().HidbusStart(&protocol);
}

TEST_F(HidButtonsTest, DirectButtonPushUnpushPush) {
  BindDevice(gpios_direct, std::size(gpios_direct), buttons_direct, std::size(buttons_direct));

  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(0, 1, fuchsia_hardware_gpio::GpioPolarity::kLow);
  device().FakeInterrupt();
  device().DebounceWait();

  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(0, 0, fuchsia_hardware_gpio::GpioPolarity::kHigh);
  device().FakeInterrupt();
  device().DebounceWait();

  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(0, 1, fuchsia_hardware_gpio::GpioPolarity::kLow);
  device().FakeInterrupt();
  device().DebounceWait();
}

TEST_F(HidButtonsTest, DirectButtonFlaky) {
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

TEST_F(HidButtonsTest, MatrixButtonBind) {
  BindDevice(gpios_matrix, std::size(gpios_matrix), buttons_matrix, std::size(buttons_matrix));
}

TEST_F(HidButtonsTest, MatrixButtonPush) {
  BindDevice(gpios_matrix, std::size(gpios_matrix), buttons_matrix, std::size(buttons_matrix));

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

  hidbus_ifc_protocol_ops_t ops = {};
  ops.io_queue = [](void* ctx, const uint8_t* buffer, size_t size, zx_time_t time) {
    buttons_input_rpt_t report_volume_up = {};
    report_volume_up.rpt_id = 1;
    report_volume_up.volume_up = 1;
    ASSERT_BYTES_EQ(buffer, &report_volume_up, size);
    EXPECT_EQ(size, sizeof(report_volume_up));
  };
  hidbus_ifc_protocol_t protocol = {};
  protocol.ops = &ops;
  device().HidbusStart(&protocol);
}

TEST_F(HidButtonsTest, DuplicateReports) {
  BindDevice(gpios_duplicate, std::size(gpios_duplicate), buttons_duplicate,
             std::size(buttons_duplicate));

  // Holding FDR (VOL_UP and VOL_DOWN), then release VOL_UP, should only get one report.
  // Reconfigure Polarity due to interrupt.
  ReconfigureGpioPolarity(2, 1, fuchsia_hardware_gpio::GpioPolarity::kLow);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(1));  // Read value to prepare report.
  GetGpio(1).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(1));  // Read value to prepare report.
  GetGpio(2).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(1));  // Read value to prepare report.
  device().FakeInterrupt(ButtonType::kReset);
  device().DebounceWait();

  ReconfigureGpioPolarity(0, 0, fuchsia_hardware_gpio::GpioPolarity::kHigh);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  GetGpio(1).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(1));  // Read value to prepare report.
  GetGpio(2).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  device().FakeInterrupt(ButtonType::kVolumeUp);
  device().DebounceWait();

  ReconfigureGpioPolarity(2, 0, fuchsia_hardware_gpio::GpioPolarity::kHigh);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  GetGpio(1).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(1));  // Read value to prepare report.
  GetGpio(2).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  device().FakeInterrupt(ButtonType::kReset);
  device().DebounceWait();

  hidbus_ifc_protocol_ops_t ops = {};
  ops.io_queue = [](void* ctx, const uint8_t* buffer, size_t size, zx_time_t time) {
    buttons_input_rpt_t reports[2];
    reports[0] = {};
    reports[0].rpt_id = 1;
    reports[0].volume_up = 1;    // Pushed.
    reports[0].volume_down = 1;  // Pushed.
    reports[0].reset = 1;        // Pushed.
    reports[1] = {};
    reports[1].rpt_id = 1;
    reports[1].volume_up = 0;    // Unpushed.
    reports[1].volume_down = 1;  // Pushed.
    reports[1].reset = 0;        // Unpushed.
    ASSERT_BYTES_EQ(buffer, reports, size);
    EXPECT_EQ(size, sizeof(reports));
  };
  hidbus_ifc_protocol_t protocol = {};
  protocol.ops = &ops;
  device().HidbusStart(&protocol);
}

TEST_F(HidButtonsTest, CamMute) {
  BindDevice(gpios_multiple, std::size(gpios_multiple), buttons_multiple,
             std::size(buttons_multiple));

  hidbus_ifc_protocol_ops_t ops = {};
  ops.io_queue = [](void* ctx, const uint8_t* buffer, size_t size, zx_time_t time) {
    buttons_input_rpt_t report_volume_up = {};
    report_volume_up.rpt_id = 1;
    report_volume_up.camera_access_disabled = 1;
    ASSERT_BYTES_EQ(buffer, &report_volume_up, size);
    EXPECT_EQ(size, sizeof(report_volume_up));
  };
  hidbus_ifc_protocol_t protocol = {};
  protocol.ops = &ops;
  EXPECT_OK(device().HidbusStart(&protocol));

  ReconfigureGpioPolarity(2, 1, fuchsia_hardware_gpio::GpioPolarity::kLow);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  GetGpio(1).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  GetGpio(2).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(1));  // Read value to prepare report.
  device().FakeInterrupt(ButtonType::kCamMute);
  device().DebounceWait();

  device().HidbusStop();

  ops.io_queue = [](void* ctx, const uint8_t* buffer, size_t size, zx_time_t time) {
    buttons_input_rpt_t report_volume_up = {};
    report_volume_up.rpt_id = 1;
    report_volume_up.camera_access_disabled = 0;
    ASSERT_BYTES_EQ(buffer, &report_volume_up, size);
    EXPECT_EQ(size, sizeof(report_volume_up));
  };
  protocol.ops = &ops;
  EXPECT_OK(device().HidbusStart(&protocol));

  ReconfigureGpioPolarity(2, 0, fuchsia_hardware_gpio::GpioPolarity::kHigh);
  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  GetGpio(1).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  GetGpio(2).SyncCall(&fake_gpio::FakeGpio::PushReadResponse,
                      zx::ok<uint8_t>(0));  // Read value to prepare report.
  device().FakeInterrupt(ButtonType::kCamMute);
  device().DebounceWait();
}

TEST_F(HidButtonsTest, PollOneButton) {
  BindDevice(gpios_multiple_one_polled, std::size(gpios_multiple_one_polled), buttons_multiple,
             std::size(buttons_multiple));

  // All GPIOs must have a default read value if polling is being used, as they are all ready
  // every poll period.
  GetGpio(2).SyncCall(&fake_gpio::FakeGpio::SetDefaultReadResponse, zx::ok<uint8_t>(0));

  std::vector<buttons_input_rpt_t> reports;

  hidbus_ifc_protocol_ops_t ops = {};
  ops.io_queue = [](void* ctx, const uint8_t* buffer, size_t size, zx_time_t time) {
    buttons_input_rpt_t report;
    ASSERT_EQ(size, sizeof(report));
    memcpy(&report, buffer, size);
    reinterpret_cast<std::vector<buttons_input_rpt_t>*>(ctx)->push_back(report);
  };
  hidbus_ifc_protocol_t protocol = {.ops = &ops, .ctx = &reports};
  device().HidbusStart(&protocol);

  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::SetDefaultReadResponse, zx::ok<uint8_t>(1));
  device().FakeInterrupt();
  device().DebounceWait();

  GetGpio(1).SyncCall(&fake_gpio::FakeGpio::SetDefaultReadResponse, zx::ok<uint8_t>(1));
  device().DebounceWait();

  GetGpio(0).SyncCall(&fake_gpio::FakeGpio::SetDefaultReadResponse, zx::ok<uint8_t>(0));
  device().FakeInterrupt();
  device().DebounceWait();

  GetGpio(1).SyncCall(&fake_gpio::FakeGpio::SetDefaultReadResponse, zx::ok<uint8_t>(0));
  device().DebounceWait();

  ASSERT_EQ(reports.size(), 4);

  EXPECT_EQ(reports[0].rpt_id, BUTTONS_RPT_ID_INPUT);
  EXPECT_EQ(reports[0].volume_up, 1);
  EXPECT_EQ(reports[0].mute, 0);

  EXPECT_EQ(reports[1].rpt_id, BUTTONS_RPT_ID_INPUT);
  EXPECT_EQ(reports[1].volume_up, 1);
  EXPECT_EQ(reports[1].mute, 1);

  EXPECT_EQ(reports[2].rpt_id, BUTTONS_RPT_ID_INPUT);
  EXPECT_EQ(reports[2].volume_up, 0);
  EXPECT_EQ(reports[2].mute, 1);

  EXPECT_EQ(reports[3].rpt_id, BUTTONS_RPT_ID_INPUT);
  EXPECT_EQ(reports[3].volume_up, 0);
  EXPECT_EQ(reports[3].mute, 0);
}

}  // namespace buttons
