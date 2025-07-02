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

  void ResetAndExpectResponse();
  void RequestCodecStartAndExpectResponse();
  void RequestCodecStopAndExpectResponse();

  void RequestRingBufferChannelWithMinFormat();
  void RequestRingBufferChannelWithMaxFormat();
  void CalculateRingBufferFrameSize();

  void RequestRingBufferProperties();
  void RequestBuffer(uint32_t min_ring_buffer_frames, uint32_t notifications_per_ring);

  enum SetActiveChannelsOutcome : uint8_t {
    SUCCESS = 1,  // Successful call.
    CHANGE,       // Successful call. As intended, the active-channels state was changed.
    NO_CHANGE,    // Successful call. As intended, the active-channels state was NOT changed.
    FAILURE,      // Unsuccessful.
  };
  void ActivateChannelsAndExpectOutcome(uint64_t active_channels_bitmask,
                                        SetActiveChannelsOutcome expected_outcome);

  void RetrieveRingBufferFormats() override;
  void RetrieveDaiFormats() override;

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

  void SignalProcessingConnect();

  void RequestElements();
  void ValidateElements();
  void ValidateDaiElements();
  void ValidateDynamicsElements();
  void ValidateEqualizerElements();
  void ValidateGainElements();
  void ValidateVendorSpecificElements();

  void RequestTopologies();
  void RetrieveInitialTopology();
  void ValidateElementTopologyClosure();

  void WatchForTopology(fuchsia::hardware::audio::signalprocessing::TopologyId id);
  void FailOnWatchTopologyCompletion();
  void WatchTopologyAndExpectDisconnect(zx_status_t expected_error);

  void SetAllTopologies();
  void SetTopologyAndExpectCallback(fuchsia::hardware::audio::signalprocessing::TopologyId id);
  void SetTopologyUnknownIdAndExpectError();
  void SetTopologyNoChangeAndExpectNoWatch();

  void RetrieveInitialElementStates();
  void ValidateElementStates();
  void ValidateDaiElementStates();
  void ValidateDynamicsElementStates();
  void ValidateEqualizerElementStates();
  void ValidateGainElementStates();
  void ValidateVendorSpecificElementStates();

  void SetAllElementStates();
  void SetAllDynamicsElementStates();
  void SetAllEqualizerElementStates();
  void SetAllGainElementStates();
  void SetAllGainElementStatesNoChange();
  void SetAllGainElementStatesInvalidGainShouldError();
  void SetAllElementStatesNoChange();
  void SetElementStateUnknownIdAndExpectError();
  void SetElementStateNoChange(fuchsia::hardware::audio::signalprocessing::ElementId id);

  void FailOnWatchElementStateCompletion(fuchsia::hardware::audio::signalprocessing::ElementId id);
  void WatchElementStateAndExpectDisconnect(
      fuchsia::hardware::audio::signalprocessing::ElementId id, zx_status_t expected_error);
  void WatchElementStateUnknownIdAndExpectDisconnect(zx_status_t expected_error);

  fidl::InterfacePtr<fuchsia::hardware::audio::RingBuffer>& ring_buffer() { return ring_buffer_; }
  uint32_t ring_buffer_frames() const { return ring_buffer_frames_; }
  fuchsia::hardware::audio::PcmFormat ring_buffer_pcm_format() const {
    return ring_buffer_pcm_format_;
  }
  void SetRingBufferIncoming(std::optional<bool> is_incoming) {
    ring_buffer_is_incoming_ = is_incoming;
  }
  bool ElementIsRingBuffer(fuchsia::hardware::audio::ElementId element_id);
  bool RingBufferElementIsIncoming(fuchsia::hardware::audio::ElementId element_id);
  std::optional<bool> ElementIsIncoming(
      std::optional<fuchsia::hardware::audio::ElementId> ring_buffer_element_id);

  uint32_t notifications_per_ring() const { return notifications_per_ring_; }
  const zx::time& start_time() const { return start_time_; }
  uint16_t frame_size() const { return frame_size_; }

  std::optional<uint64_t>& ring_buffer_id() { return ring_buffer_id_; }

  fidl::InterfacePtr<fuchsia::hardware::audio::signalprocessing::SignalProcessing>&
  signal_processing() {
    return sp_;
  }
  const std::vector<fuchsia::hardware::audio::signalprocessing::Topology>& topologies() const {
    return topologies_;
  }
  const std::vector<fuchsia::hardware::audio::signalprocessing::Element>& elements() const {
    return elements_;
  }

 private:
  void RequestRingBufferChannel();

  static void ValidateElement(const fuchsia::hardware::audio::signalprocessing::Element& element);
  static void ValidateDaiElement(
      const fuchsia::hardware::audio::signalprocessing::Element& element);
  static void ValidateDynamicsElement(
      const fuchsia::hardware::audio::signalprocessing::Element& element);
  static void ValidateEqualizerElement(
      const fuchsia::hardware::audio::signalprocessing::Element& element);
  static void ValidateGainElement(
      const fuchsia::hardware::audio::signalprocessing::Element& element);
  static void ValidateVendorSpecificElement(
      const fuchsia::hardware::audio::signalprocessing::Element& element);

  static void ValidateElementState(
      const fuchsia::hardware::audio::signalprocessing::Element& element,
      const fuchsia::hardware::audio::signalprocessing::ElementState& state);
  static void ValidateDaiElementState(
      const fuchsia::hardware::audio::signalprocessing::Element& element,
      const fuchsia::hardware::audio::signalprocessing::ElementState& state);
  static void ValidateDynamicsElementState(
      const fuchsia::hardware::audio::signalprocessing::Element& element,
      const fuchsia::hardware::audio::signalprocessing::ElementState& state);
  static void ValidateEqualizerElementState(
      const fuchsia::hardware::audio::signalprocessing::Element& element,
      const fuchsia::hardware::audio::signalprocessing::ElementState& state);
  static void ValidateGainElementState(
      const fuchsia::hardware::audio::signalprocessing::Element& element,
      const fuchsia::hardware::audio::signalprocessing::ElementState& state);
  static void ValidateVendorSpecificElementState(
      const fuchsia::hardware::audio::signalprocessing::Element& element,
      const fuchsia::hardware::audio::signalprocessing::ElementState& state);

  void TestSetElementState(
      const fuchsia::hardware::audio::signalprocessing::Element& element,
      const fuchsia::hardware::audio::signalprocessing::ElementState& initial_state);
  void TestSetDynamicsElementState(
      const fuchsia::hardware::audio::signalprocessing::Element& element,
      const fuchsia::hardware::audio::signalprocessing::ElementState& initial_state);
  void TestSetEqualizerElementState(
      const fuchsia::hardware::audio::signalprocessing::Element& element,
      const fuchsia::hardware::audio::signalprocessing::ElementState& initial_state);
  void TestSetGainElementState(
      const fuchsia::hardware::audio::signalprocessing::Element& element,
      const fuchsia::hardware::audio::signalprocessing::ElementState& initial_state);
  void TestSetGainElementStateNoChange(
      fuchsia::hardware::audio::signalprocessing::ElementId element_id, float current_gain);
  void TestSetGainElementStateInvalidGain(
      fuchsia::hardware::audio::signalprocessing::ElementId element_id);

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

  fidl::InterfacePtr<fuchsia::hardware::audio::signalprocessing::SignalProcessing> sp_;
  std::optional<bool> signalprocessing_is_supported_ = std::nullopt;
  std::vector<fuchsia::hardware::audio::signalprocessing::Topology> topologies_;
  std::vector<fuchsia::hardware::audio::signalprocessing::Element> elements_;
  std::optional<fuchsia::hardware::audio::signalprocessing::TopologyId> initial_topology_id_ =
      std::nullopt;
  std::optional<fuchsia::hardware::audio::signalprocessing::TopologyId> pending_set_topology_id_;
  std::optional<fuchsia::hardware::audio::signalprocessing::TopologyId> current_topology_id_ =
      std::nullopt;

  std::map<fuchsia::hardware::audio::signalprocessing::TopologyId,
           fuchsia::hardware::audio::signalprocessing::ElementState>
      initial_element_states_;

  std::optional<uint64_t> ring_buffer_id_;  // Ring buffer process element id.
  std::optional<uint64_t> dai_id_;          // DAI interconnect process element id.
};

}  // namespace media::audio::drivers::test

#endif  // SRC_MEDIA_AUDIO_DRIVERS_TESTS_ADMIN_TEST_H_
