// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/media/audio/drivers/virtual-audio-dfv2/virtual-audio-device.h"

#include <lib/driver/logging/cpp/logger.h>

namespace virtual_audio {

void VirtualAudioDevice::NotifyFormatChanged(uint32_t frames_per_second, uint32_t sample_format,
                                             uint32_t num_channels, zx_duration_t external_delay) {
  fidl::Status status = fidl::WireSendEvent(binding_)->OnSetFormat(frames_per_second, sample_format,
                                                                   num_channels, external_delay);
  if (status.status() != ZX_OK) {
    FDF_LOG(WARNING, "OnSetFormat failed with status %s", status.status_string());
  }
}

void VirtualAudioDevice::NotifySetGain(bool current_mute, bool current_agc, float current_gain_db) {
  fidl::Status status =
      fidl::WireSendEvent(binding_)->OnSetGain(current_mute, current_agc, current_gain_db);
  if (status.status() != ZX_OK) {
    FDF_LOG(WARNING, "OnSetGain failed with status %s", status.status_string());
  }
}

void VirtualAudioDevice::NotifyRingBufferCreated(zx::vmo ring_buffer_vmo,
                                                 uint32_t num_ring_buffer_frames,
                                                 uint32_t notifications_per_ring) {
  fidl::Status status = fidl::WireSendEvent(binding_)->OnBufferCreated(
      std::move(ring_buffer_vmo), num_ring_buffer_frames, notifications_per_ring);
  if (status.status() != ZX_OK) {
    FDF_LOG(WARNING, "OnBufferCreated failed with status %s", status.status_string());
  }
}

void VirtualAudioDevice::NotifyRingBufferStarted(zx_time_t start_time) {
  fidl::Status status = fidl::WireSendEvent(binding_)->OnStart(start_time);
  if (status.status() != ZX_OK) {
    FDF_LOG(WARNING, "OnStart failed with status %s", status.status_string());
  }
}

void VirtualAudioDevice::NotifyRingBufferStopped(zx_time_t stop_time,
                                                 uint32_t ring_buffer_position) {
  fidl::Status status = fidl::WireSendEvent(binding_)->OnStop(stop_time, ring_buffer_position);
  if (status.status() != ZX_OK) {
    FDF_LOG(WARNING, "OnStop failed with status %s", status.status_string());
  }
}

void VirtualAudioDevice::NotifyRingBufferPosition(zx_time_t monotonic_time,
                                                  uint32_t ring_buffer_position) {
  fidl::Status status =
      fidl::WireSendEvent(binding_)->OnPositionNotify(monotonic_time, ring_buffer_position);
  if (status.status() != ZX_OK) {
    FDF_LOG(WARNING, "OnPositionNotify failed with status %s", status.status_string());
  }
}

void VirtualAudioDevice::GetFormat(GetFormatCompleter::Sync& completer) {
  completer.ReplyError(fuchsia_virtualaudio::Error::kNotSupported);
}

void VirtualAudioDevice::GetGain(GetGainCompleter::Sync& completer) {
  completer.ReplyError(fuchsia_virtualaudio::Error::kNotSupported);
}

void VirtualAudioDevice::GetBuffer(GetBufferCompleter::Sync& completer) {
  completer.ReplyError(fuchsia_virtualaudio::Error::kNotSupported);
}

void VirtualAudioDevice::SetNotificationFrequency(
    SetNotificationFrequencyRequestView request,
    SetNotificationFrequencyCompleter::Sync& completer) {
  completer.ReplyError(fuchsia_virtualaudio::Error::kNotSupported);
}

void VirtualAudioDevice::GetPosition(GetPositionCompleter::Sync& completer) {
  completer.ReplyError(fuchsia_virtualaudio::Error::kNotSupported);
}

void VirtualAudioDevice::ChangePlugState(ChangePlugStateRequestView request,
                                         ChangePlugStateCompleter::Sync& completer) {
  completer.ReplyError(fuchsia_virtualaudio::Error::kNotSupported);
}

void VirtualAudioDevice::AdjustClockRate(AdjustClockRateRequestView request,
                                         AdjustClockRateCompleter::Sync& completer) {
  completer.ReplyError(fuchsia_virtualaudio::Error::kNotSupported);
}

}  // namespace virtual_audio
