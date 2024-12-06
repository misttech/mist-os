// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/adc/drivers/adc/adc.h"

#include <lib/ddk/metadata.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <cmath>

#include <gtest/gtest.h>

#include "src/devices/adc/metadata/metadata.h"
#include "src/devices/lib/fidl-metadata/adc.h"

namespace adc {

class FakeAdcImplServer : public fdf::Server<fuchsia_hardware_adcimpl::Device> {
 public:
  ~FakeAdcImplServer() {
    for (const auto& [_, expected] : expected_samples_) {
      EXPECT_TRUE(expected.empty());
    }
  }

  void set_resolution(uint8_t resolution) { resolution_ = resolution; }
  void ExpectGetSample(uint32_t channel, uint32_t sample) {
    expected_samples_[channel].push(sample);
  }

  void GetResolution(GetResolutionCompleter::Sync& completer) override {
    completer.Reply(fit::ok(resolution_));
  }
  void GetSample(GetSampleRequest& request, GetSampleCompleter::Sync& completer) override {
    ASSERT_FALSE(expected_samples_.empty());
    ASSERT_NE(expected_samples_.find(request.channel_id()), expected_samples_.end());
    ASSERT_FALSE(expected_samples_.at(request.channel_id()).empty());
    completer.Reply(fit::ok(expected_samples_.at(request.channel_id()).front()));
    expected_samples_.at(request.channel_id()).pop();
  }

  fuchsia_hardware_adcimpl::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_adcimpl::Service::InstanceHandler({
        .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                          fidl::kIgnoreBindingClosure),
    });
  }

 private:
  uint8_t resolution_ = 0;
  std::map<uint32_t, std::queue<uint32_t>> expected_samples_;

  fdf::ServerBindingGroup<fuchsia_hardware_adcimpl::Device> bindings_;
};

class AdcTestEnvironment : fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    device_server_.Initialize(component::kDefaultInstance);
    device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &to_driver_vfs);

    zx::result result = to_driver_vfs.AddService<fuchsia_hardware_adcimpl::Service>(
        fake_adc_impl_server_.GetInstanceHandler());
    if (result.is_error()) {
      return result.take_error();
    }

    if (zx::result result = metadata_server_.Serve(
            to_driver_vfs, fdf::Dispatcher::GetCurrent()->async_dispatcher());
        result.is_error()) {
      return result.take_error();
    }

    return zx::ok();
  }

  void Init(std::vector<fidl_metadata::adc::Channel> kAdcChannels) {
    std::vector<fuchsia_hardware_adcimpl::AdcChannel> channels;
    std::transform(
        kAdcChannels.begin(), kAdcChannels.end(), std::back_inserter(channels),
        [](const auto& channel) {
          return fuchsia_hardware_adcimpl::AdcChannel{{.idx = channel.idx, .name = channel.name}};
        });
    fuchsia_hardware_adcimpl::Metadata metadata{{.channels = std::move(channels)}};

    ASSERT_EQ(ZX_OK, metadata_server_.SetMetadata(metadata).status_value());
  }

  FakeAdcImplServer& fake_adc_impl_server() { return fake_adc_impl_server_; }

 private:
  compat::DeviceServer device_server_;
  FakeAdcImplServer fake_adc_impl_server_;
  MetadataServer metadata_server_;
};

class AdcTestConfig final {
 public:
  using DriverType = adc::Adc;
  using EnvironmentType = AdcTestEnvironment;
};

class AdcTest : public ::testing::Test {
 public:
  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  zx::result<> Init(const std::vector<fidl_metadata::adc::Channel>& kAdcChannels) {
    driver_test().RunInEnvironmentTypeContext(
        [kAdcChannels](AdcTestEnvironment& env) { env.Init(kAdcChannels); });
    return driver_test().StartDriver();
  }
  fidl::ClientEnd<fuchsia_hardware_adc::Device> GetClient(uint32_t channel) {
    // Connect to Adc.
    auto result =
        driver_test().Connect<fuchsia_hardware_adc::Service::Device>(std::to_string(channel));
    EXPECT_EQ(ZX_OK, result.status_value());
    return std::move(result.value());
  }

