// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.clock/cpp/wire_test_base.h>
#include <fuchsia/hardware/shareddma/cpp/banjo-mock.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/zx/clock.h>
#include <lib/zx/interrupt.h>
#include <zircon/errors.h>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <soc/as370/as370-hw.h>
#include <zxtest/zxtest.h>

#include "audio-stream-out.h"
#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

bool operator==(const shared_dma_protocol_t& a, const shared_dma_protocol_t& b) { return true; }
bool operator==(const dma_notify_callback_t& a, const dma_notify_callback_t& b) { return true; }

namespace audio::as370 {

fidl::WireSyncClient<audio_fidl::StreamConfig> GetStreamClient(
    fidl::ClientEnd<audio_fidl::StreamConfigConnector> client) {
  fidl::WireSyncClient client_wrap{std::move(client)};
  if (!client_wrap.is_valid()) {
    return {};
  }
  auto endpoints = fidl::CreateEndpoints<audio_fidl::StreamConfig>();
  if (!endpoints.is_ok()) {
    return {};
  }
  auto [stream_channel_local, stream_channel_remote] = *std::move(endpoints);
  auto ret = client_wrap->Connect(std::move(stream_channel_remote));
  if (!ret.ok()) {
    return {};
  }
  return fidl::WireSyncClient<audio_fidl::StreamConfig>(std::move(stream_channel_local));
}

class FakeSharedDmaDevice : public ddk::SharedDmaProtocol<FakeSharedDmaDevice, ddk::base_protocol> {
 public:
  FakeSharedDmaDevice() : proto_({&shared_dma_protocol_ops_, this}) {}

  void SharedDmaStart(uint32_t dma_id) {}
  void SharedDmaStop(uint32_t dma_id) {}
  uint32_t SharedDmaGetTransferSize(uint32_t dma_id) { return 0; }
  uint32_t SharedDmaGetBufferPosition(uint32_t dma_id) { return 0; }
  zx_status_t SharedDmaInitializeAndGetBuffer(uint32_t dma_id, dma_type_t type, uint32_t size,
                                              zx::vmo* out_vmo) {
    return ZX_OK;
  }
  zx_status_t SharedDmaSetNotifyCallback(uint32_t dma_id, const dma_notify_callback_t* cb,
                                         uint32_t* out_size_per_notification) {
    return ZX_OK;
  }

  const shared_dma_protocol_t* GetProto() const { return &proto_; }

 private:
  shared_dma_protocol_t proto_;
};

class FakeCodecDevice : public ddk::CodecProtocol<FakeCodecDevice, ddk::base_protocol>,
                        public fidl::WireServer<fuchsia_hardware_audio::Codec> {
 public:
  FakeCodecDevice() : proto_({&codec_protocol_ops_, this}) {
    ASSERT_OK(loop_.StartThread("Fake codec thread"));
  }

  zx_status_t CodecConnect(zx::channel channel) {
    fidl::ServerEnd<fuchsia_hardware_audio::Codec> server(std::move(channel));
    fidl::BindServer(loop_.dispatcher(), std::move(server), this);
    return ZX_OK;
  }

  // LLCPP implementation for the Codec API.
  void Reset(ResetCompleter::Sync& completer) override { completer.Reply(); }
  void Stop(StopCompleter::Sync& completer) override { completer.Reply({}); }
  void Start(StartCompleter::Sync& completer) override { completer.Reply({}); }
  void GetProperties(GetPropertiesCompleter::Sync& completer) override { completer.Reply({}); }
  void GetHealthState(GetHealthStateCompleter::Sync& completer) override { completer.Reply({}); }
  void IsBridgeable(IsBridgeableCompleter::Sync& completer) override { completer.Reply({}); }
  void SetBridgedMode(SetBridgedModeRequestView request,
                      SetBridgedModeCompleter::Sync& completer) override {}
  void GetDaiFormats(GetDaiFormatsCompleter::Sync& completer) override {
    completer.ReplySuccess({});
  }
  void SetDaiFormat(SetDaiFormatRequestView request,
                    SetDaiFormatCompleter::Sync& completer) override {
    completer.ReplySuccess({});
  }
  void WatchPlugState(WatchPlugStateCompleter::Sync& completer) override { completer.Reply({}); }
  void SignalProcessingConnect(SignalProcessingConnectRequestView request,
                               SignalProcessingConnectCompleter::Sync& completer) override {
    request->protocol.Close(ZX_ERR_NOT_SUPPORTED);
  }

  const codec_protocol_t* GetProto() const { return &proto_; }

 private:
  codec_protocol_t proto_;
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
};

class FakeClockDevice : public fidl::testing::WireTestBase<fuchsia_hardware_clock::Clock> {
 public:
  fuchsia_hardware_clock::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_clock::Service::InstanceHandler({
        .clock = binding_group_.CreateHandler(this, async_get_default_dispatcher(),
                                              fidl::kIgnoreBindingClosure),
    });
  }

  void Enable(EnableCompleter::Sync& completer) override { completer.ReplySuccess(); }

  void SetRate(SetRateRequestView request, SetRateCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  fidl::ServerBindingGroup<fuchsia_hardware_clock::Clock> binding_group_;
};

class FakeMmio {
 public:
  explicit FakeMmio(uint32_t size) : mmio_(sizeof(uint32_t), size / sizeof(uint32_t)) {}

  fdf::MmioBuffer mmio() { return fdf::MmioBuffer(mmio_.GetMmioBuffer()); }
  ddk_fake::FakeMmioReg& reg(size_t ix) { return mmio_[ix]; }

