// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_VALIDATE_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_VALIDATE_H_

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>

namespace media_audio {

// TODO(https://fxbug.dev/42068183): official frame-rate limits/expectations for audio devices.
constexpr uint32_t kMinSupportedDaiFrameRate = 1000;
constexpr uint32_t kMaxSupportedDaiFrameRate = 192000 * 8 * 64;
constexpr uint8_t kMaxSupportedDaiFormatBitsPerSlot = 64;

// We define these here only temporarily, as we do not publish frame-rate limits for audio devices.
// TODO(https://fxbug.dev/42068183): official frame-rate limits/expectations for audio devices.
const uint32_t kMinSupportedRingBufferFrameRate = 1000;
const uint32_t kMaxSupportedRingBufferFrameRate = 192000;

// Utility functions to validate direct responses from audio drivers.
std::vector<fuchsia_audio_device::PcmFormatSet> TranslateFormatSets(
    std::vector<fuchsia_hardware_audio::SupportedFormats>& formats);

zx_status_t ValidateStreamProperties(
    const fuchsia_hardware_audio::StreamProperties& props,
    std::optional<const fuchsia_hardware_audio::GainState> gain_state = std::nullopt,
    std::optional<const fuchsia_hardware_audio::PlugState> plug_state = std::nullopt);
zx_status_t ValidateSupportedFormats(
    const std::vector<fuchsia_hardware_audio::SupportedFormats>& formats);
zx_status_t ValidateGainState(
    const fuchsia_hardware_audio::GainState& gain_state,
    std::optional<const fuchsia_hardware_audio::StreamProperties> stream_properties = std::nullopt);
zx_status_t ValidatePlugState(const fuchsia_hardware_audio::PlugState& plug_state,
                              std::optional<fuchsia_hardware_audio::PlugDetectCapabilities>
                                  plug_detect_capabilities = std::nullopt);

zx_status_t ValidateCodecProperties(
    const fuchsia_hardware_audio::CodecProperties& props,
    std::optional<const fuchsia_hardware_audio::PlugState> plug_state = std::nullopt);
zx_status_t ValidateDaiFormatSets(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats);
zx_status_t ValidateDaiFormat(const fuchsia_hardware_audio::DaiFormat& formats);
zx_status_t ValidateCodecFormatInfo(const fuchsia_hardware_audio::CodecFormatInfo& format_info);

bool ValidateDeviceInfo(const fuchsia_audio_device::Info& device_info);

zx_status_t ValidateRingBufferProperties(
    const fuchsia_hardware_audio::RingBufferProperties& rb_props);
zx_status_t ValidateRingBufferFormat(const fuchsia_hardware_audio::Format& format);
zx_status_t ValidateFormatCompatibility(uint8_t bytes_per_sample,
                                        fuchsia_hardware_audio::SampleFormat sample_format);
zx_status_t ValidateRingBufferVmo(const zx::vmo& vmo, uint32_t num_frames,
                                  const fuchsia_hardware_audio::Format& format);
zx_status_t ValidateDelayInfo(const fuchsia_hardware_audio::DelayInfo& delay_info);

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_VALIDATE_H_
