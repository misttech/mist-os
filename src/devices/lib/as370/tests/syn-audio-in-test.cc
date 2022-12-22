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
                  uint32_t output_total_channels, uint32_t output_channel,
                  uint32_t multiplier_shift) {
    return 4;  // mock decodes 4 bytes.
  }
};

class SynAudioInDeviceTest : public SynAudioInDevice {
 public:
  SynAudioInDeviceTest(ddk::MmioBuffer mmio_avio, ddk::MmioBuffer mmio_i2s,
                       ddk::SharedDmaProtocolClient dma)
      : SynAudioInDevice(std::move(mmio_avio), std::move(mmio_i2s), std::move(dma)) {
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
    global_mocks_ = fbl::Array(new ddk_mock::MockMmioReg[kGlobalRegCount], kGlobalRegCount);
    i2s_mocks_ = fbl::Array(new ddk_mock::MockMmioReg[kI2sRegCount], kI2sRegCount);
    global_region_.emplace(global_mocks_.data(), sizeof(uint32_t), kGlobalRegCount);
    i2s_region_.emplace(i2s_mocks_.data(), sizeof(uint32_t), kI2sRegCount);

    ddk::MmioBuffer global_buffer(global_region_->GetMmioBuffer());
    ddk::MmioBuffer i2s_buffer(i2s_region_->GetMmioBuffer());

    device_ = std::unique_ptr<SynAudioInDeviceTest>(
        new SynAudioInDeviceTest(std::move(global_buffer), std::move(i2s_buffer), dma_.GetProto()));

    dma_notify_callback_t notify = {};
    auto callback = [](void* ctx, dma_state_t state) -> void {};
    notify.callback = callback;
    dma_.ExpectSetNotifyCallback(ZX_OK, DmaId::kDmaIdPdmW0, notify);
    device_->Init();
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

 private:
  fbl::Array<ddk_mock::MockMmioReg> global_mocks_;
  fbl::Array<ddk_mock::MockMmioReg> i2s_mocks_;
  std::optional<ddk_mock::MockMmioRegRegion> global_region_;
  std::optional<ddk_mock::MockMmioRegRegion> i2s_region_;
  std::unique_ptr<SynAudioInDeviceTest> device_;
  ddk::MockSharedDma dma_;
};

}  // namespace

namespace audio {

TEST_F(SynAudioInTest, ProcessDmaSimple) {
  dma().ExpectGetTransferSize(4, DmaId::kDmaIdPdmW0);

  dma().ExpectGetBufferPosition(0x4, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0x8, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);

  device()->ProcessDma(0);
}

TEST_F(SynAudioInTest, ProcessDmaWarp) {
  dma().ExpectGetTransferSize(4, DmaId::kDmaIdPdmW0);

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
  dma().ExpectGetTransferSize(4, DmaId::kDmaIdPdmW0);

  dma().ExpectGetBufferPosition(0x8, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);

  device()->ProcessDma(0);
}

TEST_F(SynAudioInTest, ProcessDmaOverflow) {
  dma().ExpectGetTransferSize(4, DmaId::kDmaIdPdmW0);

  dma().ExpectGetBufferPosition(0x4, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0xc, DmaId::kDmaIdPdmW0);
  dma().ExpectGetBufferPosition(0x4, DmaId::kDmaIdPdmW0);

  device()->ProcessDma(0);
}

TEST_F(SynAudioInTest, ProcessDmaPdm0AndPdm1) {
  if (!device()->HasAtLeastTwoDmas()) {
    return;
  }

  // every call to ProcessDma gets transfer size from PDM0.
  dma().ExpectGetTransferSize(4, DmaId::kDmaIdPdmW0);
  dma().ExpectGetTransferSize(4, DmaId::kDmaIdPdmW0);
  dma().ExpectGetTransferSize(4, DmaId::kDmaIdPdmW0);

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
  dma().ExpectGetTransferSize(16384, DmaId::kDmaIdPdmW0);

  // 12288 = 3 channels x 1024 samples per DMA x 2 bytes per sample x 2 for ping-pong.
  ASSERT_EQ(device()->FifoDepth(), 12288);
}

TEST_F(SynAudioInTest, StartTime) {
  dma().ExpectStart(DmaId::kDmaIdPdmW0);
  dma().ExpectStart(DmaId::kDmaIdPdmW1);

  uint64_t before = zx::clock::get_monotonic().get();
  uint64_t timestamp = device()->Start();
  uint64_t after = zx::clock::get_monotonic().get();
  EXPECT_GE(timestamp, before);
  EXPECT_LE(timestamp, after);
}

TEST_F(SynAudioInTest, StopTime) {
  dma().ExpectStop(DmaId::kDmaIdPdmW0);
  dma().ExpectStop(DmaId::kDmaIdPdmW1);

  uint64_t before = zx::clock::get_monotonic().get();
  uint64_t timestamp = device()->Stop();
  uint64_t after = zx::clock::get_monotonic().get();
  EXPECT_GE(timestamp, before);
  EXPECT_LE(timestamp, after);
}

}  // namespace audio
