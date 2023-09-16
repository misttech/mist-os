// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_AML_DSP_AML_G12_PDM_DSP_AUDIO_STREAM_IN_H_
#define SRC_MEDIA_AUDIO_DRIVERS_AML_DSP_AML_G12_PDM_DSP_AUDIO_STREAM_IN_H_

#include <fidl/fuchsia.hardware.dsp/cpp/wire.h>
#include <fidl/fuchsia.hardware.mailbox/cpp/wire.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/fzl/pinned-vmo.h>
#include <lib/simple-audio-stream/simple-audio-stream.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/bti.h>
#include <lib/zx/vmo.h>

#include <memory>
#include <optional>

#include <audio-proto/audio-proto.h>
#include <ddktl/device.h>
#include <soc/aml-common/aml-pdm-audio.h>

#include "src/media/audio/drivers/lib/aml-dsp/dsp.h"

namespace audio::aml_g12 {

class AudioStreamInDsp : public SimpleAudioStream {
 public:
  static zx_status_t Create(void* ctx, zx_device_t* parent);

 protected:
  zx_status_t Init() __TA_REQUIRES(domain_token()) override;
  zx_status_t ChangeFormat(const audio_proto::StreamSetFmtReq& req)
      __TA_REQUIRES(domain_token()) override;
  zx_status_t GetBuffer(const audio_proto::RingBufGetBufferReq& req, uint32_t* out_num_rb_frames,
                        zx::vmo* out_buffer) __TA_REQUIRES(domain_token()) override;
  zx_status_t Start(uint64_t* out_start_time) __TA_REQUIRES(domain_token()) override;
  zx_status_t Stop() __TA_REQUIRES(domain_token()) override;
  zx_status_t SetGain(const audio_proto::SetGainReq& req) override;
  zx_status_t ChangeActiveChannels(uint64_t mask) __TA_REQUIRES(domain_token()) override {
    return ZX_ERR_NOT_SUPPORTED;
  }
  void RingBufferShutdown() TA_REQ(domain_token()) override;
  void ShutdownHook() __TA_REQUIRES(domain_token()) override;
  explicit AudioStreamInDsp(zx_device_t* parent);
  std::unique_ptr<AmlMailboxDevice> audio_mailbox_;
  std::unique_ptr<AmlDspDevice> audio_dsp_;

 private:
  friend class fbl::RefPtr<AudioStreamInDsp>;
  friend class SimpleAudioStream;

  zx_status_t AddFormats() TA_REQ(domain_token());
  zx_status_t InitPDev() TA_REQ(domain_token());
  void InitHw();
  void ProcessRingNotification();
  void RingNotificationReport();
  virtual bool AllowNonContiguousRingBuffer() { return false; }
  int Thread();
  int MailboxBind();
  int DspBind();

  zx::duration notification_rate_ = {};
  uint32_t frames_per_second_ = 0;
  async::TaskClosureMethod<AudioStreamInDsp, &AudioStreamInDsp::ProcessRingNotification>
      notify_timer_ __TA_GUARDED(domain_token()){this};
  // Inform DSP FW of ring buffer location information regularly
  async::TaskClosureMethod<AudioStreamInDsp, &AudioStreamInDsp::RingNotificationReport>
      position_timer_ __TA_GUARDED(domain_token()){this};

  std::unique_ptr<AmlPdmDevice> lib_;
  zx::bti bti_;
  metadata::AmlPdmConfig metadata_ = {};

  zx::interrupt irq_;
  std::atomic<bool> running_ = false;
  thrd_t thread_ = {};
  inspect::IntProperty status_time_;
  inspect::UintProperty dma_status_;
  inspect::UintProperty pdm_status_;
  inspect::UintProperty ring_buffer_physical_address_;
  size_t ring_buffer_size_ = 0;
  std::optional<fdf::MmioBuffer> dsp_mmio_;
};
}  // namespace audio::aml_g12

#endif  // SRC_MEDIA_AUDIO_DRIVERS_AML_DSP_AML_G12_PDM_DSP_AUDIO_STREAM_IN_H_
