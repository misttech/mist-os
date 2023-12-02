// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/aml-g12-tdm/composite.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdio/directory.h>
#include <lib/fzl/vmo-mapper.h>

#include <algorithm>

#include <zxtest/zxtest.h>

namespace audio::aml_g12 {

// TODO(fxbug.dev/132252): Complete audio-composite tests.
class FakePlatformDevice : public fidl::WireServer<fuchsia_hardware_platform_device::Device> {
 public:
  fuchsia_hardware_platform_device::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_platform_device::Service::InstanceHandler({
        .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                          fidl::kIgnoreBindingClosure),
    });
  }

  void InitResources() { EXPECT_OK(zx::vmo::create(kMmioSize, 0, &mmio_)); }

  cpp20::span<uint32_t> mmio() {
    // The test has to wait for the driver to set the MMIO cache policy before mapping.
    if (!mapped_mmio_.start()) {
      MapMmio();
    }

    return {reinterpret_cast<uint32_t*>(mapped_mmio_.start()), kMmioSize / sizeof(uint32_t)};
  }

 private:
  static constexpr size_t kMmioSize = 0x1000;

  void GetMmio(fuchsia_hardware_platform_device::wire::DeviceGetMmioRequest* request,
               GetMmioCompleter::Sync& completer) override {
    if (request->index != 0) {
      return completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
    }

    zx::vmo vmo;
    if (zx_status_t status = mmio_.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo); status != ZX_OK) {
      return completer.ReplyError(status);
    }

    fidl::Arena arena;
    completer.ReplySuccess(fuchsia_hardware_platform_device::wire::Mmio::Builder(arena)
                               .offset(0)
                               .size(kMmioSize)
                               .vmo(std::move(vmo))
                               .Build());
  }

  void GetInterrupt(fuchsia_hardware_platform_device::wire::DeviceGetInterruptRequest* request,
                    GetInterruptCompleter::Sync& completer) override {}

  void GetBti(fuchsia_hardware_platform_device::wire::DeviceGetBtiRequest* request,
              GetBtiCompleter::Sync& completer) override {}

  void GetSmc(fuchsia_hardware_platform_device::wire::DeviceGetSmcRequest* request,
              GetSmcCompleter::Sync& completer) override {}

  void GetDeviceInfo(GetDeviceInfoCompleter::Sync& completer) override {}

  void GetBoardInfo(GetBoardInfoCompleter::Sync& completer) override {}

  void MapMmio() { ASSERT_OK(mapped_mmio_.Map(mmio_)); }

  zx::vmo mmio_;
  fzl::VmoMapper mapped_mmio_;

  fidl::ServerBindingGroup<fuchsia_hardware_platform_device::Device> bindings_;
};

struct IncomingNamespace {
  fdf_testing::TestNode node_{std::string("root")};
  fdf_testing::TestEnvironment env_{fdf::Dispatcher::GetCurrent()->get()};
};

class AmlG12CompositeTest : public zxtest::Test {
 public:
  AmlG12CompositeTest()
      : env_dispatcher_(runtime_.StartBackgroundDispatcher()),
        driver_dispatcher_(runtime_.StartBackgroundDispatcher()),
        incoming_(env_dispatcher(), std::in_place) {}

  void SetUp() override {
    fuchsia_driver_framework::DriverStartArgs driver_start_args;
    incoming_.SyncCall([&driver_start_args, this](IncomingNamespace* incoming) {
      auto start_args_result = incoming->node_.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());

      auto init_result =
          incoming->env_.Initialize(std::move(start_args_result->incoming_directory_server));
      ASSERT_TRUE(init_result.is_ok());

      platform_device_.InitResources();
      const zx::result add_platform_result =
          incoming->env_.incoming_directory().AddService<fuchsia_hardware_platform_device::Service>(
              platform_device_.GetInstanceHandler());
      EXPECT_TRUE(add_platform_result.is_ok());

      driver_start_args = std::move(start_args_result->start_args);
    });

    zx::result result = runtime_.RunToCompletion(
        dut_.SyncCall(&fdf_testing::DriverUnderTest<Driver>::Start, std::move(driver_start_args)));
    ASSERT_EQ(ZX_OK, result.status_value());

