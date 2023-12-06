// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/adc/drivers/adc/adc.h"

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <fbl/auto_lock.h>
#include <zxtest/zxtest.h>

#include "src/devices/lib/fidl-metadata/adc.h"

namespace {

struct IncomingNamespace {
  fdf_testing::TestNode node_{std::string("root")};
  fdf_testing::TestEnvironment env_{fdf::Dispatcher::GetCurrent()->get()};
  compat::DeviceServer device_server_;

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
  FakeAdcImplServer fake_adc_impl_server_;
};

class AdcTest : public zxtest::Test {
 public:
  zx::result<> Init(std::vector<fidl_metadata::adc::Channel> kAdcChannels) {
    fuchsia_driver_framework::DriverStartArgs start_args;
    incoming_.SyncCall([&](IncomingNamespace* incoming) {
      auto start_args_result = incoming->node_.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());
      start_args = std::move(start_args_result->start_args);
      outgoing_directory_client_ = std::move(start_args_result->outgoing_directory_client);

      auto init_result =
          incoming->env_.Initialize(std::move(start_args_result->incoming_directory_server));
      ASSERT_TRUE(init_result.is_ok());

      incoming->device_server_.Init(component::kDefaultInstance, "");

      // Serve metadata.
      auto metadata = fidl_metadata::adc::AdcChannelsToFidl(kAdcChannels);
      ASSERT_TRUE(metadata.is_ok());
      auto status = incoming->device_server_.AddMetadata(DEVICE_METADATA_ADC, metadata->data(),
                                                         metadata->size());
      EXPECT_OK(status);
      status = incoming->device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                              &incoming->env_.incoming_directory());
      EXPECT_OK(status);

      // Serve fake_adc_impl_server_.
      auto result =
          incoming->env_.incoming_directory().AddService<fuchsia_hardware_adcimpl::Service>(
              std::move(incoming->fake_adc_impl_server_.GetInstanceHandler()));
      ASSERT_TRUE(result.is_ok());
    });

    // Start dut_.
    return runtime_.RunToCompletion(
        dut_.SyncCall(&fdf_testing::DriverUnderTest<adc::Adc>::Start, std::move(start_args)));
  }

  fidl::ClientEnd<fuchsia_hardware_adc::Device> GetClient(uint32_t channel) {
    // Connect to Adc.
    auto svc_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    EXPECT_EQ(ZX_OK, svc_endpoints.status_value());

    zx_status_t status = fdio_open_at(outgoing_directory_client_.handle()->get(), "/svc",
                                      static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                                      svc_endpoints->server.TakeChannel().release());
    EXPECT_EQ(ZX_OK, status);

    auto connect_result = component::ConnectAtMember<fuchsia_hardware_adc::Service::Device>(
        svc_endpoints->client, std::to_string(channel));
    EXPECT_TRUE(connect_result.is_ok());
    return std::move(connect_result.value());
  }

  void TearDown() override {
    zx::result result = runtime_.RunToCompletion(
        dut_.SyncCall(&fdf_testing::DriverUnderTest<adc::Adc>::PrepareStop));
    ASSERT_EQ(ZX_OK, result.status_value());

    incoming_.SyncCall([](IncomingNamespace* incoming) {
      auto result =
          incoming->env_.incoming_directory().RemoveService<fuchsia_hardware_adcimpl::Service>();
      EXPECT_TRUE(result.is_ok());
    });
  }

 private:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_ = runtime_.StartBackgroundDispatcher();

  fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory_client_;

 protected:
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{
      env_dispatcher_->async_dispatcher(), std::in_place};
  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<adc::Adc>> dut_{
      driver_dispatcher_->async_dispatcher(), std::in_place};
};

TEST_F(AdcTest, CreateDevicesTest) {
  auto result = Init({DECL_ADC_CHANNEL(1), DECL_ADC_CHANNEL(4)});
  ASSERT_TRUE(result.is_ok());

  incoming_.SyncCall([](IncomingNamespace* incoming) {
    ASSERT_EQ(incoming->node_.children().size(), 2);
    EXPECT_NE(incoming->node_.children().find("1"), incoming->node_.children().end());
    EXPECT_NE(incoming->node_.children().find("4"), incoming->node_.children().end());
  });
}

TEST_F(AdcTest, OverlappingChannelsTest) {
  auto result = Init({DECL_ADC_CHANNEL(1), DECL_ADC_CHANNEL(4), DECL_ADC_CHANNEL(1)});
  ASSERT_TRUE(result.is_error());
  EXPECT_EQ(result.error_value(), ZX_ERR_INVALID_ARGS);
}

TEST_F(AdcTest, GetResolutionTest) {
  incoming_.SyncCall(
      [](IncomingNamespace* incoming) { incoming->fake_adc_impl_server_.set_resolution(12); });
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

  incoming_.SyncCall(
      [](IncomingNamespace* incoming) { incoming->fake_adc_impl_server_.ExpectGetSample(1, 20); });
  auto sample = fidl::WireCall(GetClient(1))->GetSample();
  ASSERT_TRUE(sample.ok());
  ASSERT_TRUE(sample->is_ok());
  EXPECT_EQ(sample.value()->value, 20);
}

TEST_F(AdcTest, GetNormalizedSampleTest) {
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    incoming->fake_adc_impl_server_.set_resolution(2);
    incoming->fake_adc_impl_server_.ExpectGetSample(4, 9);
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

  incoming_.SyncCall(
      [](IncomingNamespace* incoming) { incoming->fake_adc_impl_server_.set_resolution(12); });
  auto resolution = fidl::WireCall(GetClient(3))->GetResolution();
  ASSERT_FALSE(resolution.ok());
}

}  // namespace
