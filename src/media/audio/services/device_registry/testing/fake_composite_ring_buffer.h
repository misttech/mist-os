// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_COMPOSITE_RING_BUFFER_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_COMPOSITE_RING_BUFFER_H_

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/test_base.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/fidl/cpp/wire/unknown_interaction_handler.h>

#include <cstddef>
#include <cstdint>
#include <optional>
#include <string_view>

#include "src/media/audio/services/device_registry/basic_types.h"

namespace media_audio {

static constexpr bool kLogFakeCompositeRingBuffer = false;

class FakeComposite;

class FakeCompositeRingBuffer final
    : public fidl::testing::TestBase<fuchsia_hardware_audio::RingBuffer> {
  static inline const std::string_view kClassName = "FakeCompositeRingBuffer";

 public:
  static constexpr bool kDefaultNeedsCacheFlushInvalidate = false;
  static constexpr uint32_t kDefaultDriverTransferBytes = 32;
  static constexpr bool kDefaultSupportsActiveChannels = false;
  static constexpr std::optional<zx::duration> kDefaultTurnOnDelay = std::nullopt;
  static constexpr std::optional<zx::duration> kDefaultInternalDelay = zx::usec(20);
  static constexpr std::optional<zx::duration> kDefaultExternalDelay = std::nullopt;

  FakeCompositeRingBuffer() : TestBase() { ++count_; }
  FakeCompositeRingBuffer(FakeComposite* parent, ElementId element_id,
                          fuchsia_hardware_audio::PcmFormat format,
                          size_t ring_buffer_allocated_size);
  ~FakeCompositeRingBuffer() override;

  static void on_rb_unbind(FakeCompositeRingBuffer* fake_ring_buffer, fidl::UnbindInfo info,
                           fidl::ServerEnd<fuchsia_hardware_audio::RingBuffer> server_end);

  void GetProperties(GetPropertiesCompleter::Sync& completer) override;
  void GetVmo(GetVmoRequest& request, GetVmoCompleter::Sync& completer) override;
  void Start(StartCompleter::Sync& completer) override;
  void Stop(StopCompleter::Sync& completer) override;
  void SetActiveChannels(SetActiveChannelsRequest& request,
                         SetActiveChannelsCompleter::Sync& completer) override;
  void WatchDelayInfo(WatchDelayInfoCompleter::Sync& completer) override;
  void WatchClockRecoveryPositionInfo(
      WatchClockRecoveryPositionInfoCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_audio::RingBuffer> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override;

  void AllocateRingBuffer(ElementId element_id, size_t size);
  void InjectDelayUpdate(std::optional<zx::duration> internal_delay,
                         std::optional<zx::duration> external_delay);
  void MaybeCompleteWatchDelayInfo();

  // Accessors
  ElementId element_id() const { return element_id_; }

  // To be used during run-time
  bool started() const { return started_; }
  zx::time mono_start_time() const { return mono_start_time_; }
  uint64_t active_channels_bitmask() const { return active_channels_bitmask_; }
  zx::time set_active_channels_completed_at() const { return set_active_channels_completed_at_; }

  // For configuring the object before it starts being used.
  void enable_active_channels_support() { supports_active_channels_ = true; }
  void disable_active_channels_support() { supports_active_channels_ = false; }
  void set_turn_on_delay(zx::duration turn_on_delay) { turn_on_delay_ = turn_on_delay; }
  void clear_turn_on_delay() { turn_on_delay_.reset(); }
  void set_internal_delay(zx::duration internal_delay) { internal_delay_ = internal_delay; }
  void set_external_delay(zx::duration external_delay) { external_delay_ = external_delay; }
  void clear_external_delay() { external_delay_.reset(); }

  static uint64_t count() { return count_; }
  FakeComposite* parent() { return parent_; }

 private:
  static inline uint64_t count_ = 0;

  // ctor
  FakeComposite* parent_;
  ElementId element_id_;
  fuchsia_hardware_audio::PcmFormat format_;
  uint32_t bytes_per_frame_;

  // GetProperties
  std::optional<bool> needs_cache_flush_or_invalidate_ = kDefaultNeedsCacheFlushInvalidate;
  std::optional<zx::duration> turn_on_delay_ = kDefaultTurnOnDelay;
  std::optional<uint32_t> driver_transfer_bytes_ = kDefaultDriverTransferBytes;

  // GetVmo
  uint32_t requested_frames_;
  zx::vmo vmo_;
  size_t allocated_size_;

  // Start / Stop
  bool started_ = false;
  zx::time mono_start_time_;

  // SetActiveChannels
  bool supports_active_channels_ = kDefaultSupportsActiveChannels;
  uint64_t active_channels_bitmask_;
  zx::time set_active_channels_completed_at_;

  // WatchDelayInfo
  std::optional<WatchDelayInfoCompleter::Async> watch_delay_info_completer_;
  std::optional<zx::duration> internal_delay_ = kDefaultInternalDelay;
  std::optional<zx::duration> external_delay_;
  bool delays_have_changed_ = true;

  // WatchClockRecoveryPositionInfo
  uint32_t clock_recovery_notifications_per_ring_ = 0;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_COMPOSITE_RING_BUFFER_H_
