// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_DFV2_VIRTUAL_AUDIO_DEVICE_H_
#define SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_DFV2_VIRTUAL_AUDIO_DEVICE_H_

#include <fidl/fuchsia.virtualaudio/cpp/fidl.h>
#include <lib/zx/result.h>

namespace virtual_audio {

class VirtualAudioDevice : public fidl::WireServer<fuchsia_virtualaudio::Device> {
 public:
  using OnBindingClosed = fit::callback<void(fidl::UnbindInfo)>;

  virtual std::optional<bool> is_input() const = 0;

  VirtualAudioDevice(async_dispatcher_t* dispatcher,
                     fidl::ServerEnd<fuchsia_virtualaudio::Device> server,
                     OnBindingClosed on_binding_closed)
      : dispatcher_(dispatcher),
        binding_(dispatcher_, std::move(server), this, std::move(on_binding_closed)) {}

 protected:
  // virtualaudio.Device implementation.
  void GetFormat(GetFormatCompleter::Sync& completer) override;
  void GetGain(GetGainCompleter::Sync& completer) override;
  void GetBuffer(GetBufferCompleter::Sync& completer) override;
  void SetNotificationFrequency(SetNotificationFrequencyRequestView request,
                                SetNotificationFrequencyCompleter::Sync& completer) override;
  void GetPosition(GetPositionCompleter::Sync& completer) override;
  void ChangePlugState(ChangePlugStateRequestView request,
                       ChangePlugStateCompleter::Sync& completer) override;
  void AdjustClockRate(AdjustClockRateRequestView request,
                       AdjustClockRateCompleter::Sync& completer) override;

  void NotifyFormatChanged(uint32_t frames_per_second, uint32_t sample_format,
                           uint32_t num_channels, zx_duration_t external_delay);
  void NotifySetGain(bool current_mute, bool current_agc, float current_gain_db);
  void NotifyRingBufferCreated(zx::vmo ring_buffer_vmo, uint32_t num_ring_buffer_frames,
                               uint32_t notifications_per_ring);
  void NotifyRingBufferStarted(zx_time_t start_time);
  void NotifyRingBufferStopped(zx_time_t stop_time, uint32_t ring_buffer_position);
  void NotifyRingBufferPosition(zx_time_t monotonic_time, uint32_t ring_buffer_position);

  async_dispatcher_t* dispatcher() { return dispatcher_; }

 private:
  async_dispatcher_t* dispatcher_;

  fidl::ServerBinding<fuchsia_virtualaudio::Device> binding_;
};

}  // namespace virtual_audio

#endif  // SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_DFV2_VIRTUAL_AUDIO_DEVICE_H_
