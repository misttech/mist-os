
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "cy8cmbr3108.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/metadata.h>
#include <lib/fake-hidbus-ifc/fake-hidbus-ifc.h>
#include <lib/mock-i2c/mock-i2c.h>

#include <hid/visalia-touch.h>
#include <zxtest/zxtest.h>

#include "src/devices/gpio/testing/fake-gpio/fake-gpio.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "zxtest/base/test.h"

namespace cypress {

class Cy8cmbr3108TestDevice : public Cy8cmbr3108 {
 public:
  explicit Cy8cmbr3108TestDevice(zx_device_t* parent) : Cy8cmbr3108(parent) {}

  zx_status_t Init() { return Cy8cmbr3108::Init(); }
};

class Cy8cmbr3108Test : public zxtest::Test {
 public:
  void SetUp() override {
    constexpr touch_button_config_t kMetadata[] = {
        {
            .id = BUTTONS_ID_VOLUME_UP,
            .idx = 4,
        },
        {
            .id = BUTTONS_ID_VOLUME_DOWN,
            .idx = 5,
        },
        {
            .id = BUTTONS_ID_PLAY_PAUSE,
            .idx = 0,
        },
    };
    fake_parent_ = MockDevice::FakeRootParent();
    fake_parent_->SetMetadata(DEVICE_METADATA_PRIVATE, &kMetadata, sizeof(kMetadata));
    EXPECT_OK(outgoing_loop_.StartThread("outgoing"));

    // Create i2c fragment.
    auto i2c_handler = fuchsia_hardware_i2c::Service::InstanceHandler(
        {.device =
             mock_i2c_.SyncCall(&mock_i2c::MockI2c::bind_handler, async_patterns::PassDispatcher)});
    auto service_result = outgoing_.SyncCall(
        [handler = std::move(i2c_handler)](component::OutgoingDirectory* outgoing) mutable {
          return outgoing->AddService<fuchsia_hardware_i2c::Service>(std::move(handler));
        });
    ZX_ASSERT(service_result.is_ok());
    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ZX_ASSERT(endpoints.is_ok());
    ZX_ASSERT(outgoing_.SyncCall(&component::OutgoingDirectory::Serve, std::move(endpoints->server))
                  .is_ok());
    fake_parent_->AddFidlService(fuchsia_hardware_i2c::Service::Name, std::move(endpoints->client),
                                 "i2c");

    // Create gpio fragment.
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &mock_irq_));
    zx::interrupt gpio_interrupt;
    ASSERT_OK(mock_irq_.duplicate(ZX_RIGHT_SAME_RIGHTS, &gpio_interrupt));
    mock_touch_gpio_.SyncCall(&fake_gpio::FakeGpio::SetInterrupt,
                              zx::ok(std::move(gpio_interrupt)));
    auto gpio_handler = mock_touch_gpio_.SyncCall(&fake_gpio::FakeGpio::CreateInstanceHandler);
    service_result = outgoing_.SyncCall(
        [handler = std::move(gpio_handler)](component::OutgoingDirectory* outgoing) mutable {
          return outgoing->AddService<fuchsia_hardware_gpio::Service>(std::move(handler));
        });
    ZX_ASSERT(service_result.is_ok());
    endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ZX_ASSERT(endpoints.is_ok());
    ZX_ASSERT(outgoing_.SyncCall(&component::OutgoingDirectory::Serve, std::move(endpoints->server))
                  .is_ok());
    fake_parent_->AddFidlService(fuchsia_hardware_gpio::Service::Name, std::move(endpoints->client),
                                 "gpio");

    dut_.emplace(fake_parent_.get());

