// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/serial/drivers/aml-uart/aml-uart-dfv2.h"

#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <bind/fuchsia/broadcom/platform/cpp/bind.h>
#include <zxtest/zxtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/serial/drivers/aml-uart/tests/device_state.h"

static constexpr serial_port_info_t kSerialInfo = {
    .serial_class = fidl::ToUnderlying(fuchsia_hardware_serial::Class::kBluetoothHci),
    .serial_vid = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM,
    .serial_pid = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458,
};

class Environment {
 public:
  fdf::DriverStartArgs InitEnvironment(fake_pdev::FakePDevFidl::Config pdev_config) {
    zx::result start_args_result = test_node_.CreateStartArgsAndServe();
    ZX_ASSERT(start_args_result.is_ok());

    zx::result init_result =
        test_environment_.Initialize(std::move(start_args_result->incoming_directory_server));
    ZX_ASSERT(init_result.is_ok());

    pdev_server_.SetConfig(std::move(pdev_config));

    async_dispatcher_t* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
    std::string instance_name = "pdev";

    zx::result add_service_result =
        test_environment_.incoming_directory()
            .AddService<fuchsia_hardware_platform_device::Service>(
                pdev_server_.GetInstanceHandler(dispatcher), instance_name);
    ZX_ASSERT(add_service_result.is_ok());

    compat_server_.Init("default", "topo");
    compat_server_.AddMetadata(DEVICE_METADATA_SERIAL_PORT_INFO, &kSerialInfo, sizeof(kSerialInfo));
    zx_status_t status = compat_server_.Serve(dispatcher, &test_environment_.incoming_directory());
    ZX_ASSERT(status == ZX_OK);

    outgoing_client_ = std::move(start_args_result->outgoing_directory_client);

    return std::move(start_args_result.value().start_args);
  }

  fidl::ClientEnd<fuchsia_io::Directory> TakeOutgoingClient() {
    return std::move(outgoing_client_);
  }

 private:
  fdf_testing::TestNode test_node_{"root"};
  fake_pdev::FakePDevFidl pdev_server_;
  fdf_testing::TestEnvironment test_environment_;
  compat::DeviceServer compat_server_;
  fidl::ClientEnd<fuchsia_io::Directory> outgoing_client_;
};

class AmlUartHarness : public zxtest::Test {
 public:
  void SetUp() override {
    fake_pdev::FakePDevFidl::Config config;
    config.irqs[0] = {};
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &config.irqs[0]));
    state_.set_irq_signaller(config.irqs[0].borrow());
    config.mmios[0] = state_.GetMmio();

    fdf::DriverStartArgs start_args =
        env_.SyncCall(&Environment::InitEnvironment, std::move(config));

    zx::result run_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args)));
    ZX_ASSERT(run_result.is_ok());
  }

  void TearDown() override {
    zx::result run_result = runtime_.RunToCompletion(dut_.PrepareStop());
    ZX_ASSERT(run_result.is_ok());
  }

  serial::AmlUart& Device() { return dut_->aml_uart_for_testing(); }

  fidl::ClientEnd<fuchsia_io::Directory> CreateDriverSvcClient() {
    auto driver_outgoing = env_.SyncCall(&Environment::TakeOutgoingClient);
    // Open the svc directory in the driver's outgoing, and store a client to it.
    auto svc_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    EXPECT_EQ(ZX_OK, svc_endpoints.status_value());
    zx_status_t status = fdio_open_at(driver_outgoing.handle()->get(), "/svc",
                                      static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                                      svc_endpoints->server.TakeChannel().release());
    EXPECT_EQ(ZX_OK, status);
    return std::move(svc_endpoints->client);
  }

  DeviceState& device_state() { return state_; }

  async_dispatcher_t* env_dispatcher() { return env_dispatcher_->async_dispatcher(); }

  fdf_testing::DriverRuntime& runtime() { return runtime_; }

 private:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();
  async_patterns::TestDispatcherBound<Environment> env_{env_dispatcher(), std::in_place};

  DeviceState state_;
  fdf_testing::DriverUnderTest<serial::AmlUartV2> dut_;
};

TEST_F(AmlUartHarness, SerialImplAsyncGetInfo) {
  serial_port_info_t info;
  ASSERT_OK(Device().SerialImplAsyncGetInfo(&info));
  ASSERT_EQ(info.serial_class, fidl::ToUnderlying(fuchsia_hardware_serial::Class::kBluetoothHci));
  ASSERT_EQ(info.serial_pid, bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458);
  ASSERT_EQ(info.serial_vid, bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM);
}

