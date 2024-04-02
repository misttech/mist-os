// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_EXAMPLES_SIMPLE_ADR_SIMPLE_ADR_H_
#define SRC_MEDIA_AUDIO_EXAMPLES_SIMPLE_ADR_SIMPLE_ADR_H_

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fit/function.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/sys/cpp/component_context.h>

#include <string_view>

namespace examples {

class MediaApp;

template <typename ProtocolT>
class FidlHandler : public fidl::AsyncEventHandler<ProtocolT> {
 public:
  FidlHandler(MediaApp* parent, std::string_view name);
  void on_fidl_error(fidl::UnbindInfo error) final;

 private:
  MediaApp* parent_;
  std::string_view name_;
};

class MediaApp : public fidl::AsyncEventHandler<fuchsia_audio_device::Registry>,
                 public fidl::AsyncEventHandler<fuchsia_audio_device::Observer>,
                 public fidl::AsyncEventHandler<fuchsia_audio_device::ControlCreator>,
                 public fidl::AsyncEventHandler<fuchsia_audio_device::Control>,
                 public fidl::AsyncEventHandler<fuchsia_audio_device::RingBuffer> {
  // TODO(b/306455236): Use a format / rate supported by the detected device.
  static inline constexpr fuchsia_audio::SampleType kSampleFormat =
      fuchsia_audio::SampleType::kInt16;
  static inline constexpr uint16_t kBytesPerSample = 2;
  static inline constexpr float kToneAmplitude = 0.125f;

  static inline constexpr uint32_t kFrameRate = 48000;
  static inline constexpr float kApproxToneFrequency = 240.0f;
  static inline constexpr float kApproxFramesPerCycle = kFrameRate / kApproxToneFrequency;

 public:
  MediaApp(async::Loop& loop, fit::closure quit_callback);

  void Run();
  void Shutdown();

 private:
  void ConnectToRegistry();
  void WaitForFirstAudioOutput();

  void ObserveDevice();

  void ConnectToControlCreator();
  void ControlDevice();

  void CreateRingBufferConnection();

  bool MapRingBufferVmo();
  void WriteAudioToVmo();
  void StartRingBuffer();

  void ChangeGainByDbAfter(float change_db, zx::duration wait_duration, int32_t iterations);
  void StopRingBuffer();

  async::Loop& loop_;
  fit::closure quit_callback_;

  fidl::Client<fuchsia_audio_device::Registry> registry_client_;
  fidl::Client<fuchsia_audio_device::Observer> observer_client_;
  fidl::Client<fuchsia_audio_device::ControlCreator> control_creator_client_;
  fidl::Client<fuchsia_audio_device::Control> control_client_;
  fidl::Client<fuchsia_audio_device::RingBuffer> ring_buffer_client_;

  fuchsia_audio_device::TokenId device_token_id_;
  fuchsia_audio::RingBuffer ring_buffer_;
  uint64_t ring_buffer_size_;  // From fuchsia.mem.Buffer/size and kBytesPerFrame
  fzl::VmoMapper ring_buffer_mapper_;
  float max_gain_db_;
  float min_gain_db_;
  int16_t* rb_start_;
  size_t channels_per_frame_ = 0;

  FidlHandler<fuchsia_audio_device::Registry> reg_handler_{this, "Registry"};
  FidlHandler<fuchsia_audio_device::Observer> obs_handler_{this, "Observer"};
  FidlHandler<fuchsia_audio_device::ControlCreator> ctl_crtr_handler_{this, "ControlCreator"};
  FidlHandler<fuchsia_audio_device::Control> ctl_handler_{this, "Control"};
  FidlHandler<fuchsia_audio_device::RingBuffer> rb_handler_{this, "RingBuffer"};
};

}  // namespace examples

#endif  // SRC_MEDIA_AUDIO_EXAMPLES_SIMPLE_ADR_SIMPLE_ADR_H_
