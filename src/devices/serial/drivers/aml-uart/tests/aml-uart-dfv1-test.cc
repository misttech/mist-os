// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/serial/drivers/aml-uart/aml-uart-dfv1.h"

#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>

#include <bind/fuchsia/broadcom/platform/cpp/bind.h>
#include <zxtest/zxtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/serial/drivers/aml-uart/tests/device_state.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
};

class AmlUartHarness : public zxtest::Test {
 public:
  void SetUp() override {
    static constexpr fuchsia_hardware_serial::wire::SerialPortInfo kSerialInfo = {
        .serial_class = fuchsia_hardware_serial::Class::kBluetoothHci,
        .serial_vid = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM,
        .serial_pid = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458,
    };

    fake_pdev::FakePDevFidl::Config config;
    config.irqs[0] = {};
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &config.irqs[0]));
    state_.set_irq_signaller(config.irqs[0].borrow());

    auto pdev = fidl::Endpoints<fuchsia_hardware_platform_device::Device>::Create();
    ASSERT_OK(incoming_loop_.StartThread("incoming-ns-thread"));
    incoming_.SyncCall([config = std::move(config),
                        server = std::move(pdev.server)](IncomingNamespace* infra) mutable {
      infra->pdev_server.SetConfig(std::move(config));
      infra->pdev_server.Connect(std::move(server));
    });
    ASSERT_NO_FATAL_FAILURE();

    auto uart = std::make_unique<serial::AmlUartV1>(fake_parent_.get());
    zx_status_t status =
        uart->Init(ddk::PDevFidl(std::move(pdev.client)), kSerialInfo, state_.GetMmio());
    ASSERT_OK(status);
    device_ = uart.get();
    // The AmlUart* is now owned by the fake_ddk.
    uart.release();
  }

  void TearDown() override {
    device_async_remove(device_->zxdev());
    ASSERT_OK(mock_ddk::ReleaseFlaggedDevices(fake_parent_.get()));
  }

  serial::AmlUart& Device() { return device_->aml_uart_for_testing(); }

  DeviceState& device_state() { return state_; }

  auto& Runtime() { return *runtime_; }

  fdf::WireClient<fuchsia_hardware_serialimpl::Device> CreateClient() {
    auto [client, server] = fdf::Endpoints<fuchsia_hardware_serialimpl::Device>::Create();
    binding_.emplace(fdf::Dispatcher::GetCurrent()->get(), std::move(server),
                     &device_->aml_uart_for_testing(), fidl::kIgnoreBindingClosure);
    return fdf::WireClient(std::move(client), fdf::Dispatcher::GetCurrent()->get());
  }

 private:
  DeviceState state_;  // Must not be destructed before fake_parent_.
  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();
  std::shared_ptr<fdf_testing::DriverRuntime> runtime_ = mock_ddk::GetDriverRuntime();
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  serial::AmlUartV1* device_;
  std::optional<fdf::ServerBinding<fuchsia_hardware_serialimpl::Device>> binding_;
};

TEST_F(AmlUartHarness, SerialImplAsyncGetInfo) {
  auto client = CreateClient();

  fdf::Arena arena('TEST');
  client.buffer(arena)->GetInfo().ThenExactlyOnce([](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());

    const auto& info = result->value()->info;
    ASSERT_EQ(info.serial_class, fuchsia_hardware_serial::Class::kBluetoothHci);
    ASSERT_EQ(info.serial_pid, bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458);
    ASSERT_EQ(info.serial_vid, bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM);
  });

  Runtime().RunUntilIdle();
}

