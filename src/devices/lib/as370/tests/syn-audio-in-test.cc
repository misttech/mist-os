// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/hardware/shareddma/cpp/banjo-mock.h>
#include <lib/zx/clock.h>

#include <fbl/array.h>
#include <mock-mmio-reg/mock-mmio-reg.h>
#include <soc/as370/as370-dma.h>
#include <soc/as370/as370-hw.h>
#include <soc/as370/syn-audio-in.h>
#include <zxtest/zxtest.h>

bool operator==(const shared_dma_protocol_t& a, const shared_dma_protocol_t& b) { return true; }
bool operator==(const dma_notify_callback_t& a, const dma_notify_callback_t& b) { return true; }

namespace {

class CicFilterTest : public CicFilter {
 public:
  explicit CicFilterTest() : CicFilter() {}
  uint32_t Filter(uint32_t index, void* input, uint32_t input_size, void* output,
                  uint32_t input_total_channels, uint32_t input_channel,
                  uint32_t output_total_channels, uint32_t output_channel) {
    return 4;  // mock decodes 4 bytes.
  }
};

class SynAudioInDeviceTest : public SynAudioInDevice {
 public:
  SynAudioInDeviceTest(ddk::MmioBuffer mmio_avio, ddk::MmioBuffer mmio_i2s,
                       ddk::SharedDmaProtocolClient dma)
      : SynAudioInDevice(std::move(mmio_avio), std::move(mmio_i2s), std::move(dma), nullptr) {
    cic_filter_ = std::make_unique<CicFilterTest>();
    dma_buffer_size_[0] = 0x10;
    if (kNumberOfDmas > 1) {
      dma_buffer_size_[1] = 0x20;
    }
  }
  bool HasAtLeastTwoDmas() { return kNumberOfDmas >= 2; }
};

class SynAudioInTest : public zxtest::Test {
 public:
  // in 32 bits chunks.
  static constexpr size_t kGlobalRegCount = as370::kAudioGlobalSize / sizeof(uint32_t);
  static constexpr size_t kI2sRegCount = as370::kAudioI2sSize / sizeof(uint32_t);

  void SetUp() override {
    global_region_.emplace(sizeof(uint32_t), kGlobalRegCount);
    i2s_region_.emplace(sizeof(uint32_t), kI2sRegCount);

    ddk::MmioBuffer global_buffer(global_region_->GetMmioBuffer());
    ddk::MmioBuffer i2s_buffer(i2s_region_->GetMmioBuffer());

    device_ = std::unique_ptr<SynAudioInDeviceTest>(
        new SynAudioInDeviceTest(std::move(global_buffer), std::move(i2s_buffer), dma_.GetProto()));
  }

  void TearDown() override {
    dma_.ExpectStop(DmaId::kDmaIdPdmW0);
    dma_.ExpectStop(DmaId::kDmaIdPdmW1);
    device_->Shutdown();
    dma_.VerifyAndClear();
  }

 protected:
  std::unique_ptr<SynAudioInDeviceTest>& device() { return device_; }
  ddk::MockSharedDma& dma() { return dma_; }

  void Init(uint32_t notification_size) {
    ExpectNotificationCallback(notification_size, DmaId::kDmaIdPdmW0);
    device_->Init();
  }

  void ExpectNotificationCallback(uint32_t size_per_notification, uint32_t client_id) {
    dma_notify_callback_t notify = {};
    auto notify_cb = [](void* ctx, dma_state_t state) -> void {};
    notify.callback = notify_cb;
    dma_.ExpectSetNotifyCallback(ZX_OK, client_id, notify, size_per_notification);
  }

 private:
  std::optional<ddk_mock::MockMmioRegRegion> global_region_;
  std::optional<ddk_mock::MockMmioRegRegion> i2s_region_;
  std::unique_ptr<SynAudioInDeviceTest> device_;
  ddk::MockSharedDma dma_;
};

}  // namespace

