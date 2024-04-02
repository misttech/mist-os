// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/device_watcher.h"

#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.audio.device/cpp/type_conversions.h>
#include <fidl/fuchsia.audio/cpp/natural_types.h>
#include <fidl/fuchsia.audio/cpp/type_conversions.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/compiler.h>

#include "src/media/audio/audio_core/shared/select_best_format.h"

namespace media_audio {

namespace {

#define USE_AUDIO_DEVICE_REGISTRY_DEVICES (1)

template <typename ResultT>
bool LogResultError(const ResultT& result, const char* debug_context) {
  if (!result.ok()) {
    FX_LOGS(WARNING) << debug_context << ": failed with transport error: " << result;
    return true;
  }
  if (!result->is_ok()) {
    FX_LOGS(ERROR) << debug_context
                   << ": failed with code: " << fidl::ToUnderlying(result->error_value());
    return true;
  }
  return false;
}

#if (USE_AUDIO_DEVICE_REGISTRY_DEVICES)
audio_stream_unique_id_t DeviceUniqueId(fuchsia_audio_device::wire::Info info) {
  audio_stream_unique_id_t unique_id{0};
  if (info.has_unique_instance_id()) {
    std::memcpy(unique_id.data, info.unique_instance_id().data(), sizeof(unique_id.data));
  }
  return unique_id;
}
#endif  // USE_AUDIO_DEVICE_REGISTRY_DEVICES

}  // namespace

DeviceWatcher::DeviceWatcher(Args args)
    : graph_client_(std::move(args.graph_client)),
      registry_client_(fidl::WireSharedClient(std::move(args.registry_client), args.dispatcher)),
      control_creator_client_(
          fidl::WireSharedClient(std::move(args.control_creator_client), args.dispatcher)),
      dispatcher_(args.dispatcher),
      output_thread_(args.output_thread),
      input_thread_(args.input_thread),
      output_thread_profile_(args.output_thread_profile),
      input_thread_profile_(args.input_thread_profile),
      route_graph_(std::move(args.route_graph)),
      effects_loader_(std::move(args.effects_loader)) {
  WatchDevicesAdded();
  WatchDevicesRemoved();
}

void DeviceWatcher::WatchDevicesAdded() {
  registry_client_->WatchDevicesAdded().Then([this, self = shared_from_this()](auto& result) {
    if (!LogResultError(result, "WatchDevicesAdded")) {
      return;
    }
    if (!result->value()->has_devices()) {
      FX_LOGS(ERROR) << "WatchDevicesAdded bug: response missing `devices`";
      return;
    }
    for (auto device : result->value()->devices()) {
      AddDevice(device);
    }
    WatchDevicesAdded();
  });
}

void DeviceWatcher::WatchDevicesRemoved() {
  registry_client_->WatchDeviceRemoved().Then([this, self = shared_from_this()](auto& result) {
    if (!LogResultError(result, "WatchDeviceRemoved")) {
      return;
    }
    if (!result->value()->has_token_id()) {
      FX_LOGS(ERROR) << "WatchDeviceRemoved bug: response missing `devices`";
      return;
    }
    RemoveDevice(result->value()->token_id());
    WatchDevicesRemoved();
  });
}

void DeviceWatcher::AddDevice(fuchsia_audio_device::wire::Info info) {
  // Ignore invalid device infos.
  if (!info.has_token_id() || !info.has_device_type() || !info.has_device_name() ||
      !info.has_ring_buffer_format_sets() || info.ring_buffer_format_sets().empty() ||
      !info.has_gain_caps() || !info.has_plug_detect_caps() || !info.has_clock_domain()) {
    FX_LOGS(ERROR) << "fuchsia.audio.device.Info missing required field";
    return;
  }
#if USE_AUDIO_DEVICE_REGISTRY_DEVICES
  for (const auto& format_sets_for_element : info.ring_buffer_format_sets()) {
    if (!format_sets_for_element.has_element_id() || !format_sets_for_element.has_format_sets() ||
        format_sets_for_element.format_sets().empty()) {
      FX_LOGS(ERROR) << "fuchsia.audio.device.ElementRingBufferFormatSet missing required field";
      return;
    }
    for (auto format : format_sets_for_element.format_sets()) {
      if (!format.has_channel_sets() || format.channel_sets().empty() ||
          !format.has_sample_types() || format.sample_types().empty() ||
          !format.has_frame_rates() || format.frame_rates().empty()) {
        FX_LOGS(ERROR) << "fuchsia.audio.device.PcmFormatSet missing required field";
        return;
      }
      for (auto channel_set : format.channel_sets()) {
        if (!channel_set.has_attributes() || channel_set.attributes().empty()) {
          FX_LOGS(ERROR) << "fuchsia.audio.device.ChannelSet missing required field";
          return;
        }
      }
    }
  }
#endif  // USE_AUDIO_DEVICE_REGISTRY_DEVICES
  if (!info.gain_caps().has_min_gain_db() || !info.gain_caps().has_max_gain_db() ||
      !info.gain_caps().has_gain_step_db()) {
    FX_LOGS(ERROR) << "fuchsia.audio.device.GainCapabilities missing required field";
    return;
  }

  // Duplicates should not happen: this is a bug.
  if (devices_.count(info.token_id()) > 0) {
    FX_LOGS(ERROR) << "device with token_id '" << info.token_id() << "' already exists";
    return;
  }

  if (info.device_type() != fuchsia_audio_device::DeviceType::kInput &&
      info.device_type() != fuchsia_audio_device::DeviceType::kOutput) {
    FX_LOGS(WARNING) << "ignoring device with unsupported device_type '"
                     << fidl::ToUnderlying(info.device_type()) << "'";
    return;
  }

  // Create the Device.
  auto observer_endpoints = fidl::CreateEndpoints<fuchsia_audio_device::Observer>();
  if (!observer_endpoints.is_ok()) {
    FX_PLOGS(ERROR, observer_endpoints.status_value()) << "fidl::CreateEndpoints failed";
    return;
  }

  auto control_endpoints = fidl::CreateEndpoints<fuchsia_audio_device::Control>();
  if (!control_endpoints.is_ok()) {
    FX_PLOGS(ERROR, control_endpoints.status_value()) << "fidl::CreateEndpoints failed";
    return;
  }

  fidl::Arena<> arena;
  registry_client_
      ->CreateObserver(fuchsia_audio_device::wire::RegistryCreateObserverRequest::Builder(arena)
                           .token_id(info.token_id())
                           .observer_server(std::move(observer_endpoints->server))
                           .Build())
      .Then([this, self = shared_from_this(), token_id = info.token_id()](auto& result) {
        if (!LogResultError(result, "CreateObserver")) {
          RemoveDevice(token_id);
        }
      });

  control_creator_client_
      ->Create(fuchsia_audio_device::wire::ControlCreatorCreateRequest::Builder(arena)
                   .token_id(info.token_id())
                   .control_server(std::move(control_endpoints->server))
                   .Build())
      .Then([this, self = shared_from_this(), token_id = info.token_id()](auto& result) {
        if (!LogResultError(result, "CreateControl")) {
          RemoveDevice(token_id);
        }
      });

  switch (info.device_type()) {
    case fuchsia_audio_device::DeviceType::kInput:
      AddInputDevice(info, std::move(observer_endpoints->client),
                     std::move(control_endpoints->client));
      break;
    case fuchsia_audio_device::DeviceType::kOutput:
      AddOutputDevice(info, std::move(observer_endpoints->client),
                      std::move(control_endpoints->client));
      break;
    default:
      __UNREACHABLE;
      break;
  }
}

void DeviceWatcher::AddOutputDevice(fuchsia_audio_device::wire::Info info,
                                    fidl::ClientEnd<fuchsia_audio_device::Observer> observer_client,
                                    fidl::ClientEnd<fuchsia_audio_device::Control> control_client) {
  FX_CHECK(info.device_type() == fuchsia_audio_device::DeviceType::kOutput);

#if (USE_AUDIO_DEVICE_REGISTRY_DEVICES)
  auto profile = config_.output_device_profile(DeviceUniqueId(info));

  // The pipeline's preferred format, converted from media::audio::Format to media_audio::Format.
  const auto pref_format_old = profile.pipeline_config().OutputFormat(effects_loader_.get());
  const auto pref_format = Format::CreateLegacyOrDie(fuchsia_media::wire::AudioStreamType{
      .sample_format =
          static_cast<fuchsia_media::AudioSampleFormat>(pref_format_old.sample_format()),
      .channels = pref_format_old.stream_type().channels,
      .frames_per_second = pref_format_old.stream_type().frames_per_second,
  });

  // Select the format to use for this device.
  const auto format = media::audio::SelectBestFormat(
      *fidl::ToNatural(info.ring_buffer_format_sets()
                           .at(fuchsia_audio_device::kDefaultRingBufferElementId)
                           .format_sets()),
      pref_format);
  if (!format.is_ok()) {
    FX_LOGS(WARNING) << "output device with token_id '" << info.token_id()
                     << "' cannot select a format given the pipeline format '" << pref_format
                     << "'";
    return;
  }

  // If the selected rate or channelization differs from our pipeline's config, then:
  // If the root pipeline stage has effects, the effects may not be compatible with the selected
  // format, so fail; otherwise, adjust the root pipeline state to match the selected format.
  if (format->frames_per_second() != pref_format.frames_per_second() ||
      format->channels() != pref_format.channels()) {
    if (profile.pipeline_config().root().effects_v2) {
      FX_LOGS(WARNING) << "output device with token_id '" << info.token_id()
                       << "' cannot use selected format '" << *format << "' with pipeline format '"
                       << pref_format << "'";
      return;
    }

    media::audio::PipelineConfig pipeline_config = profile.pipeline_config();
    pipeline_config.mutable_root().output_rate = static_cast<int32_t>(format->frames_per_second());
    pipeline_config.mutable_root().output_channels = static_cast<int16_t>(format->channels());

    profile = media::audio::DeviceConfig::OutputDeviceProfile(
        profile.eligible_for_loopback(), profile.supported_usages(), profile.volume_curve(),
        profile.independent_volume_control(), pipeline_config, profile.driver_gain_db(),
        profile.software_gain_db());
  }

  // Compute how many "producer bytes" we need for this device's ring buffer.
  // These constants match ../v1/driver_output.cc.
  constexpr auto kDefaultMaxRetentionNsec = zx::msec(60);
  constexpr auto kDefaultRetentionGapNsec = zx::msec(10);
  // This formula matches ../v1/driver_output.cc.
  const auto high_water_duration = 2 * output_thread_profile_.period;
  const auto producer_duration =
      high_water_duration + kDefaultMaxRetentionNsec + kDefaultRetentionGapNsec;

  devices_[info.token_id()] = Device::Create({
      .graph_client = graph_client_,
      .observer_client = std::move(observer_client),
      .control_client = std::move(control_client),
      .dispatcher = dispatcher_,
      .info = info,
      .format = *format,
      .thread = output_thread_,
      .config = std::move(profile),
      .min_ring_buffer_bytes = format->bytes_per(producer_duration),
      .route_graph = route_graph_,
      .effects_loader = effects_loader_,
  });
#else   // USE_AUDIO_DEVICE_REGISTRY_DEVICES
  FX_LOGS(WARNING) << "For now, AudioCoreV2 is not connecting to devices";
  return;
#endif  // USE_AUDIO_DEVICE_REGISTRY_DEVICES
}

void DeviceWatcher::AddInputDevice(fuchsia_audio_device::wire::Info info,
                                   fidl::ClientEnd<fuchsia_audio_device::Observer> observer_client,
                                   fidl::ClientEnd<fuchsia_audio_device::Control> control_client) {
  FX_CHECK(info.device_type() == fuchsia_audio_device::DeviceType::kInput);

#if (USE_AUDIO_DEVICE_REGISTRY_DEVICES)
  auto profile = config_.input_device_profile(DeviceUniqueId(info));

  // Select the format to use for this device.
  const auto pref_format = Format::CreateOrDie({
      .sample_type = fuchsia_audio::SampleType::kInt16,
      .channels = 1,
      .frames_per_second = profile.rate(),
  });
  const auto format = media::audio::SelectBestFormat(
      *fidl::ToNatural(info.ring_buffer_format_sets()
                           .at(fuchsia_audio_device::kDefaultRingBufferElementId)
                           .format_sets()),
      pref_format);
  if (!format.is_ok()) {
    FX_LOGS(WARNING) << "input device with token_id '" << info.token_id()
                     << "' cannot select a format given the pipeline format '" << pref_format
                     << "'";
    return;
  }

  // Compute how many "consumer bytes" we need for this device's ring buffer.
  // These constants match ../v1/audio_input.cc.
  constexpr zx::duration kMinFenceDistance = zx::msec(200);
  constexpr zx::duration kMaxFenceDistance = kMinFenceDistance + zx::msec(20);

  devices_[info.token_id()] = Device::Create({
      .graph_client = graph_client_,
      .observer_client = std::move(observer_client),
      .control_client = std::move(control_client),
      .dispatcher = dispatcher_,
      .info = info,
      .format = *format,
      .thread = input_thread_,
      .config = std::move(profile),
      .min_ring_buffer_bytes = format->bytes_per(kMaxFenceDistance),
      .route_graph = route_graph_,
      .effects_loader = effects_loader_,
  });
#else   // USE_AUDIO_DEVICE_REGISTRY_DEVICES
  FX_LOGS(WARNING) << "For now, AudioCoreV2 is not connecting to devices";
  return;
#endif  // USE_AUDIO_DEVICE_REGISTRY_DEVICES
}

void DeviceWatcher::RemoveDevice(TokenId token_id) {
  auto it = devices_.find(token_id);
  if (it == devices_.end()) {
    FX_LOGS(WARNING) << "device with token_id '" << token_id << "' does not exist";
    return;
  }

  it->second->Destroy();
  devices_.erase(it);
}

}  // namespace media_audio
