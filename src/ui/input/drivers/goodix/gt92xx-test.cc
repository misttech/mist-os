// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gt92xx.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <lib/mock-i2c/mock-i2c.h>

#include <atomic>

#include <ddk/metadata/buttons.h>
#include <hid/gt92xx.h>
#include <zxtest/zxtest.h>

#include "src/devices/gpio/testing/fake-gpio/fake-gpio.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace goodix {

class Gt92xxTestDevice : public Gt92xxDevice {
 public:
  Gt92xxTestDevice(ddk::I2cChannel i2c, fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> intr,
                   fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> reset, zx_device_t* parent)
      : Gt92xxDevice(parent, std::move(i2c), std::move(intr), std::move(reset)) {}

  void Running(bool run) { Gt92xxDevice::running_.store(run); }

  zx_status_t Init() { return Gt92xxDevice::Init(); }

  void Trigger() { irq_.trigger(0, zx::time()); }
  zx_status_t StartThread() {
    EXPECT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &irq_));

    auto thunk = [](void* arg) -> int {
      return reinterpret_cast<Gt92xxTestDevice*>(arg)->Thread();
    };

    Running(true);
    int ret = thrd_create_with_name(&test_thread_, thunk, this, "gt92xx-test-thread");
    return (ret == thrd_success) ? ZX_OK : ZX_ERR_BAD_STATE;
  }
  zx_status_t StopThread() {
    Running(false);
    irq_.trigger(0, zx::time());
    int ret = thrd_join(test_thread_, NULL);
    return (ret == thrd_success) ? ZX_OK : ZX_ERR_BAD_STATE;
  }

  thrd_t test_thread_;
};

class Gt92xxTest : public zxtest::Test {
 public:
  void SetUp() override {
    ASSERT_OK(fidl_servers_loop_.StartThread("fidl-servers"));
    fidl::ClientEnd reset_gpio_client = reset_gpio_.SyncCall(&fake_gpio::FakeGpio::Connect);
    fidl::ClientEnd intr_gpio_client = intr_gpio_.SyncCall(&fake_gpio::FakeGpio::Connect);
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_i2c::Device>();
    ASSERT_TRUE(endpoints.is_ok());
    fidl::BindServer(fidl_servers_loop_.dispatcher(), std::move(endpoints->server), &mock_i2c_);
    fake_parent_ = MockDevice::FakeRootParent();
    device_.emplace(std::move(endpoints->client), std::move(intr_gpio_client),
                    std::move(reset_gpio_client), fake_parent_.get());
  }

  void TearDown() override {}

  void InitDevice() {
    ASSERT_OK(device_->Init());

    std::vector reset_states = reset_gpio_.SyncCall(&fake_gpio::FakeGpio::GetStateLog);
    ASSERT_GE(reset_states.size(), 2);
    ASSERT_EQ(fake_gpio::WriteState{.value = 0}, reset_states[0]);
    ASSERT_EQ(fake_gpio::WriteState{.value = 1}, reset_states[1]);

    std::vector intr_states = intr_gpio_.SyncCall(&fake_gpio::FakeGpio::GetStateLog);
    ASSERT_GE(intr_states.size(), 2);
    ASSERT_EQ(fake_gpio::WriteState{.value = 0}, intr_states[0]);
    ASSERT_EQ(fake_gpio::ReadState{.flags = fuchsia_hardware_gpio::GpioFlags::kPullUp},
              intr_states[1]);
  }

  async::Loop fidl_servers_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<fake_gpio::FakeGpio> reset_gpio_{
      fidl_servers_loop_.dispatcher(), std::in_place};
  async_patterns::TestDispatcherBound<fake_gpio::FakeGpio> intr_gpio_{
      fidl_servers_loop_.dispatcher(), std::in_place};
  mock_i2c::MockI2c mock_i2c_;
  std::optional<Gt92xxTestDevice> device_;
  std::shared_ptr<MockDevice> fake_parent_;
};

static std::atomic<uint8_t> rpt_ran = 0;

void rpt_handler(void* ctx, const uint8_t* buffer, size_t size, zx_time_t time) {
  gt92xx_touch_t touch_rpt = {};
  touch_rpt.rpt_id = GT92XX_RPT_ID_TOUCH;
  touch_rpt.fingers[0] = {0x01, 0x110, 0x100};
  touch_rpt.fingers[1] = {0x05, 0x220, 0x200};
  touch_rpt.fingers[2] = {0x09, 0x330, 0x300};
  touch_rpt.fingers[3] = {0x0d, 0x440, 0x400};
  touch_rpt.fingers[4] = {0x11, 0x550, 0x500};
  touch_rpt.contact_count = 5;
  ASSERT_BYTES_EQ(buffer, &touch_rpt, size);
  EXPECT_EQ(size, sizeof(touch_rpt));
  rpt_ran.store(1);
}

