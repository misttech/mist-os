// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_TESTS_ADMIN_TEST_H_
#define SRC_MEDIA_AUDIO_DRIVERS_TESTS_ADMIN_TEST_H_

#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/zx/time.h>
#include <zircon/device/audio.h>
#include <zircon/errors.h>
#include <zircon/rights.h>

#include <optional>

#include "src/media/audio/drivers/tests/test_base.h"

namespace media::audio::drivers::test {

// BasicTest cases must run in environments where an audio driver may already have an active client.
// AdminTest cases, by contrast, need not worry about interfering with any other client. AdminTest
// cases, by definition, can reconfigure devices without worrying about restoring previous state.
//
// A driver can have only one RingBuffer client connection at any time, so BasicTest avoids any
// usage of the RingBuffer interface. AdminTest includes (but is not limited to) RingBuffer tests.
// AdminTest cases may also change signalprocessing topology/elements or other device state.
class AdminTest : public TestBase {
 public:
  explicit AdminTest(const DeviceEntry& dev_entry) : TestBase(dev_entry) {}

 protected:
  static constexpr zx_rights_t kRightsVmoIncoming =
      ZX_RIGHT_READ | ZX_RIGHT_MAP | ZX_RIGHT_TRANSFER;
  static constexpr zx_rights_t kRightsVmoOutgoing = kRightsVmoIncoming | ZX_RIGHT_WRITE;

  void TearDown() override;
  void DropRingBuffer();

  void ValidateElementTopologyClosure();

  void ResetAndExpectResponse();
  void RequestCodecStartAndExpectResponse();
  void RequestCodecStopAndExpectResponse();

  void RequestRingBufferChannelWithMinFormat();
  void RequestRingBufferChannelWithMaxFormat();
  void CalculateRingBufferFrameSize();

  void RequestRingBufferProperties();
  void RequestBuffer(uint32_t min_ring_buffer_frames, uint32_t notifications_per_ring);

  enum SetActiveChannelsOutcome : uint8_t {
    SUCCESS = 1,  // May or may not represent a change
    CHANGE,       // Successful change of active channels.
    NO_CHANGE,    // Successful but not a change.
    FAILURE,      // Unsuccessful.
  };
  void ActivateChannelsAndExpectOutcome(uint64_t active_channels_bitmask,
                                        SetActiveChannelsOutcome expected_outcome);

  zx::time RequestRingBufferStart();
  void RequestRingBufferStartAndExpectCallback();
  void RequestRingBufferStartAndExpectDisconnect(zx_status_t expected_error);
  void WaitUntilAfterStartTime();

  void RequestRingBufferStopAndExpectCallback();
  void RequestRingBufferStopAndExpectNoPositionNotifications();
  void RequestRingBufferStopAndExpectDisconnect(zx_status_t expected_error);

  void RequestPositionNotification();
  virtual void PositionNotificationCallback(
      fuchsia::hardware::audio::RingBufferPositionInfo position_info);

  // Clear flag so position notifications (even already-enqueued ones) do not cause failures.
  void ExpectPositionNotifications() { fail_on_position_notification_ = false; }
  // Set flag so position notifications (even already-enqueued ones!) cause failures.
  void ExpectNoPositionNotifications() { fail_on_position_notification_ = true; }

  void WatchDelayAndExpectUpdate();
  void WatchDelayAndExpectNoUpdate();
  void ValidateInternalDelay();
  void ValidateExternalDelay();

  fidl::InterfacePtr<fuchsia::hardware::audio::RingBuffer>& ring_buffer() { return ring_buffer_; }
  uint32_t ring_buffer_frames() const { return ring_buffer_frames_; }
  fuchsia::hardware::audio::PcmFormat ring_buffer_pcm_format() const {
    return ring_buffer_pcm_format_;
  }
  void SetRingBufferIncoming(std::optional<bool> is_incoming) {
    ring_buffer_is_incoming_ = is_incoming;
  }

  uint32_t notifications_per_ring() const { return notifications_per_ring_; }
  const zx::time& start_time() const { return start_time_; }
  uint16_t frame_size() const { return frame_size_; }

 private:
  static constexpr zx::duration kRingBufferDisconnectCooldownDuration = zx::msec(100);

  static void CooldownAfterRingBufferDisconnect();
  void RequestRingBufferChannel();

  fidl::InterfacePtr<fuchsia::hardware::audio::RingBuffer> ring_buffer_;
  std::optional<bool> ring_buffer_is_incoming_ = std::nullopt;
  std::optional<fuchsia::hardware::audio::RingBufferProperties> ring_buffer_props_;
  std::optional<fuchsia::hardware::audio::DelayInfo> delay_info_;

  uint32_t min_ring_buffer_frames_ = 0;
  uint32_t notifications_per_ring_ = 0;
  uint32_t ring_buffer_frames_ = 0;
  fzl::VmoMapper ring_buffer_mapper_;

  zx::time start_time_;
  // Ring buffer PCM format.
  fuchsia::hardware::audio::PcmFormat ring_buffer_pcm_format_;
  // DAI interconnect format.
  fuchsia::hardware::audio::DaiFormat dai_format_;
  uint16_t frame_size_ = 0;

  // Position notifications are hanging-gets. On receipt, should we register the next one or fail?
  bool fail_on_position_notification_ = false;
};

}  // namespace media::audio::drivers::test

#endif  // SRC_MEDIA_AUDIO_DRIVERS_TESTS_ADMIN_TEST_H_
