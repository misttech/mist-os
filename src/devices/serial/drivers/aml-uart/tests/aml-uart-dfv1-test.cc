// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/serial/drivers/aml-uart/aml-uart-dfv1.h"

#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>

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
    static constexpr serial_port_info_t kSerialInfo = {
        .serial_class = fidl::ToUnderlying(fuchsia_hardware_serial::Class::kBluetoothHci),
        .serial_vid = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM,
        .serial_pid = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458,
    };
    fake_parent_->SetMetadata(DEVICE_METADATA_SERIAL_PORT_INFO, &kSerialInfo, sizeof(kSerialInfo));

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

 private:
  DeviceState state_;  // Must not be destructed before fake_parent_.
  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  serial::AmlUartV1* device_;
};

TEST_F(AmlUartHarness, SerialImplAsyncGetInfo) {
  serial_port_info_t info;
  ASSERT_OK(Device().SerialImplAsyncGetInfo(&info));
  ASSERT_EQ(info.serial_class, fidl::ToUnderlying(fuchsia_hardware_serial::Class::kBluetoothHci));
  ASSERT_EQ(info.serial_pid, bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458);
  ASSERT_EQ(info.serial_vid, bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM);
}

TEST_F(AmlUartHarness, SerialImplAsyncConfig) {
  ASSERT_OK(Device().SerialImplAsyncEnable(false));
  ASSERT_EQ(device_state().Control().tx_enable(), 0);
  ASSERT_EQ(device_state().Control().rx_enable(), 0);
  ASSERT_EQ(device_state().Control().inv_cts(), 0);
  static constexpr uint32_t serial_test_config =
      SERIAL_DATA_BITS_6 | SERIAL_STOP_BITS_2 | SERIAL_PARITY_EVEN | SERIAL_FLOW_CTRL_CTS_RTS;
  ASSERT_OK(Device().SerialImplAsyncConfig(20, serial_test_config));
  ASSERT_EQ(device_state().DataBits(), SERIAL_DATA_BITS_6);
  ASSERT_EQ(device_state().StopBits(), SERIAL_STOP_BITS_2);
  ASSERT_EQ(device_state().Parity(), SERIAL_PARITY_EVEN);
  ASSERT_TRUE(device_state().FlowControl());
  ASSERT_OK(Device().SerialImplAsyncConfig(40, SERIAL_SET_BAUD_RATE_ONLY));
  ASSERT_EQ(device_state().DataBits(), SERIAL_DATA_BITS_6);
  ASSERT_EQ(device_state().StopBits(), SERIAL_STOP_BITS_2);
  ASSERT_EQ(device_state().Parity(), SERIAL_PARITY_EVEN);
  ASSERT_TRUE(device_state().FlowControl());

  ASSERT_NOT_OK(Device().SerialImplAsyncConfig(0, serial_test_config));
  ASSERT_NOT_OK(Device().SerialImplAsyncConfig(UINT32_MAX, serial_test_config));
  ASSERT_NOT_OK(Device().SerialImplAsyncConfig(1, serial_test_config));
  ASSERT_EQ(device_state().DataBits(), SERIAL_DATA_BITS_6);
  ASSERT_EQ(device_state().StopBits(), SERIAL_STOP_BITS_2);
  ASSERT_EQ(device_state().Parity(), SERIAL_PARITY_EVEN);
  ASSERT_TRUE(device_state().FlowControl());
  ASSERT_OK(Device().SerialImplAsyncConfig(40, SERIAL_SET_BAUD_RATE_ONLY));
  ASSERT_EQ(device_state().DataBits(), SERIAL_DATA_BITS_6);
  ASSERT_EQ(device_state().StopBits(), SERIAL_STOP_BITS_2);
  ASSERT_EQ(device_state().Parity(), SERIAL_PARITY_EVEN);
  ASSERT_TRUE(device_state().FlowControl());
}