TEST_F(Gt92xxTest, Init) {
  mock_i2c_
      .ExpectWrite({static_cast<uint8_t>(GT_REG_CONFIG_DATA >> 8),
                    static_cast<uint8_t>(GT_REG_CONFIG_DATA & 0xff)})
      .ExpectReadStop({0x00})
      .ExpectWriteStop(Gt92xxDevice::GetConfData())
      .ExpectWriteStop({static_cast<uint8_t>(GT_REG_TOUCH_STATUS >> 8),
                        static_cast<uint8_t>(GT_REG_TOUCH_STATUS & 0xff), 0x00})
      .ExpectWrite({static_cast<uint8_t>(GT_REG_CONFIG_DATA >> 8),
                    static_cast<uint8_t>(GT_REG_CONFIG_DATA & 0xff)})
      .ExpectReadStop({0x00})
      .ExpectWrite({static_cast<uint8_t>(GT_REG_FW_VERSION >> 8),
                    static_cast<uint8_t>(GT_REG_FW_VERSION & 0xff)})
      .ExpectReadStop({0x05, 0x61});

  InitDevice();
}

TEST_F(Gt92xxTest, InitForceConfig) {
  fbl::Vector conf_data = Gt92xxDevice::GetConfData();
  EXPECT_NE(conf_data[sizeof(uint16_t)], 0x00);
  conf_data[sizeof(uint16_t)] = 0x00;

  mock_i2c_
      .ExpectWrite({static_cast<uint8_t>(GT_REG_CONFIG_DATA >> 8),
                    static_cast<uint8_t>(GT_REG_CONFIG_DATA & 0xff)})
      .ExpectReadStop({0x60})
      .ExpectWriteStop(std::move(conf_data))
      .ExpectWriteStop({static_cast<uint8_t>(GT_REG_TOUCH_STATUS >> 8),
                        static_cast<uint8_t>(GT_REG_TOUCH_STATUS & 0xff), 0x00})
      .ExpectWrite({static_cast<uint8_t>(GT_REG_CONFIG_DATA >> 8),
                    static_cast<uint8_t>(GT_REG_CONFIG_DATA & 0xff)})
      .ExpectReadStop({0x60})
      .ExpectWrite({static_cast<uint8_t>(GT_REG_FW_VERSION >> 8),
                    static_cast<uint8_t>(GT_REG_FW_VERSION & 0xff)})
      .ExpectReadStop({0x05, 0x61});

  InitDevice();
}

TEST_F(Gt92xxTest, TestReport) {
  mock_i2c_
      .ExpectWrite({static_cast<uint8_t>(GT_REG_TOUCH_STATUS >> 8),
                    static_cast<uint8_t>(GT_REG_TOUCH_STATUS & 0xff)})
      .ExpectReadStop({0x85})
      .ExpectWrite(
          {static_cast<uint8_t>(GT_REG_REPORTS >> 8), static_cast<uint8_t>(GT_REG_REPORTS & 0xff)})
      .ExpectReadStop({0x00, 0x00, 0x01, 0x10, 0x01, 0x01, 0x01, 0x00, 0x01, 0x00,
                       0x02, 0x20, 0x02, 0x01, 0x01, 0x00, 0x02, 0x00, 0x03, 0x30,
                       0x03, 0x01, 0x01, 0x00, 0x03, 0x00, 0x04, 0x40, 0x04, 0x01,
                       0x01, 0x00, 0x04, 0x00, 0x05, 0x50, 0x05, 0x01, 0x01, 0x00})
      .ExpectWriteStop({static_cast<uint8_t>(GT_REG_TOUCH_STATUS >> 8),
                        static_cast<uint8_t>(GT_REG_TOUCH_STATUS & 0xff), 0x00});
  EXPECT_OK(device_->StartThread());
  zx_nanosleep(zx_deadline_after(ZX_MSEC(10)));

  hidbus_ifc_protocol_ops_t ops = {};
  ops.io_queue = rpt_handler;

  hidbus_ifc_protocol_t protocol = {};
  protocol.ops = &ops;
  device_->HidbusStart(&protocol);
  zx_nanosleep(zx_deadline_after(ZX_MSEC(10)));
  device_->Trigger();
  while (!rpt_ran.load()) {
    zx_nanosleep(zx_deadline_after(ZX_MSEC(10)));
  }
  EXPECT_OK(device_->StopThread());
}

}  // namespace goodix