TEST_F(AmlUartHarness, SerialImplAsyncConfig) {
  auto client = CreateClient();

  fdf::Arena arena('TEST');

  client.buffer(arena)->Enable(false).ThenExactlyOnce([](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  Runtime().RunUntilIdle();

  ASSERT_EQ(device_state().Control().tx_enable(), 0);
  ASSERT_EQ(device_state().Control().rx_enable(), 0);
  ASSERT_EQ(device_state().Control().inv_cts(), 0);

  static constexpr uint32_t serial_test_config = fuchsia_hardware_serialimpl::kSerialDataBits6 |
                                                 fuchsia_hardware_serialimpl::kSerialStopBits2 |
                                                 fuchsia_hardware_serialimpl::kSerialParityEven |
                                                 fuchsia_hardware_serialimpl::kSerialFlowCtrlCtsRts;
  client.buffer(arena)->Config(20, serial_test_config).ThenExactlyOnce([](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  Runtime().RunUntilIdle();

  ASSERT_EQ(device_state().DataBits(), fuchsia_hardware_serialimpl::kSerialDataBits6);
  ASSERT_EQ(device_state().StopBits(), fuchsia_hardware_serialimpl::kSerialStopBits2);
  ASSERT_EQ(device_state().Parity(), fuchsia_hardware_serialimpl::kSerialParityEven);
  ASSERT_TRUE(device_state().FlowControl());

  client.buffer(arena)
      ->Config(40, fuchsia_hardware_serialimpl::kSerialSetBaudRateOnly)
      .ThenExactlyOnce([](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
      });
  Runtime().RunUntilIdle();

  ASSERT_EQ(device_state().DataBits(), fuchsia_hardware_serialimpl::kSerialDataBits6);
  ASSERT_EQ(device_state().StopBits(), fuchsia_hardware_serialimpl::kSerialStopBits2);
  ASSERT_EQ(device_state().Parity(), fuchsia_hardware_serialimpl::kSerialParityEven);
  ASSERT_TRUE(device_state().FlowControl());

  client.buffer(arena)->Config(0, serial_test_config).ThenExactlyOnce([](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  });

  client.buffer(arena)->Config(UINT32_MAX, serial_test_config).ThenExactlyOnce([](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  });

  client.buffer(arena)->Config(1, serial_test_config).ThenExactlyOnce([](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  });
  Runtime().RunUntilIdle();

  ASSERT_EQ(device_state().DataBits(), fuchsia_hardware_serialimpl::kSerialDataBits6);
  ASSERT_EQ(device_state().StopBits(), fuchsia_hardware_serialimpl::kSerialStopBits2);
  ASSERT_EQ(device_state().Parity(), fuchsia_hardware_serialimpl::kSerialParityEven);
  ASSERT_TRUE(device_state().FlowControl());

  client.buffer(arena)
      ->Config(40, fuchsia_hardware_serialimpl::kSerialSetBaudRateOnly)
      .ThenExactlyOnce([](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
      });
  Runtime().RunUntilIdle();

  ASSERT_EQ(device_state().DataBits(), fuchsia_hardware_serialimpl::kSerialDataBits6);
  ASSERT_EQ(device_state().StopBits(), fuchsia_hardware_serialimpl::kSerialStopBits2);
  ASSERT_EQ(device_state().Parity(), fuchsia_hardware_serialimpl::kSerialParityEven);
  ASSERT_TRUE(device_state().FlowControl());
}

TEST_F(AmlUartHarness, SerialImplAsyncEnable) {
  auto client = CreateClient();

  fdf::Arena arena('TEST');

  client.buffer(arena)->Enable(false).ThenExactlyOnce([](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  Runtime().RunUntilIdle();

  ASSERT_EQ(device_state().Control().tx_enable(), 0);
  ASSERT_EQ(device_state().Control().rx_enable(), 0);
  ASSERT_EQ(device_state().Control().inv_cts(), 0);

  client.buffer(arena)->Enable(true).ThenExactlyOnce([](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  Runtime().RunUntilIdle();

  ASSERT_EQ(device_state().Control().tx_enable(), 1);
  ASSERT_EQ(device_state().Control().rx_enable(), 1);
  ASSERT_EQ(device_state().Control().inv_cts(), 0);
  ASSERT_TRUE(device_state().PortResetRX());
  ASSERT_TRUE(device_state().PortResetTX());
  ASSERT_FALSE(device_state().Control().rst_rx());
  ASSERT_FALSE(device_state().Control().rst_tx());
  ASSERT_TRUE(device_state().Control().tx_interrupt_enable());
  ASSERT_TRUE(device_state().Control().rx_interrupt_enable());
}

TEST_F(AmlUartHarness, SerialImplReadAsync) {
  auto client = CreateClient();

  fdf::Arena arena('TEST');

  client.buffer(arena)->Enable(true).ThenExactlyOnce([](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });

  std::vector<uint8_t> expected_data;
  for (size_t i = 0; i < kDataLen; i++) {
    expected_data.push_back(static_cast<uint8_t>(i));
  }

  client.buffer(arena)->Read().ThenExactlyOnce([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    const std::vector actual_data(result->value()->data.cbegin(), result->value()->data.cend());
    EXPECT_EQ(expected_data, actual_data);
    Runtime().Quit();
  });
  // Let the driver run and enqueue the read. It won't be completed until we inject an interrupt.
  Runtime().RunUntilIdle();

  device_state().Inject(expected_data.data(), kDataLen);

  // Wait for the interrupt dispatcher to read out the data and complete the read.
  Runtime().Run();
}

TEST_F(AmlUartHarness, SerialImplWriteAsync) {
  auto client = CreateClient();

  fdf::Arena arena('TEST');

  client.buffer(arena)->Enable(true).ThenExactlyOnce([](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });

  std::vector<uint8_t> expected_data;
  for (size_t i = 0; i < kDataLen; i++) {
    expected_data.push_back(static_cast<uint8_t>(i));
  }

  client.buffer(arena)
      ->Write(fidl::VectorView<uint8_t>::FromExternal(expected_data.data(), kDataLen))
      .ThenExactlyOnce([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        Runtime().Quit();
      });

  Runtime().Run();

  EXPECT_EQ(expected_data, device_state().TxBuf());
}

TEST_F(AmlUartHarness, SerialImplAsyncWriteDoubleCallback) {
  // NOTE: we don't start the IRQ thread.  The Handle*RaceForTest() enable.
  auto client = CreateClient();

  fdf::Arena arena('TEST');

  std::vector<uint8_t> expected_data;
  for (size_t i = 0; i < kDataLen; i++) {
    expected_data.push_back(static_cast<uint8_t>(i));
  }

  bool write_complete = false;
  client.buffer(arena)
      ->Write(fidl::VectorView<uint8_t>::FromExternal(expected_data.data(), kDataLen))
      .ThenExactlyOnce([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        write_complete = true;
      });
  Runtime().RunUntilIdle();
  Device().HandleTXRaceForTest();
  Runtime().RunUntil([&]() { return write_complete; });

  EXPECT_EQ(expected_data, device_state().TxBuf());
}

TEST_F(AmlUartHarness, SerialImplAsyncReadDoubleCallback) {
  // NOTE: we don't start the IRQ thread.  The Handle*RaceForTest() enable.
  auto client = CreateClient();

  fdf::Arena arena('TEST');

  std::vector<uint8_t> expected_data;
  for (size_t i = 0; i < kDataLen; i++) {
    expected_data.push_back(static_cast<uint8_t>(i));
  }

  client.buffer(arena)->Read().ThenExactlyOnce([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    const std::vector actual_data(result->value()->data.cbegin(), result->value()->data.cend());
    EXPECT_EQ(expected_data, actual_data);
    Runtime().Quit();
  });
  Runtime().RunUntilIdle();

  device_state().Inject(expected_data.data(), kDataLen);
  Device().HandleRXRaceForTest();
  Runtime().Run();
}