  fdf_testing::BackgroundDriverTest<AdcTestConfig>& driver_test() { return driver_test_; }

 private:
  fdf_testing::BackgroundDriverTest<AdcTestConfig> driver_test_;
};

TEST_F(AdcTest, CreateDevicesTest) {
  auto result = Init({DECL_ADC_CHANNEL(1), DECL_ADC_CHANNEL(4)});
  ASSERT_TRUE(result.is_ok());

  driver_test().RunInNodeContext([](fdf_testing::TestNode& node) {
    ASSERT_EQ(node.children().size(), 2ul);
    EXPECT_NE(node.children().find("1"), node.children().end());
    EXPECT_NE(node.children().find("4"), node.children().end());
  });
}

TEST_F(AdcTest, OverlappingChannelsTest) {
  auto result = Init({DECL_ADC_CHANNEL(1), DECL_ADC_CHANNEL(4), DECL_ADC_CHANNEL(1)});
  ASSERT_TRUE(result.is_error());
  EXPECT_EQ(result.error_value(), ZX_ERR_INVALID_ARGS);
}

TEST_F(AdcTest, GetResolutionTest) {
  driver_test().RunInEnvironmentTypeContext(
      [](AdcTestEnvironment& env) { env.fake_adc_impl_server().set_resolution(12); });
  auto result = Init({DECL_ADC_CHANNEL(1), DECL_ADC_CHANNEL(4)});
  ASSERT_TRUE(result.is_ok());

  auto resolution = fidl::WireCall(GetClient(1))->GetResolution();
  ASSERT_TRUE(resolution.ok());
  ASSERT_TRUE(resolution->is_ok());
  EXPECT_EQ(resolution.value()->resolution, 12);
}

TEST_F(AdcTest, GetSampleTest) {
  auto result = Init({DECL_ADC_CHANNEL(1), DECL_ADC_CHANNEL(4)});
  ASSERT_TRUE(result.is_ok());

  driver_test().RunInEnvironmentTypeContext(
      [](AdcTestEnvironment& env) { env.fake_adc_impl_server().ExpectGetSample(1, 20); });
  auto sample = fidl::WireCall(GetClient(1))->GetSample();
  ASSERT_TRUE(sample.ok());
  ASSERT_TRUE(sample->is_ok());
  EXPECT_EQ(sample.value()->value, 20u);
}

TEST_F(AdcTest, GetNormalizedSampleTest) {
  driver_test().RunInEnvironmentTypeContext([](AdcTestEnvironment& env) {
    env.fake_adc_impl_server().set_resolution(2);
    env.fake_adc_impl_server().ExpectGetSample(4, 9);
  });

  auto result = Init({DECL_ADC_CHANNEL(1), DECL_ADC_CHANNEL(4)});
  ASSERT_TRUE(result.is_ok());

  auto sample = fidl::WireCall(GetClient(4))->GetNormalizedSample();
  ASSERT_TRUE(sample.ok());
  ASSERT_TRUE(sample->is_ok());
  EXPECT_EQ(std::lround(sample.value()->value), 3);
}

TEST_F(AdcTest, ChannelOutOfBoundsTest) {
  auto result = Init({DECL_ADC_CHANNEL(1), DECL_ADC_CHANNEL(4)});
  ASSERT_TRUE(result.is_ok());

  driver_test().RunInEnvironmentTypeContext(
      [](AdcTestEnvironment& env) { env.fake_adc_impl_server().set_resolution(12); });
  auto resolution = fidl::WireCall(GetClient(3))->GetResolution();
  ASSERT_FALSE(resolution.ok());
}

}  // namespace adc
