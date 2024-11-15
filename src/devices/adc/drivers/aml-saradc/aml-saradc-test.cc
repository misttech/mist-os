// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/adc/drivers/aml-saradc/aml-saradc.h"

#include <lib/driver/testing/cpp/driver_test.h>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <gtest/gtest.h>

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

class AmlSaradcTestEnvironment : fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    device_server_.Initialize(component::kDefaultInstance);
    device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &to_driver_vfs);

    fake_pdev::FakePDevFidl::Config config;
    config.irqs[0] = {};
    zx_status_t status =
        zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &config.irqs[0]);
    EXPECT_EQ(ZX_OK, status);
    config.mmios[0] = mmio_[0].mmio();
    config.mmios[1] = mmio_[1].mmio();
    irq_ = config.irqs[0].borrow();

    pdev_server_.SetConfig(std::move(config));

    fuchsia_hardware_adcimpl::Metadata metadata{{.channels = {}}};

    auto raw_metadata = fidl::Persist(metadata);
    EXPECT_TRUE(raw_metadata.is_ok());
    pdev_server_.set_metadata(
        {{fuchsia_hardware_adcimpl::kMetadataTypeName, std::move(raw_metadata.value())}});
    return to_driver_vfs.AddService<fuchsia_hardware_platform_device::Service>(
        pdev_server_.GetInstanceHandler(fdf::Dispatcher::GetCurrent()->async_dispatcher()));
  }

  FakeMmio* mmio() { return mmio_; }
  zx::unowned_interrupt& irq() { return irq_; }

 private:
  compat::DeviceServer device_server_;

  FakeMmio mmio_[kRegisterBanks];
  zx::unowned_interrupt irq_;
  fake_pdev::FakePDevFidl pdev_server_;
};

class AmlSaradcTestConfig final {
 public:
  using DriverType = aml_saradc::AmlSaradc;
  using EnvironmentType = AmlSaradcTestEnvironment;
};

class AmlSaradcTest : public ::testing::Test {
 public:
  void SetUp() override {
    zx::result<> result = driver_test().StartDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
    auto connect_result = driver_test().Connect<fuchsia_hardware_adcimpl::Service::Device>(
        component::kDefaultInstance);
    ASSERT_TRUE(connect_result.is_ok());
    adc_.Bind(std::move(connect_result.value()));
    ASSERT_TRUE(adc_.is_valid());
  }

  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  fdf::WireSyncClient<fuchsia_hardware_adcimpl::Device>& adc() { return adc_; }

  fdf_testing::BackgroundDriverTest<AmlSaradcTestConfig>& driver_test() { return driver_test_; }

 private:
  fdf_testing::BackgroundDriverTest<AmlSaradcTestConfig> driver_test_;
  fdf::WireSyncClient<fuchsia_hardware_adcimpl::Device> adc_;
};

TEST_F(AmlSaradcTest, GetResolution) {
  fdf::Arena arena('TEST');
  auto result = adc().buffer(arena)->GetResolution();
  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_ok());
  EXPECT_EQ(result.value()->resolution, 10);
}

TEST_F(AmlSaradcTest, GetSample) {
  driver_test().RunInEnvironmentTypeContext([](AmlSaradcTestEnvironment& env) {
    env.mmio()[0].set(AO_SAR_ADC_FIFO_RD_OFFS >> 2, 0x4);
    env.irq()->trigger(0, zx::clock::get_boot());
  });

  fdf::Arena arena('TEST');
  auto result = adc().buffer(arena)->GetSample(0);
  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_ok());
  EXPECT_EQ(result.value()->value, 1u);
}

TEST_F(AmlSaradcTest, GetSampleInvalidArgs) {
  fdf::Arena arena('TEST');
  auto result = adc().buffer(arena)->GetSample(8);
  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_error());
  EXPECT_EQ(result->error_value(), ZX_ERR_INVALID_ARGS);
}

}  // namespace