    incoming_.SyncCall([this](IncomingNamespace* incoming) {
      auto client_channel = incoming->node_.children().at(kDriverName).ConnectToDevice();
      client_.Bind(
          fidl::ClientEnd<fuchsia_hardware_audio::Composite>(std::move(client_channel.value())));
      ASSERT_TRUE(client_.is_valid());
    });
  }

  void TearDown() override {
    zx::result result =
        runtime_.RunToCompletion(dut_.SyncCall(&fdf_testing::DriverUnderTest<Driver>::PrepareStop));
    ASSERT_EQ(ZX_OK, result.status_value());
  }

 protected:
  fidl::SyncClient<fuchsia_hardware_audio::Composite> client_;
  FakePlatformDevice platform_device_;

 private:
  async_dispatcher_t* driver_dispatcher() { return driver_dispatcher_->async_dispatcher(); }
  async_dispatcher_t* env_dispatcher() { return env_dispatcher_->async_dispatcher(); }

  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_;
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_;
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_;
  // Use dut_ instead of driver_ because driver_ is used by zxtest.
  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<Driver>> dut_{
      driver_dispatcher(), std::in_place};
};

TEST_F(AmlG12CompositeTest, CompositeProperties) {
  fidl::Result result = client_->GetProperties();
  ASSERT_TRUE(result.is_ok());
  ASSERT_EQ(fuchsia_hardware_audio::kClockDomainMonotonic,
            result->properties().clock_domain().value());
}

