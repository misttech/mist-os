// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/media/audio/drivers/virtual_audio/virtual_audio_dai.h"

#include <lib/ddk/debug.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <audio-proto-utils/format-utils.h>
#include <fbl/algorithm.h>

namespace virtual_audio {

// static
int VirtualAudioDai::instance_count_ = 0;

fit::result<VirtualAudioDai::ErrorT, CurrentFormat> VirtualAudioDai::GetFormatForVA() {
  if (!ring_buffer_format_.has_value() || !ring_buffer_format_->pcm_format().has_value()) {
    zxlogf(WARNING, "%p ring buffer not initialized yet", this);
    return fit::error(ErrorT::kNoRingBuffer);
  }
  auto& pcm_format = ring_buffer_format_->pcm_format();
  return fit::ok(CurrentFormat{
      .frames_per_second = pcm_format->frame_rate(),
      .sample_format = audio::utils::GetSampleFormat(pcm_format->valid_bits_per_sample(),
                                                     pcm_format->bytes_per_sample() * 8),
      .num_channels = pcm_format->number_of_channels(),
      .external_delay = config_.external_delay,
  });
}

fit::result<VirtualAudioDai::ErrorT, CurrentBuffer> VirtualAudioDai::GetBufferForVA() {
  if (!ring_buffer_vmo_.is_valid()) {
    zxlogf(WARNING, "%p ring buffer not initialized yet", this);
    return fit::error(ErrorT::kNoRingBuffer);
  }

  zx::vmo dup_vmo;
  zx_status_t status = ring_buffer_vmo_.duplicate(
      ZX_RIGHT_TRANSFER | ZX_RIGHT_READ | ZX_RIGHT_WRITE | ZX_RIGHT_MAP, &dup_vmo);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%p ring buffer creation failed: %s", this, zx_status_get_string(status));
    return fit::error(ErrorT::kInternal);
  }

  return fit::ok(CurrentBuffer{
      .vmo = std::move(dup_vmo),
      .num_frames = num_ring_buffer_frames_,
      .notifications_per_ring = notifications_per_ring_,
  });
}

void VirtualAudioDai::GetProperties(
    fidl::Server<fuchsia_hardware_audio::Dai>::GetPropertiesCompleter::Sync& completer) {
  fidl::Arena arena;
  fuchsia_hardware_audio::DaiProperties properties;
  properties.is_input(config_.is_input)
      .clock_domain(config_.clock.domain)
      .unique_id(config_.unique_id)
      .product_name(config_.product_name)
      .manufacturer(config_.manufacturer_name);
  completer.Reply(std::move(properties));
}

void VirtualAudioDai::GetDaiFormats(GetDaiFormatsCompleter::Sync& completer) {
  completer.Reply(zx::ok(std::vector{config_.dai_supported_formats}));
}

void VirtualAudioDai::GetRingBufferFormats(GetRingBufferFormatsCompleter::Sync& completer) {
  std::vector<fuchsia_hardware_audio::SupportedFormats> all_formats;
  for (auto& formats : config_.supported_formats) {
    fuchsia_hardware_audio::PcmSupportedFormats pcm_formats;
    std::vector<fuchsia_hardware_audio::ChannelSet> channel_sets;
    for (uint8_t number_of_channels = formats.min_channels;
         number_of_channels <= formats.max_channels; ++number_of_channels) {
      // Vector with number_of_channels empty attributes.
      std::vector<fuchsia_hardware_audio::ChannelAttributes> attributes(number_of_channels);
      fuchsia_hardware_audio::ChannelSet channel_set;
      channel_set.attributes(std::move(attributes));
      channel_sets.push_back(std::move(channel_set));
    }
    pcm_formats.channel_sets(std::move(channel_sets));

    std::vector<uint32_t> frame_rates;
    audio::utils::FrameRateEnumerator enumerator(formats);
    for (uint32_t frame_rate : enumerator) {
      frame_rates.push_back(frame_rate);
    }
    pcm_formats.frame_rates(std::move(frame_rates));

    std::vector<audio::utils::Format> formats2 =
        audio::utils::GetAllFormats(formats.sample_formats);
    for (audio::utils::Format& format : formats2) {
      std::vector<fuchsia_hardware_audio::SampleFormat> sample_formats{format.format};
      std::vector<uint8_t> bytes_per_sample{format.bytes_per_sample};
      std::vector<uint8_t> valid_bits_per_sample{format.valid_bits_per_sample};
      auto pcm_formats2 = pcm_formats;
      pcm_formats2.sample_formats(std::move(sample_formats));
      pcm_formats2.bytes_per_sample(std::move(bytes_per_sample));
      pcm_formats2.valid_bits_per_sample(std::move(valid_bits_per_sample));
      fuchsia_hardware_audio::SupportedFormats formats_entry;
      formats_entry.pcm_supported_formats(std::move(pcm_formats2));
      all_formats.push_back(std::move(formats_entry));
    }
  }
  completer.Reply(zx::ok(std::move(all_formats)));
}

