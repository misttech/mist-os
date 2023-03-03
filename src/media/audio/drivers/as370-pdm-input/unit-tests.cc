// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/hardware/clock/cpp/banjo-mock.h>
#include <fuchsia/hardware/shareddma/cpp/banjo-mock.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fake-bti/bti.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <soc/as370/as370-hw.h>
#include <zxtest/zxtest.h>

#include "audio-stream-in.h"
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

  void SetBti(zx::bti bti) { bti_ = std::move(bti); }

  void SharedDmaStart(uint32_t dma_id) {}
  void SharedDmaStop(uint32_t dma_id) {}
  uint32_t SharedDmaGetTransferSize(uint32_t dma_id) { return 0; }
  uint32_t SharedDmaGetBufferPosition(uint32_t dma_id) { return 0; }
  zx_status_t SharedDmaInitializeAndGetBuffer(uint32_t dma_id, dma_type_t type, uint32_t size,
                                              zx::vmo* out_vmo) {
    return zx::vmo::create_contiguous(bti_, 4096, 0, out_vmo);

    return ZX_OK;
  }
  zx_status_t SharedDmaSetNotifyCallback(uint32_t dma_id, const dma_notify_callback_t* cb,
                                         uint32_t* out_size_per_notification) {
    *out_size_per_notification = 4096;
    return ZX_OK;
  }

  const shared_dma_protocol_t* GetProto() const { return &proto_; }

 private:
  zx::bti bti_;
  shared_dma_protocol_t proto_;
};

class FakeClockDevice : public ddk::ClockProtocol<FakeClockDevice, ddk::base_protocol> {
 public:
  FakeClockDevice() : proto_({&clock_protocol_ops_, this}) {}

  zx_status_t ClockEnable() { return ZX_OK; }
  zx_status_t ClockDisable() { return ZX_OK; }
  zx_status_t ClockIsEnabled(bool* out_enabled) { return ZX_OK; }

  zx_status_t ClockSetRate(uint64_t hz) { return ZX_OK; }
  zx_status_t ClockQuerySupportedRate(uint64_t max_rate, uint64_t* out_max_supported_rate) {
    return ZX_OK;
  }
  zx_status_t ClockGetRate(uint64_t* out_current_rate) { return ZX_OK; }

  zx_status_t ClockSetInput(uint32_t idx) { return ZX_OK; }
  zx_status_t ClockGetNumInputs(uint32_t* out) { return ZX_OK; }
  zx_status_t ClockGetInput(uint32_t* out) { return ZX_OK; }

  const clock_protocol_t* GetProto() const { return &proto_; }

 private:
  clock_protocol_t proto_;
};

class FakeMmio {
 public:
  explicit FakeMmio(uint32_t size) : reg_count_(size / sizeof(uint32_t)) {  // in 32 bits chunks.
    regs_ = std::make_unique<ddk_fake::FakeMmioReg[]>(reg_count_);
    mmio_ =
        std::make_unique<ddk_fake::FakeMmioRegRegion>(regs_.get(), sizeof(uint32_t), reg_count_);
  }

  fake_pdev::MmioInfo mmio_info() { return {.offset = reinterpret_cast<size_t>(this)}; }

  fdf::MmioBuffer mmio() { return fdf::MmioBuffer(mmio_->GetMmioBuffer()); }
  ddk_fake::FakeMmioReg& reg(size_t ix) {
    return regs_[ix >> 2];  // Registers are in virtual address units.
  }

 private:
  const size_t reg_count_;
  std::unique_ptr<ddk_fake::FakeMmioReg[]> regs_;
  std::unique_ptr<ddk_fake::FakeMmioRegRegion> mmio_;
};

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
};

struct As370PdmInputTest : public zxtest::Test {
  void SetUp() override {
    fake_pdev::FakePDevFidl::Config config;
    config.mmios[0] = mmio0_.mmio_info();
    config.mmios[1] = mmio1_.mmio_info();
    config.btis[0] = {};
    ASSERT_OK(fake_bti_create(config.btis[0].reset_and_get_address()));
    zx::bti dup;
    ASSERT_OK(config.btis[0].duplicate(ZX_RIGHT_SAME_RIGHTS, &dup));
    dma_.SetBti(std::move(dup));

    zx::result outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(outgoing_endpoints);
    ASSERT_OK(incoming_loop_.StartThread("incoming-ns-thread"));
    incoming_.SyncCall([config = std::move(config), server = std::move(outgoing_endpoints->server)](
                           IncomingNamespace* infra) mutable {
      infra->pdev_server.SetConfig(std::move(config));
      ASSERT_OK(infra->outgoing.AddService<fuchsia_hardware_platform_device::Service>(
          infra->pdev_server.GetInstanceHandler()));

      ASSERT_OK(infra->outgoing.Serve(std::move(server)));
    });
    ASSERT_NO_FATAL_FAILURE();
    fake_parent_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                                 std::move(outgoing_endpoints->client), "pdev");

    fake_parent_->AddProtocol(ZX_PROTOCOL_CLOCK, clock_.GetProto()->ops, clock_.GetProto()->ctx,
                              "clock");
    fake_parent_->AddProtocol(ZX_PROTOCOL_SHARED_DMA, dma_.GetProto()->ops, dma_.GetProto()->ctx,
                              "dma");
  }

  void TearDown() override {}

  std::shared_ptr<zx_device> fake_parent_ = MockDevice::FakeRootParent();
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  fbl::RefPtr<As370AudioStreamIn> device_;
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  FakeClockDevice clock_;
  FakeSharedDmaDevice dma_;
  FakeMmio mmio0_{::as370::kAudioGlobalSize}, mmio1_{::as370::kAudioI2sSize};
};

TEST_F(As370PdmInputTest, GetSupportedFormats) {
  device_ = audio::SimpleAudioStream::Create<audio::as370::As370AudioStreamIn>(fake_parent_.get());
  ZX_ASSERT(device_.get());

  auto connector_endpoints = fidl::CreateEndpoints<audio_fidl::StreamConfigConnector>();
  ASSERT_TRUE(connector_endpoints.is_ok());

  loop_.StartThread("fidl-thread");
  fidl::BindServer(loop_.dispatcher(), std::move(connector_endpoints->server), device_.get());

  auto stream_client = GetStreamClient(std::move(connector_endpoints->client));
  ASSERT_TRUE(stream_client.is_valid());

  auto supported = stream_client->GetSupportedFormats();
  ASSERT_OK(supported.status());

  EXPECT_EQ(supported->supported_formats.count(), 1);
  EXPECT_EQ(supported->supported_formats[0].pcm_supported_formats().channel_sets().count(), 1);
  EXPECT_EQ(supported->supported_formats[0]
                .pcm_supported_formats()
                .channel_sets()[0]
                .attributes()
                .count(),
            3);  // 3 channels.

  EXPECT_EQ(supported->supported_formats[0].pcm_supported_formats().frame_rates().count(), 1);
  EXPECT_EQ(supported->supported_formats[0].pcm_supported_formats().frame_rates()[0], 96'000);
}

}  // namespace audio::as370

// Redefine PDevMakeMmioBufferWeak per the recommendation in pdev.h.
zx_status_t ddk::PDevMakeMmioBufferWeak(const pdev_mmio_t& pdev_mmio,
                                        std::optional<MmioBuffer>* mmio, uint32_t cache_policy) {
  auto* test_harness = reinterpret_cast<audio::as370::FakeMmio*>(pdev_mmio.offset);
  mmio->emplace(test_harness->mmio());
  return ZX_OK;
}
