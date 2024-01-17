// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/adc/drivers/aml-saradc/aml-saradc.h"

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <zxtest/zxtest.h>

#include "src/devices/adc/drivers/aml-saradc/registers.h"
#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"

namespace {

constexpr auto kRegisterBanks = 2;
constexpr auto kRegisterCount = 2048;

class FakeMmio {
 public:
  FakeMmio() : region_(sizeof(uint32_t), kRegisterCount) {
    for (size_t c = 0; c < kRegisterCount; c++) {
      region_[c * sizeof(uint32_t)].SetReadCallback(
          [this, c]() { return reg_values_.find(c) == reg_values_.end() ? 0 : reg_values_.at(c); });
      region_[c * sizeof(uint32_t)].SetWriteCallback(
          [this, c](uint64_t value) { reg_values_[c] = value; });
    }
  }

  fdf::MmioBuffer mmio() { return region_.GetMmioBuffer(); }

  void set(size_t offset, uint64_t value) { reg_values_[offset] = value; }

 private:
  ddk_fake::FakeMmioRegRegion region_;
  std::map<size_t, uint64_t> reg_values_;
};

struct IncomingNamespace {
  fdf_testing::TestNode node_{std::string("root")};
  fdf_testing::TestEnvironment env_{fdf::Dispatcher::GetCurrent()->get()};
  compat::DeviceServer device_server_;
  fake_pdev::FakePDevFidl pdev_server_;
};

class AmlSaradcTest : public zxtest::Test {
 public:
  void SetUp() override {
    static constexpr uint8_t kAdcChannels[] = {};

    fake_pdev::FakePDevFidl::Config config;
    config.irqs[0] = {};
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &config.irqs[0]));
    config.mmios[0] = mmio_[0].mmio();
    config.mmios[1] = mmio_[1].mmio();
    irq_ = config.irqs[0].borrow();

    fuchsia_driver_framework::DriverStartArgs start_args;
    fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory_client;
    incoming_.SyncCall([&](IncomingNamespace* incoming) {
      auto start_args_result = incoming->node_.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());
      start_args = std::move(start_args_result->start_args);
      outgoing_directory_client = std::move(start_args_result->outgoing_directory_client);

      auto init_result =
          incoming->env_.Initialize(std::move(start_args_result->incoming_directory_server));
      ASSERT_TRUE(init_result.is_ok());

      incoming->device_server_.Init(component::kDefaultInstance, "");

      // Serve metadata.
      auto status = incoming->device_server_.AddMetadata(DEVICE_METADATA_ADC, &kAdcChannels,
                                                         sizeof(kAdcChannels));
      EXPECT_OK(status);
      status = incoming->device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                              &incoming->env_.incoming_directory());
      EXPECT_OK(status);

      // Serve pdev_server.
      incoming->pdev_server_.SetConfig(std::move(config));
      auto result =
          incoming->env_.incoming_directory().AddService<fuchsia_hardware_platform_device::Service>(
              std::move(incoming->pdev_server_.GetInstanceHandler(
                  fdf::Dispatcher::GetCurrent()->async_dispatcher())));
      ASSERT_TRUE(result.is_ok());
    });

    // Start dut_.
    auto result = runtime_.RunToCompletion(dut_.SyncCall(
        &fdf_testing::DriverUnderTest<aml_saradc::AmlSaradc>::Start, std::move(start_args)));
    ASSERT_TRUE(result.is_ok());

    // Connect to AdcImpl.
    auto svc_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    EXPECT_EQ(ZX_OK, svc_endpoints.status_value());

    zx_status_t status = fdio_open_at(outgoing_directory_client.handle()->get(), "/svc",
                                      static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                                      svc_endpoints->server.TakeChannel().release());
    EXPECT_EQ(ZX_OK, status);

    auto connect_result =
        fdf::internal::DriverTransportConnect<fuchsia_hardware_adcimpl::Service::Device>(
            svc_endpoints->client, component::kDefaultInstance);
    ASSERT_TRUE(connect_result.is_ok());
    adc_.Bind(std::move(connect_result.value()));
    ASSERT_TRUE(adc_.is_valid());
  }

  void TearDown() override {
    // Stop dut_.
    auto result = runtime_.RunToCompletion(
        dut_.SyncCall(&fdf_testing::DriverUnderTest<aml_saradc::AmlSaradc>::PrepareStop));
    ASSERT_TRUE(result.is_ok());
  }

  FakeMmio& mmio(size_t i) { return mmio_[i]; }

 private:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_ = runtime_.StartBackgroundDispatcher();
  FakeMmio mmio_[kRegisterBanks];
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{
      env_dispatcher_->async_dispatcher(), std::in_place};

  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<aml_saradc::AmlSaradc>> dut_{
      driver_dispatcher_->async_dispatcher(), std::in_place};

 protected:
  fdf::WireSyncClient<fuchsia_hardware_adcimpl::Device> adc_;
  zx::unowned_interrupt irq_;
};

TEST_F(AmlSaradcTest, GetResolution) {
  fdf::Arena arena('TEST');
  auto result = adc_.buffer(arena)->GetResolution();
  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_ok());
  EXPECT_EQ(result.value()->resolution, 10);
}

TEST_F(AmlSaradcTest, GetSample) {
  mmio(0).set(AO_SAR_ADC_FIFO_RD_OFFS >> 2, 0x4);
  irq_->trigger(0, zx::clock::get_monotonic());

  fdf::Arena arena('TEST');
  auto result = adc_.buffer(arena)->GetSample(0);
  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_ok());
  EXPECT_EQ(result.value()->value, 1);
}

TEST_F(AmlSaradcTest, GetSampleInvalidArgs) {
  fdf::Arena arena('TEST');
  auto result = adc_.buffer(arena)->GetSample(8);
  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_error());
  EXPECT_EQ(result->error_value(), ZX_ERR_INVALID_ARGS);
}

}  // namespace