void VirtualAudioDai::CreateRingBuffer(CreateRingBufferRequest& request,
                                       CreateRingBufferCompleter::Sync& completer) {
  ring_buffer_format_.emplace(request.ring_buffer_format());
  dai_format_.emplace(request.dai_format());
  fidl::OnUnboundFn<fidl::Server<fuchsia_hardware_audio::RingBuffer>> on_unbound =
      [this](fidl::Server<fuchsia_hardware_audio::RingBuffer>*, fidl::UnbindInfo info,
             fidl::ServerEnd<fuchsia_hardware_audio::RingBuffer>) {
        // Do not log canceled cases which happens too often in particular in test cases.
        if (info.status() != ZX_ERR_CANCELED) {
          zxlogf(INFO, "Ring buffer channel closing: %s", info.FormatDescription().c_str());
        }
        ResetRingBuffer();
      };
  fidl::BindServer(dispatcher(), std::move(request.ring_buffer()), this, std::move(on_unbound));
}

void VirtualAudioDai::ResetRingBuffer() {
  watch_delay_replied_ = 0;
  watch_position_replied_ = false;
  ring_buffer_vmo_fetched_ = false;
  ring_buffer_started_ = false;
  notifications_per_ring_ = 0;
  position_info_completer_.reset();
  delay_info_completer_.reset();
  // We don't reset ring_buffer_format_ and dai_format_ to allow for retrieval for observability.
}

void VirtualAudioDai::GetProperties(
    fidl::Server<fuchsia_hardware_audio::RingBuffer>::GetPropertiesCompleter::Sync& completer) {
  fidl::Arena arena;
  fuchsia_hardware_audio::RingBufferProperties properties;
  properties.needs_cache_flush_or_invalidate(false).driver_transfer_bytes(config_.fifo_depth_bytes);
  completer.Reply(std::move(properties));
}

void VirtualAudioDai::GetVmo(GetVmoRequest& request, GetVmoCompleter::Sync& completer) {
  if (ring_buffer_mapper_.start() != nullptr) {
    ring_buffer_mapper_.Unmap();
  }

  num_ring_buffer_frames_ = std::max(
      config_.ring_buffer.min_frames,
      fbl::round_up<uint32_t, uint32_t>(request.min_frames(), config_.ring_buffer.modulo_frames));

  zx_status_t status = ring_buffer_mapper_.CreateAndMap(
      num_ring_buffer_frames_ * frame_size_, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr,
      &ring_buffer_vmo_,
      ZX_RIGHT_READ | ZX_RIGHT_WRITE | ZX_RIGHT_MAP | ZX_RIGHT_DUPLICATE | ZX_RIGHT_TRANSFER);

  ZX_ASSERT_MSG(status == ZX_OK, "failed to create ring buffer VMO: %s",
                zx_status_get_string(status));

  zx::vmo out_vmo;
  status = ring_buffer_vmo_.duplicate(
      ZX_RIGHT_TRANSFER | ZX_RIGHT_READ | ZX_RIGHT_WRITE | ZX_RIGHT_MAP, &out_vmo);
  ZX_ASSERT_MSG(status == ZX_OK, "failed to duplicate VMO handle for out param: %s",
                zx_status_get_string(status));

  notifications_per_ring_ = request.clock_recovery_notifications_per_ring();

  zx::vmo duplicate_vmo;
  status = ring_buffer_vmo_.duplicate(
      ZX_RIGHT_TRANSFER | ZX_RIGHT_READ | ZX_RIGHT_WRITE | ZX_RIGHT_MAP, &duplicate_vmo);
  ZX_ASSERT_MSG(status == ZX_OK, "failed to duplicate VMO handle for VA client: %s",
                zx_status_get_string(status));

  auto parent = parent_.lock();
  ZX_ASSERT(parent);
  parent->NotifyBufferCreated(std::move(duplicate_vmo), num_ring_buffer_frames_,
                              notifications_per_ring_);
  fuchsia_hardware_audio::RingBufferGetVmoResponse response;
  response.num_frames(num_ring_buffer_frames_);
  response.ring_buffer(std::move(out_vmo));
  completer.Reply(zx::ok(std::move(response)));
  ring_buffer_vmo_fetched_ = true;
}