TEST_F(AmlG12CompositeTest, Reset) {
  // TODO(fxbug.dev/132252): Add behavior to change state recovered to the configuration below.
  fidl::Result reset_result = client_->Reset();
  ASSERT_TRUE(reset_result.is_ok());

  // After reset we check we have configured all engines for TDM output and input.

  // Configure TDM OUT for I2S (default).
  // TDM OUT CTRL0 config, bitoffset 2, 2 slots, 32 bits per slot is 0x3001002F.
  ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x500 / 4]);  // A output.
  ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x540 / 4]);  // B output.
  ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x580 / 4]);  // C output.

  // Configure TDM IN for I2S (default).
  // TDM IN CTRL0 config, PAD_TDMIN A(0), B(1) and C(2).
  ASSERT_EQ(0x7003'001F, platform_device_.mmio()[0x300 / 4]);  // A input.
  ASSERT_EQ(0x7013'001F, platform_device_.mmio()[0x340 / 4]);  // B input.
  ASSERT_EQ(0x7023'001F, platform_device_.mmio()[0x380 / 4]);  // C input.

  // Configure clocks.
  // SCLK CTRL, clk in/out enabled, 24 sdiv, 32 lrduty, 64 lrdiv.
  ASSERT_EQ(0xc180'7c3f, platform_device_.mmio()[0x040 / 4]);  // A.
  ASSERT_EQ(0xc180'7c3f, platform_device_.mmio()[0x048 / 4]);  // B.
  ASSERT_EQ(0xc180'7c3f, platform_device_.mmio()[0x050 / 4]);  // C.
}

TEST_F(AmlG12CompositeTest, ElementsAndTopology) {
  auto endpoints =
      fidl::CreateEndpoints<fuchsia_hardware_audio_signalprocessing::SignalProcessing>();
  auto connect_result = client_->SignalProcessingConnect(std::move(endpoints->server));
  ASSERT_TRUE(connect_result.is_ok());
  fidl::SyncClient signal_client{std::move(endpoints->client)};
  auto elements_result = signal_client->GetElements();
  ASSERT_TRUE(elements_result.is_ok());

  // Got ring buffer and DAIs for all engines input and output.
  constexpr size_t kNumberOfElements = 9;
  ASSERT_EQ(kNumberOfElements, elements_result->processing_elements().size());
  for (size_t i = 0; i < kNumberOfElements; ++i) {
    auto& element = elements_result->processing_elements()[i];
    ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint, element.type());
    ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kHardwired,
              element.type_specific()->endpoint()->plug_detect_capabilities());
    auto watch_element_result = signal_client->WatchElementState(*element.id());
    ASSERT_TRUE(watch_element_result.is_ok());
    ASSERT_EQ(true,
              watch_element_result->state().type_specific()->endpoint()->plug_state()->plugged());
  }
  auto& element0 = elements_result->processing_elements()[0];
  auto& element1 = elements_result->processing_elements()[1];
  auto& element2 = elements_result->processing_elements()[2];
  auto& element3 = elements_result->processing_elements()[3];
  auto& element4 = elements_result->processing_elements()[4];
  auto& element5 = elements_result->processing_elements()[5];
  auto& element6 = elements_result->processing_elements()[6];
  auto& element7 = elements_result->processing_elements()[7];
  auto& element8 = elements_result->processing_elements()[8];
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer,
            element0.type_specific()->endpoint()->type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer,
            element1.type_specific()->endpoint()->type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer,
            element2.type_specific()->endpoint()->type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer,
            element3.type_specific()->endpoint()->type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer,
            element4.type_specific()->endpoint()->type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer,
            element5.type_specific()->endpoint()->type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect,
            element6.type_specific()->endpoint()->type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect,
            element7.type_specific()->endpoint()->type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect,
            element8.type_specific()->endpoint()->type());

  auto topology_result = signal_client->GetTopologies();
  ASSERT_TRUE(topology_result.is_ok());
  ASSERT_EQ(1, topology_result->topologies().size());
  auto& topology = topology_result->topologies()[0];
  ASSERT_EQ(1, topology.id());

  // Get edges for ring buffer and DAIs for all engines.
  constexpr size_t kNumberOfEdges = 6;
  ASSERT_EQ(kNumberOfEdges, topology.processing_elements_edge_pairs()->size());
  auto& edge0 = (*topology.processing_elements_edge_pairs())[0];
  auto& edge1 = (*topology.processing_elements_edge_pairs())[1];
  auto& edge2 = (*topology.processing_elements_edge_pairs())[2];
  auto& edge3 = (*topology.processing_elements_edge_pairs())[3];
  auto& edge4 = (*topology.processing_elements_edge_pairs())[4];
  auto& edge5 = (*topology.processing_elements_edge_pairs())[5];

  // Output.
  ASSERT_EQ(4, edge0.processing_element_id_from());
  ASSERT_EQ(1, edge0.processing_element_id_to());
  ASSERT_EQ(5, edge1.processing_element_id_from());
  ASSERT_EQ(2, edge1.processing_element_id_to());
  ASSERT_EQ(6, edge2.processing_element_id_from());
  ASSERT_EQ(3, edge2.processing_element_id_to());

  // Input.
  ASSERT_EQ(1, edge3.processing_element_id_from());
  ASSERT_EQ(7, edge3.processing_element_id_to());
  ASSERT_EQ(2, edge4.processing_element_id_from());
  ASSERT_EQ(8, edge4.processing_element_id_to());
  ASSERT_EQ(3, edge5.processing_element_id_from());
  ASSERT_EQ(9, edge5.processing_element_id_to());

  auto watch_topology_result = signal_client->WatchTopology();
  ASSERT_TRUE(watch_topology_result.is_ok());
  ASSERT_EQ(1, watch_topology_result->topology_id());
}

TEST_F(AmlG12CompositeTest, ElementsState) {
  auto endpoints =
      fidl::CreateEndpoints<fuchsia_hardware_audio_signalprocessing::SignalProcessing>();
  auto connect_result = client_->SignalProcessingConnect(std::move(endpoints->server));
  ASSERT_TRUE(connect_result.is_ok());
  fidl::SyncClient signal_client{std::move(endpoints->client)};
  auto elements_result = signal_client->GetElements();
  ASSERT_TRUE(elements_result.is_ok());

  // Able to set element state for all (endpoint) elements with no parameters.
  for (auto element : elements_result->processing_elements()) {
    if (element.type() == fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint) {
      fuchsia_hardware_audio_signalprocessing::SignalProcessingSetElementStateRequest request;
      request.processing_element_id(*element.id());
      auto set_element_result = signal_client->SetElementState(std::move(request));
      ASSERT_TRUE(set_element_result.is_ok());
    }
  }
}

}  // namespace audio::aml_g12