    EXPECT_OK(dut_->Init());
    std::vector states = mock_touch_gpio_.SyncCall(&fake_gpio::FakeGpio::GetStateLog);
    EXPECT_EQ(2, states.size());
    EXPECT_EQ(fake_gpio::AltFunctionState{.function = 0}, states[0]);
    EXPECT_EQ(fake_gpio::ReadState{.flags = fuchsia_hardware_gpio::GpioFlags::kNoPull}, states[1]);
  }

  void TearDown() override {
    dut_.value().ShutDown();
    mock_i2c_.SyncCall(&mock_i2c::MockI2c::VerifyAndClear);
  }

  void FakeInterrupt() { mock_irq_.trigger(0, zx::time()); }

  Cy8cmbr3108TestDevice& dut() { return dut_.value(); }

  async_patterns::TestDispatcherBound<mock_i2c::MockI2c>& mock_i2c() { return mock_i2c_; }

 private:
  async::Loop outgoing_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<component::OutgoingDirectory> outgoing_{
      outgoing_loop_.dispatcher(), std::in_place, async_patterns::PassDispatcher};
  async_patterns::TestDispatcherBound<fake_gpio::FakeGpio> mock_touch_gpio_{
      outgoing_loop_.dispatcher(), std::in_place};
  async_patterns::TestDispatcherBound<mock_i2c::MockI2c> mock_i2c_{outgoing_loop_.dispatcher(),
                                                                   std::in_place};
  std::optional<Cy8cmbr3108TestDevice> dut_;
  zx::interrupt mock_irq_;
  std::shared_ptr<MockDevice> fake_parent_;
};

TEST_F(Cy8cmbr3108Test, Init) {}

TEST_F(Cy8cmbr3108Test, ButtonTouched) {
  visalia_touch_buttons_input_rpt_t expected_rpt = {};
  expected_rpt.rpt_id = BUTTONS_RPT_ID_INPUT;
  expected_rpt.volume_up = 1;

  fake_hidbus_ifc::FakeHidbusIfc fake_hid_bus;
  dut().HidbusStart(fake_hid_bus.GetProto());

  mock_i2c().SyncCall([](mock_i2c::MockI2c* i2c) {
    i2c->ExpectWrite({0xAA}).ExpectReadStop({0x10, 0x00});
  });
  FakeInterrupt();

  std::vector<uint8_t> returned_rpt;
  ASSERT_OK(fake_hid_bus.WaitUntilNextReport(&returned_rpt));
  EXPECT_EQ(returned_rpt.size(), sizeof(expected_rpt));
  ASSERT_BYTES_EQ(returned_rpt.data(), &expected_rpt, returned_rpt.size());
}

TEST_F(Cy8cmbr3108Test, ButtonReleased) {
  visalia_touch_buttons_input_rpt_t expected_rpt = {};
  expected_rpt.rpt_id = BUTTONS_RPT_ID_INPUT;

  fake_hidbus_ifc::FakeHidbusIfc fake_hid_bus;
  dut().HidbusStart(fake_hid_bus.GetProto());

  mock_i2c().SyncCall([](mock_i2c::MockI2c* i2c) {
    i2c->ExpectWrite({0xAA}).ExpectReadStop({0x00, 0x00});
  });
  FakeInterrupt();

  std::vector<uint8_t> returned_rpt;
  ASSERT_OK(fake_hid_bus.WaitUntilNextReport(&returned_rpt));
  EXPECT_EQ(returned_rpt.size(), sizeof(expected_rpt));
  ASSERT_BYTES_EQ(returned_rpt.data(), &expected_rpt, returned_rpt.size());
}

TEST_F(Cy8cmbr3108Test, MultipleButtonTouch) {
  visalia_touch_buttons_input_rpt_t expected_rpt = {};
  expected_rpt.rpt_id = BUTTONS_RPT_ID_INPUT;
  expected_rpt.volume_down = 1;
  expected_rpt.pause = 1;

  fake_hidbus_ifc::FakeHidbusIfc fake_hid_bus;
  dut().HidbusStart(fake_hid_bus.GetProto());

  mock_i2c().SyncCall([](mock_i2c::MockI2c* i2c) {
    i2c->ExpectWrite({0xAA}).ExpectReadStop({0x21, 0x00});
  });
  FakeInterrupt();

  std::vector<uint8_t> returned_rpt;
  ASSERT_OK(fake_hid_bus.WaitUntilNextReport(&returned_rpt));
  EXPECT_EQ(returned_rpt.size(), sizeof(expected_rpt));
  ASSERT_BYTES_EQ(returned_rpt.data(), &expected_rpt, returned_rpt.size());
}

}  // namespace cypress