void VirtualAudioDai::Start(StartCompleter::Sync& completer) {
  if (!ring_buffer_vmo_fetched_) {
    zxlogf(ERROR, "Cannot start the ring buffer before retrieving the VMO");
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }
  if (ring_buffer_started_) {
    zxlogf(ERROR, "Cannot start the ring buffer if already started");
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }
  auto parent = parent_.lock();
  ZX_ASSERT(parent);
  zx_time_t now = zx::clock::get_monotonic().get();
  parent->NotifyStart(now);
  completer.Reply(now);
  ring_buffer_started_ = true;
}

void VirtualAudioDai::Stop(StopCompleter::Sync& completer) {
  if (!ring_buffer_vmo_fetched_) {
    zxlogf(ERROR, "Cannot start the ring buffer before retrieving the VMO");
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }
  if (!ring_buffer_started_) {
    zxlogf(INFO, "Stop called while stopped; doing nothing");
    completer.Reply();
    return;
  }
  auto parent = parent_.lock();
  ZX_ASSERT(parent);
  zx_time_t now = zx::clock::get_monotonic().get();
  // TODO(fxbug.dev/124865): Add support for position, now we always report 0.
  parent->NotifyStop(now, 0);
  completer.Reply();
  ring_buffer_started_ = false;
}

void VirtualAudioDai::WatchClockRecoveryPositionInfo(
    WatchClockRecoveryPositionInfoCompleter::Sync& completer) {
  if (!watch_position_replied_) {
    fuchsia_hardware_audio::RingBufferPositionInfo position_info;
    position_info.timestamp(zx::clock::get_monotonic().get());
    // TODO(fxbug.dev/124865): Add support for position, now we always report 0.
    position_info.position(0);
    completer.Reply(std::move(position_info));
    watch_position_replied_ = true;
  } else if (!position_info_completer_) {
    position_info_completer_.emplace(completer.ToAsync());
  } else {
    // The client called WatchClockRecoveryPositionInfo when another hanging get was pending.
    // This is an error condition and hence we unbind the channel.
    zxlogf(ERROR,
           "WatchClockRecoveryPositionInfo called when another hanging get was pending, unbinding");
    completer.Close(ZX_ERR_BAD_STATE);
  }
}

void VirtualAudioDai::WatchDelayInfo(WatchDelayInfoCompleter::Sync& completer) {
  if (!watch_delay_replied_) {
    fuchsia_hardware_audio::DelayInfo delay_info;
    delay_info.internal_delay(config_.internal_delay.get());
    completer.Reply(std::move(delay_info));
    watch_delay_replied_ = true;
  } else if (!delay_info_completer_) {
    delay_info_completer_.emplace(completer.ToAsync());
  } else {
    // The client called WatchDelayInfo when another hanging get was pending.
    // This is an error condition and hence we unbind the channel.
    zxlogf(ERROR, "WatchDelayInfo called when another hanging get was pending, unbinding");
    completer.Close(ZX_ERR_BAD_STATE);
  }
}

void VirtualAudioDai::SetActiveChannels(
    fuchsia_hardware_audio::RingBufferSetActiveChannelsRequest& request,
    SetActiveChannelsCompleter::Sync& completer) {
  // TODO(fxbug.dev/81649): Add support.
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

}  // namespace virtual_audio
