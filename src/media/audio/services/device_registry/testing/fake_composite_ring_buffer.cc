// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/testing/fake_composite_ring_buffer.h"

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/test_base.h>
#include <lib/fit/result.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>

#include <cstddef>
#include <optional>

#include "src/media/audio/services/device_registry/basic_types.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/testing/fake_composite.h"

namespace media_audio {

FakeCompositeRingBuffer::FakeCompositeRingBuffer(FakeComposite* parent, ElementId element_id,
                                                 fuchsia_hardware_audio::PcmFormat format,
                                                 size_t ring_buffer_allocated_size)
    : TestBase(),
      parent_(parent),
      element_id_(element_id),
      format_(std::move(format)),
      bytes_per_frame_(format_.number_of_channels() * format_.bytes_per_sample()),
      active_channels_bitmask_((1u << format_.number_of_channels()) - 1u),
      active_channels_set_time_(zx::clock::get_monotonic()) {
  ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
  AllocateRingBuffer(element_id_, ring_buffer_allocated_size);

  ++count_;
  ADR_LOG_METHOD(kLogFakeCompositeRingBuffer) << "There are now " << count_ << " instances";
}

FakeCompositeRingBuffer::~FakeCompositeRingBuffer() {
  --count_;
  ADR_LOG_METHOD(kLogFakeCompositeRingBuffer) << "There are now " << count_ << " instances";
}

void FakeCompositeRingBuffer::AllocateRingBuffer(ElementId element_id, size_t size) {
  ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
  FX_CHECK(!vmo_.is_valid()) << "Calling AllocateRingBuffer multiple times is not supported";
  allocated_size_ = size;

  fzl::VmoMapper mapper;
  mapper.CreateAndMap(allocated_size_, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr, &vmo_);
}

void FakeCompositeRingBuffer::NotImplemented_(const std::string& name,
                                              ::fidl::CompleterBase& completer) {
  ADR_WARN_METHOD() << name;
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void FakeCompositeRingBuffer::GetProperties(GetPropertiesCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
  fuchsia_hardware_audio::RingBufferProperties props;
  if (needs_cache_flush_or_invalidate_) {
    props.needs_cache_flush_or_invalidate(*needs_cache_flush_or_invalidate_);
  }
  if (turn_on_delay_) {
    props.turn_on_delay(turn_on_delay_->get());
  }
  if (driver_transfer_bytes_) {
    props.driver_transfer_bytes(*driver_transfer_bytes_);
  }
  completer.Reply(props);
}

void FakeCompositeRingBuffer::GetVmo(GetVmoRequest& request, GetVmoCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
  auto total_requested_size =
      driver_transfer_bytes_.value_or(0) + request.min_frames() * bytes_per_frame_;
  if (total_requested_size > allocated_size_) {
    ADR_WARN_METHOD() << "Requested size " << total_requested_size << " exceeds allocated size "
                      << allocated_size_;
    completer.Reply(fit::error(fuchsia_hardware_audio::GetVmoError::kInvalidArgs));
    return;
  }
  clock_recovery_notifications_per_ring_ = request.clock_recovery_notifications_per_ring();
  requested_frames_ = (total_requested_size - 1) / bytes_per_frame_ + 1;

  // Dup our ring buffer VMO to send over the channel.
  zx::vmo out_vmo;
  FX_CHECK(vmo_.duplicate(ZX_RIGHT_SAME_RIGHTS, &out_vmo) == ZX_OK);

  completer.Reply(zx::ok(fuchsia_hardware_audio::RingBufferGetVmoResponse{{
      .num_frames = requested_frames_,
      .ring_buffer = std::move(out_vmo),
  }}));
}

void FakeCompositeRingBuffer::Start(StartCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
  if (!vmo_.is_valid() || started_) {
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }
  started_ = true;
  mono_start_time_ = zx::clock::get_monotonic();
  completer.Reply(mono_start_time_.get());
}

void FakeCompositeRingBuffer::Stop(StopCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
  if (!vmo_.is_valid() || !started_) {
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }
  started_ = false;
  mono_start_time_ = zx::time(0);
  completer.Reply();
}

void FakeCompositeRingBuffer::SetActiveChannels(SetActiveChannelsRequest& request,
                                                SetActiveChannelsCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
  if (!supports_active_channels_) {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
    return;
  }
  if (request.active_channels_bitmask() >= (1u << format_.number_of_channels())) {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  if (active_channels_bitmask_ != request.active_channels_bitmask()) {
    active_channels_bitmask_ = request.active_channels_bitmask();
    active_channels_set_time_ = zx::clock::get_monotonic();
  }
  completer.Reply(zx::ok(active_channels_set_time_.get()));
}

void FakeCompositeRingBuffer::WatchDelayInfo(WatchDelayInfoCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
  if (watch_delay_info_completer_.has_value()) {
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }

  watch_delay_info_completer_ = completer.ToAsync();
  MaybeCompleteWatchDelayInfo();
}

void FakeCompositeRingBuffer::InjectDelayUpdate(std::optional<zx::duration> internal_delay,
                                                std::optional<zx::duration> external_delay) {
  ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
  if (internal_delay.value_or(zx::nsec(0)) != internal_delay_.value_or(zx::nsec(0)) ||
      external_delay.value_or(zx::nsec(0)) != external_delay_.value_or(zx::nsec(0))) {
    delays_have_changed_ = true;
  }
  internal_delay_ = internal_delay;
  external_delay_ = external_delay;

  MaybeCompleteWatchDelayInfo();
}

void FakeCompositeRingBuffer::MaybeCompleteWatchDelayInfo() {
  ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
  if (delays_have_changed_ && watch_delay_info_completer_) {
    delays_have_changed_ = false;

    auto completer = std::move(*watch_delay_info_completer_);
    watch_delay_info_completer_.reset();

    fuchsia_hardware_audio::DelayInfo info;
    if (internal_delay_) {
      info.internal_delay(internal_delay_->get());
    }
    if (external_delay_) {
      info.external_delay(external_delay_->get());
    }
    completer.Reply(std::move(info));
  }
}

void FakeCompositeRingBuffer::WatchClockRecoveryPositionInfo(
    WatchClockRecoveryPositionInfoCompleter::Sync& completer) {
  NotImplemented_("WatchClockRecoveryPositionInfo", completer);
}

}  // namespace media_audio