TEST_F(AmlUartHarness, SerialImplAsyncGetInfoFromDriverService) {
  zx::result driver_connect_result =
      fdf::internal::DriverTransportConnect<fuchsia_hardware_serialimpl::Service::Device>(
          CreateDriverSvcClient(), "aml-uart");
  ASSERT_EQ(ZX_OK, driver_connect_result.status_value());
  fdf::Arena arena('INFO');
  fdf::WireClient<fuchsia_hardware_serialimpl::Device> device_client(
      std::move(driver_connect_result.value()), fdf::Dispatcher::GetCurrent()->get());
  device_client.buffer(arena)->GetInfo().Then(
      [quit = runtime().QuitClosure()](
          fdf::WireUnownedResult<fuchsia_hardware_serialimpl::Device::GetInfo>& result) {
        ASSERT_EQ(ZX_OK, result.status());
        ASSERT_TRUE(result.value().is_ok());

        auto res = result.value().value();
        ASSERT_EQ(res->info.serial_class, fuchsia_hardware_serial::Class::kBluetoothHci);
        ASSERT_EQ(res->info.serial_pid,
                  bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458);
        ASSERT_EQ(res->info.serial_vid,
                  bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM);
        quit();
      });
  runtime().Run();
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

TEST_F(AmlUartHarness, SerialImplReadDriverService) {
  uint8_t data[kDataLen];
  for (size_t i = 0; i < kDataLen; i++) {
    data[i] = static_cast<uint8_t>(i);
  }

  zx::result driver_connect_result =
      fdf::internal::DriverTransportConnect<fuchsia_hardware_serialimpl::Service::Device>(
          CreateDriverSvcClient(), "aml-uart");
  ASSERT_EQ(ZX_OK, driver_connect_result.status_value());
  fdf::Arena arena('READ');
  fdf::WireClient<fuchsia_hardware_serialimpl::Device> device_client(
      std::move(driver_connect_result.value()), fdf::Dispatcher::GetCurrent()->get());

  device_client.buffer(arena)->Enable(true).Then(
      [quit = runtime().QuitClosure()](auto& res) { quit(); });
  runtime().Run();

  device_client.buffer(arena)->Read().Then(
      [quit = runtime().QuitClosure(),
       data](fdf::WireUnownedResult<fuchsia_hardware_serialimpl::Device::Read>& result) {
        ASSERT_EQ(ZX_OK, result.status());
        ASSERT_TRUE(result.value().is_ok());

        auto res = result.value().value();
        EXPECT_EQ(res->data.count(), kDataLen);
        EXPECT_EQ(memcmp(data, res->data.data(), res->data.count()), 0);
        quit();
      });
  device_state().Inject(data, kDataLen);
  runtime().Run();
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

TEST_F(AmlUartHarness, SerialImplWriteDriverService) {
  uint8_t data[kDataLen];
  for (size_t i = 0; i < kDataLen; i++) {
    data[i] = static_cast<uint8_t>(i);
  }

  zx::result driver_connect_result =
      fdf::internal::DriverTransportConnect<fuchsia_hardware_serialimpl::Service::Device>(
          CreateDriverSvcClient(), "aml-uart");
  ASSERT_EQ(ZX_OK, driver_connect_result.status_value());
  fdf::Arena arena('WRIT');
  fdf::WireClient<fuchsia_hardware_serialimpl::Device> device_client(
      std::move(driver_connect_result.value()), fdf::Dispatcher::GetCurrent()->get());

  device_client.buffer(arena)->Enable(true).Then(
      [quit = runtime().QuitClosure()](auto& res) { quit(); });
  runtime().Run();

  device_client.buffer(arena)
      ->Write(fidl::VectorView<uint8_t>::FromExternal(data, kDataLen))
      .Then([quit = runtime().QuitClosure()](
                fdf::WireUnownedResult<fuchsia_hardware_serialimpl::Device::Write>& result) {
        ASSERT_EQ(ZX_OK, result.status());
        ASSERT_TRUE(result.value().is_ok());
        quit();
      });
  runtime().Run();
  auto buf = device_state().TxBuf();
  ASSERT_EQ(buf.size(), kDataLen);
  ASSERT_EQ(memcmp(buf.data(), data, buf.size()), 0);
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