namespace audio {

TEST_F(SynAudioInTest, ShutdownWithStoppedThread) {
  Init(4);

  dma().ExpectStop(DmaId::kDmaIdPdmW0);
  dma().ExpectStop(DmaId::kDmaIdPdmW1);
  device()->Shutdown();  // Stops the thread.

  dma().ExpectStop(DmaId::kDmaIdPdmW0);
  dma().ExpectStop(DmaId::kDmaIdPdmW1);
  device()->Shutdown();  // Must still work with the thread stopped.
  // TeadDown() may Shutdown() even another time, that must work as well.
}

TEST_F(SynAudioInTest, ProcessDmaSimple) {
  Init(4);

  dma().ExpectGetBufferPosition(0x4, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0x8, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);

  device()->ProcessDma(0);
}

TEST_F(SynAudioInTest, ProcessDmaWarp) {
  Init(4);

  dma().ExpectGetBufferPosition(0x4, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0x8, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0x0, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0x4, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0x8, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);

  device()->ProcessDma(0);
}

TEST_F(SynAudioInTest, ProcessDmaIrregular) {
  Init(4);

  dma().ExpectGetBufferPosition(0x8, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);

  device()->ProcessDma(0);
}

TEST_F(SynAudioInTest, ProcessDmaOverflow) {
  Init(4);

  dma().ExpectGetBufferPosition(0x4, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0x4, DmaId::kDmaIdPdmW0);

  device()->ProcessDma(0);
}

TEST_F(SynAudioInTest, ProcessDmaPdm0AndPdm1) {
  Init(4);

  if (!device()->HasAtLeastTwoDmas()) {
    return;
  }

  dma().ExpectGetBufferPosition(0x4, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0x8, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0x0, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0x4, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0x4, DmaId::kDmaIdPdmW0);

  dma().ExpectGetBufferPosition(0x4, DmaId::kDmaIdPdmW1);
  dma().ExpectGetBufferPosition(0x8, DmaId::kDmaIdPdmW1);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW1);
  dma().ExpectGetBufferPosition(0x10, DmaId::kDmaIdPdmW1);
  dma().ExpectGetBufferPosition(0x14, DmaId::kDmaIdPdmW1);
  dma().ExpectGetBufferPosition(0x18, DmaId::kDmaIdPdmW1);
  dma().ExpectGetBufferPosition(0x1c, DmaId::kDmaIdPdmW1);
  dma().ExpectGetBufferPosition(0x0, DmaId::kDmaIdPdmW1);
  dma().ExpectGetBufferPosition(0x4, DmaId::kDmaIdPdmW1);
  dma().ExpectGetBufferPosition(0x4, DmaId::kDmaIdPdmW1);

  dma().ExpectGetBufferPosition(0x8, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);

  device()->ProcessDma(0);
  device()->ProcessDma(1);
  device()->ProcessDma(0);
}

TEST_F(SynAudioInTest, FifoDepth) {
  // 16384 PDM DMA transfer size as used for PDM, generates 1024 samples at 48KHz 16 bits.
  Init(16384);

  // 12288 = 3 channels x 1024 samples per DMA x 2 bytes per sample x 2 for ping-pong.
  ASSERT_EQ(device()->FifoDepth(), 12288);
}

TEST_F(SynAudioInTest, StartTime) {
  Init(4);

  dma().ExpectStart(DmaId::kDmaIdPdmW0);
  dma().ExpectStart(DmaId::kDmaIdPdmW1);

  uint64_t before = zx::clock::get_monotonic().get();
  uint64_t timestamp = device()->Start(48'000).value();
  uint64_t after = zx::clock::get_monotonic().get();
  EXPECT_GE(timestamp, before);
  EXPECT_LE(timestamp, after);
}

TEST_F(SynAudioInTest, StopTime) {
  Init(4);

  dma().ExpectStop(DmaId::kDmaIdPdmW0);
  dma().ExpectStop(DmaId::kDmaIdPdmW1);

  uint64_t before = zx::clock::get_monotonic().get();
  uint64_t timestamp = device()->Stop();
  uint64_t after = zx::clock::get_monotonic().get();
  EXPECT_GE(timestamp, before);
  EXPECT_LE(timestamp, after);
}

}  // namespace audio