 private:
  ddk_fake::FakeMmioRegRegion mmio_;
};

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
  FakeClockDevice clock_server;
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
};

struct As370TdmOutputTest : public zxtest::Test {
  void SetUp() override {
    fake_pdev::FakePDevFidl::Config config;
    config.mmios[0] = mmio0_.mmio();
    config.mmios[1] = mmio1_.mmio();

    zx::result pdev_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(pdev_endpoints);
    zx::result clock_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(clock_endpoints);
    ASSERT_OK(incoming_loop_.StartThread("incoming-ns-thread"));
    incoming_.SyncCall(
        [config = std::move(config), pdev_server = std::move(pdev_endpoints->server),
         clock_server = std::move(clock_endpoints->server)](IncomingNamespace* infra) mutable {
          infra->pdev_server.SetConfig(std::move(config));
          ASSERT_OK(infra->outgoing.AddService<fuchsia_hardware_platform_device::Service>(
              infra->pdev_server.GetInstanceHandler()));
          ASSERT_OK(infra->outgoing.Serve(std::move(pdev_server)));
          ASSERT_OK(infra->outgoing.AddService<fuchsia_hardware_clock::Service>(
              infra->clock_server.GetInstanceHandler()));
          ASSERT_OK(infra->outgoing.Serve(std::move(clock_server)));
        });
    ASSERT_NO_FATAL_FAILURE();
    fake_parent_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                                 std::move(pdev_endpoints->client), "pdev");
    fake_parent_->AddFidlService(fuchsia_hardware_clock::Service::Name,
                                 std::move(clock_endpoints->client), "clock");

    fake_parent_->AddProtocol(ZX_PROTOCOL_SHARED_DMA, dma_.GetProto()->ops, dma_.GetProto()->ctx,
                              "dma");
    fake_parent_->AddProtocol(ZX_PROTOCOL_CODEC, codec_.GetProto()->ops, codec_.GetProto()->ctx,
                              "codec");
  }

  void TearDown() override {}

  std::shared_ptr<zx_device> fake_parent_ = MockDevice::FakeRootParent();
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  fbl::RefPtr<As370AudioStreamOut> device_;
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  FakeSharedDmaDevice dma_;
  FakeCodecDevice codec_;
  FakeMmio mmio0_{::as370::kAudioGlobalSize}, mmio1_{::as370::kAudioI2sSize};
};

TEST_F(As370TdmOutputTest, GetSupportedFormatsWithMetadata) {
  metadata::As370Config metadata = {};
  snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "manufacturer");
  snprintf(metadata.product_name, sizeof(metadata.product_name), "product");
  metadata.is_input = false;
  metadata.ring_buffer.number_of_channels = 2;
  metadata.ring_buffer.frequency_ranges[0].min_frequency = 20;
  metadata.ring_buffer.frequency_ranges[0].max_frequency = 2'000;
  metadata.ring_buffer.frequency_ranges[1].min_frequency = 2'000;
  metadata.ring_buffer.frequency_ranges[1].max_frequency = 48'000;
  fake_parent_->SetMetadata(DEVICE_METADATA_PRIVATE, &metadata, sizeof(metadata));

  device_ = audio::SimpleAudioStream::Create<audio::as370::As370AudioStreamOut>(fake_parent_.get());
  ZX_ASSERT(device_.get());

  auto connector_endpoints = fidl::CreateEndpoints<audio_fidl::StreamConfigConnector>();
  ASSERT_TRUE(connector_endpoints.is_ok());

  loop_.StartThread("fidl-thread");
  fidl::BindServer(loop_.dispatcher(), std::move(connector_endpoints->server), device_.get());

  auto stream_client = GetStreamClient(std::move(connector_endpoints->client));
  ASSERT_TRUE(stream_client.is_valid());

  auto properties = stream_client->GetProperties();
  ASSERT_OK(properties.status());

  EXPECT_STREQ(properties->properties.manufacturer(), "manufacturer");
  EXPECT_STREQ(properties->properties.product(), "product");

  auto supported = stream_client->GetSupportedFormats();
  ASSERT_OK(supported.status());

  EXPECT_EQ(supported->supported_formats.count(), 1);
  EXPECT_EQ(supported->supported_formats[0].pcm_supported_formats().channel_sets().count(), 1);
  EXPECT_EQ(supported->supported_formats[0]
                .pcm_supported_formats()
                .channel_sets()[0]
                .attributes()
                .count(),
            2);
  EXPECT_EQ(supported->supported_formats[0]
                .pcm_supported_formats()
                .channel_sets()[0]
                .attributes()[0]
                .min_frequency(),
            20);
  EXPECT_EQ(supported->supported_formats[0]
                .pcm_supported_formats()
                .channel_sets()[0]
                .attributes()[0]
                .max_frequency(),
            2'000);
  EXPECT_EQ(supported->supported_formats[0]
                .pcm_supported_formats()
                .channel_sets()[0]
                .attributes()[1]
                .min_frequency(),
            2'000);
  EXPECT_EQ(supported->supported_formats[0]
                .pcm_supported_formats()
                .channel_sets()[0]
                .attributes()[1]
                .max_frequency(),
            48'000);

  EXPECT_EQ(supported->supported_formats[0].pcm_supported_formats().frame_rates().count(), 1);
  EXPECT_EQ(supported->supported_formats[0].pcm_supported_formats().frame_rates()[0], 96'000);
}

}  // namespace audio::as370