TEST_F(AmlUartHarness, SerialImplAsyncEnable) {
  ASSERT_OK(Device().SerialImplAsyncEnable(false));
  ASSERT_EQ(device_state().Control().tx_enable(), 0);
  ASSERT_EQ(device_state().Control().rx_enable(), 0);
  ASSERT_EQ(device_state().Control().inv_cts(), 0);
  ASSERT_OK(Device().SerialImplAsyncEnable(true));
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
  ASSERT_OK(Device().SerialImplAsyncEnable(true));
  struct Context {
    uint8_t data[kDataLen];
    sync_completion_t completion;
  } context;
  for (size_t i = 0; i < kDataLen; i++) {
    context.data[i] = static_cast<uint8_t>(i);
  }
  auto cb = [](void* ctx, zx_status_t status, const uint8_t* buffer, size_t bufsz) {
    auto context = static_cast<Context*>(ctx);
    EXPECT_EQ(bufsz, kDataLen);
    EXPECT_EQ(memcmp(buffer, context->data, bufsz), 0);
    sync_completion_signal(&context->completion);
  };
  Device().SerialImplAsyncReadAsync(cb, &context);
  device_state().Inject(context.data, kDataLen);
  sync_completion_wait(&context.completion, ZX_TIME_INFINITE);
}

TEST_F(AmlUartHarness, SerialImplWriteAsync) {
  ASSERT_OK(Device().SerialImplAsyncEnable(true));
  struct Context {
    uint8_t data[kDataLen];
    sync_completion_t completion;
  } context;
  for (size_t i = 0; i < kDataLen; i++) {
    context.data[i] = static_cast<uint8_t>(i);
  }
  auto cb = [](void* ctx, zx_status_t status) {
    auto context = static_cast<Context*>(ctx);
    sync_completion_signal(&context->completion);
  };
  Device().SerialImplAsyncWriteAsync(context.data, kDataLen, cb, &context);
  sync_completion_wait(&context.completion, ZX_TIME_INFINITE);
  auto buf = device_state().TxBuf();
  ASSERT_EQ(buf.size(), kDataLen);
  ASSERT_EQ(memcmp(buf.data(), context.data, buf.size()), 0);
}

TEST_F(AmlUartHarness, SerialImplAsyncWriteDoubleCallback) {
  // NOTE: we don't start the IRQ thread.  The Handle*RaceForTest() enable.
  struct Context {
    uint8_t data[kDataLen];
    sync_completion_t completion;
  } context;
  for (size_t i = 0; i < kDataLen; i++) {
    context.data[i] = static_cast<uint8_t>(i);
  }
  auto cb = [](void* ctx, zx_status_t status) {
    auto context = static_cast<Context*>(ctx);
    sync_completion_signal(&context->completion);
  };
  Device().SerialImplAsyncWriteAsync(context.data, kDataLen, cb, &context);
  Device().HandleTXRaceForTest();
  sync_completion_wait(&context.completion, ZX_TIME_INFINITE);
  auto buf = device_state().TxBuf();
  ASSERT_EQ(buf.size(), kDataLen);
  ASSERT_EQ(memcmp(buf.data(), context.data, buf.size()), 0);
}

TEST_F(AmlUartHarness, SerialImplAsyncReadDoubleCallback) {
  // NOTE: we don't start the IRQ thread.  The Handle*RaceForTest() enable.
  struct Context {
    uint8_t data[kDataLen];
    sync_completion_t completion;
  } context;
  for (size_t i = 0; i < kDataLen; i++) {
    context.data[i] = static_cast<uint8_t>(i);
  }
  auto cb = [](void* ctx, zx_status_t status, const uint8_t* buffer, size_t bufsz) {
    auto context = static_cast<Context*>(ctx);
    EXPECT_EQ(bufsz, kDataLen);
    EXPECT_EQ(memcmp(buffer, context->data, bufsz), 0);
    sync_completion_signal(&context->completion);
  };
  Device().SerialImplAsyncReadAsync(cb, &context);
  device_state().Inject(context.data, kDataLen);
  Device().HandleRXRaceForTest();
  sync_completion_wait(&context.completion, ZX_TIME_INFINITE);
}
