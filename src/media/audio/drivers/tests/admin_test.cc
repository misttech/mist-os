// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/tests/admin_test.h"

#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/vmo.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/time.h>

#include <cstring>
#include <numeric>
#include <optional>
#include <unordered_set>

#include <gtest/gtest.h>

namespace fhasp = fuchsia::hardware::audio::signalprocessing;

namespace media::audio::drivers::test {

inline constexpr bool kTolerateNonTerminalDaiEndpoints = true;
inline constexpr bool kDisplayElementsAndTopologies = false;

namespace {

void DisplayElements(
    const std::vector<fuchsia::hardware::audio::signalprocessing::Element>& elements) {
  std::stringstream ss;
  ss << "Elements[" << elements.size() << "]:" << '\n';
  auto element_idx = 0u;
  for (const auto& element : elements) {
    ss << "        [" << element_idx++ << "] id " << element.id() << ", type " << element.type()
       << (element.type() ==
                   fuchsia::hardware::audio::signalprocessing::ElementType::DAI_INTERCONNECT
               ? " (DAI)"
               : "")
       << (element.type() == fuchsia::hardware::audio::signalprocessing::ElementType::RING_BUFFER
               ? " (RING_BUFFER)"
               : "")
       << '\n';
  }
  printf("%s", ss.str().c_str());
}

void DisplayTopologies(
    const std::vector<fuchsia::hardware::audio::signalprocessing::Topology>& topologies) {
  std::stringstream ss;
  ss << "Topologies[" << topologies.size() << "]:" << '\n';
  auto topology_idx = 0u;
  for (const auto& topology : topologies) {
    ss << "          [" << topology_idx++ << "] id " << topology.id() << ", edges["
       << topology.processing_elements_edge_pairs().size() << "]:" << '\n';
    auto edge_idx = 0u;
    for (const auto& edge_pair : topology.processing_elements_edge_pairs()) {
      ss << "              [" << edge_idx++ << "] " << edge_pair.processing_element_id_from
         << " -> " << edge_pair.processing_element_id_to << '\n';
    }
  }
  printf("%s", ss.str().c_str());
}

}  // namespace

void AdminTest::TearDown() {
  DropRingBuffer();

  TestBase::TearDown();
}

void AdminTest::RequestCodecStartAndExpectResponse() {
  ASSERT_TRUE(device_entry().isCodec());

  zx_time_t received_start_time = ZX_TIME_INFINITE_PAST;
  zx_time_t pre_start_time = zx::clock::get_monotonic().get();
  codec()->Start(AddCallback("Codec::Start", [&received_start_time](int64_t start_time) {
    received_start_time = start_time;
  }));

  ExpectCallbacks();
  if (!HasFailure()) {
    EXPECT_GT(received_start_time, pre_start_time);
    EXPECT_LT(received_start_time, zx::clock::get_monotonic().get());
  }
}

void AdminTest::RequestCodecStopAndExpectResponse() {
  ASSERT_TRUE(device_entry().isCodec());

  zx_time_t received_stop_time = ZX_TIME_INFINITE_PAST;
  zx_time_t pre_stop_time = zx::clock::get_monotonic().get();
  codec()->Stop(AddCallback(
      "Codec::Stop", [&received_stop_time](int64_t stop_time) { received_stop_time = stop_time; }));

  ExpectCallbacks();
  if (!HasFailure()) {
    EXPECT_GT(received_stop_time, pre_stop_time);
    EXPECT_LT(received_stop_time, zx::clock::get_monotonic().get());
  }
}

// Request that the driver reset, expecting a response.
// TODO(https://fxbug.dev/42075676): Test Reset for Composite and Dai (Reset closes any RingBuffer).
// TODO(https://fxbug.dev/42077405): When SignalProcessing testing, Reset should change this state.
void AdminTest::ResetAndExpectResponse() {
  if (device_entry().isCodec()) {
    codec()->Reset(AddCallback("Codec::Reset"));
  } else {
    FAIL() << "Unexpected device type";
    __UNREACHABLE;
  }
  ExpectCallbacks();
}

// Is this id a RingBuffer element?
bool AdminTest::ElementIsRingBuffer(fuchsia::hardware::audio::ElementId element_id) {
  return std::ranges::any_of(
      elements_, [element_id](const fuchsia::hardware::audio::signalprocessing::Element& element) {
        return element.has_id() && element.id() == element_id && element.has_type() &&
               element.type() ==
                   fuchsia::hardware::audio::signalprocessing::ElementType::RING_BUFFER;
      });
}

// Is this RingBuffer element Incoming (its contents are READ-ONLY) or Outgoing (also WRITABLE)?
// We walk the signalprocessing topologies. If (across all the topologies) this element ever has
// any incoming edges, then it is not itself an outgoing RingBuffer. If this element ever has any
// outgoing edges, then it is not itself an incoming RingBuffer.
std::optional<bool> AdminTest::ElementIsIncoming(
    std::optional<fuchsia::hardware::audio::ElementId> ring_buffer_element_id) {
  if (!device_entry().isComposite()) {
    return TestBase::IsIncoming();
  }

  if (!ring_buffer_element_id.has_value()) {
    ADD_FAILURE() << "ring_buffer_element_id is not set";
    return std::nullopt;
  }

  if (!ElementIsRingBuffer(*ring_buffer_element_id)) {
    ADD_FAILURE() << "element_id " << *ring_buffer_element_id << " is not a RingBuffer element";
    return std::nullopt;
  }

  if (!current_topology_id_.has_value()) {
    ADD_FAILURE() << "current_topology_id_ is n ot set";
    return std::nullopt;
  }

  std::vector<fuchsia::hardware::audio::signalprocessing::EdgePair> edge_pairs;
  for (const auto& t : topologies_) {
    if (t.has_id() && t.id() == *current_topology_id_) {
      edge_pairs = t.processing_elements_edge_pairs();
      break;
    }
  }
  if (edge_pairs.empty()) {
    ADD_FAILURE() << "could not find edge_pairs for current_topology_id " << *current_topology_id_;
    return std::nullopt;
  }
  bool has_outgoing = std::ranges::any_of(
      edge_pairs, [element_id = *ring_buffer_element_id](
                      const fuchsia::hardware::audio::signalprocessing::EdgePair& edge_pair) {
        return (edge_pair.processing_element_id_from == element_id);
      });
  bool has_incoming = std::ranges::any_of(
      edge_pairs, [element_id = *ring_buffer_element_id](
                      const fuchsia::hardware::audio::signalprocessing::EdgePair& edge_pair) {
        return (edge_pair.processing_element_id_to == element_id);
      });

  if (has_outgoing && !has_incoming) {
    return false;
  }
  if (!has_outgoing && has_incoming) {
    return true;
  }

  ADD_FAILURE() << "For RingBuffer element " << *ring_buffer_element_id
                << ", has_outgoing and has_incoming are both " << (has_outgoing ? "TRUE" : "FALSE");
  return std::nullopt;
}

// For the channelization and sample_format that we've set for the ring buffer, determine the size
// of each frame. This method assumes that CreateRingBuffer has already been sent to the driver.
void AdminTest::CalculateRingBufferFrameSize() {
  EXPECT_LE(ring_buffer_pcm_format_.valid_bits_per_sample,
            ring_buffer_pcm_format_.bytes_per_sample * 8);
  frame_size_ =
      ring_buffer_pcm_format_.number_of_channels * ring_buffer_pcm_format_.bytes_per_sample;
}

void AdminTest::RequestRingBufferChannel() {
  ASSERT_FALSE(device_entry().isCodec());

  fuchsia::hardware::audio::Format rb_format = {};
  rb_format.set_pcm_format(ring_buffer_pcm_format_);

  fidl::InterfaceHandle<fuchsia::hardware::audio::RingBuffer> ring_buffer_handle;
  if (device_entry().isComposite()) {
    RequestTopologies();
    RetrieveInitialTopology();

    // If a ring_buffer_id exists, request it - but don't fail if the driver has no ring buffer.
    if (ring_buffer_id().has_value()) {
      composite()->CreateRingBuffer(
          *ring_buffer_id(), std::move(rb_format), ring_buffer_handle.NewRequest(),
          // Unlike in other driver types, Composite::CreateRingBuffer has a completion. However,
          // a client need not actually wait for this completion before using the ring_buffer.
          // For sequences such as Composite::CreateRingBuffer > RingBuffer::GetProperties,
          // relax the required ordering of completion receipts.
          AddCallbackUnordered(
              "CreateRingBuffer",
              [](fuchsia::hardware::audio::Composite_CreateRingBuffer_Result result) {
                EXPECT_FALSE(result.is_err());
              }));
      if (!composite().is_bound()) {
        FAIL() << "Composite failed to get ring buffer channel";
      }
      SetRingBufferIncoming(ElementIsIncoming(ring_buffer_id()));
    }
  } else if (device_entry().isDai()) {
    fuchsia::hardware::audio::DaiFormat dai_format = {};
    EXPECT_EQ(fuchsia::hardware::audio::Clone(dai_format_, &dai_format), ZX_OK);
    dai()->CreateRingBuffer(std::move(dai_format), std::move(rb_format),
                            ring_buffer_handle.NewRequest());
    EXPECT_TRUE(dai().is_bound()) << "Dai failed to get ring buffer channel";
    SetRingBufferIncoming(IsIncoming());
  } else {
    stream_config()->CreateRingBuffer(std::move(rb_format), ring_buffer_handle.NewRequest());
    EXPECT_TRUE(stream_config().is_bound()) << "StreamConfig failed to get ring buffer channel";
    SetRingBufferIncoming(IsIncoming());
  }
  zx::channel channel = ring_buffer_handle.TakeChannel();
  ring_buffer_ =
      fidl::InterfaceHandle<fuchsia::hardware::audio::RingBuffer>(std::move(channel)).Bind();
  EXPECT_TRUE(ring_buffer_.is_bound()) << "Failed to get ring buffer channel";

  AddErrorHandler(ring_buffer_, "RingBuffer");

  CalculateRingBufferFrameSize();
}

// Request that driver set format to the lowest bit-rate/channelization of the ranges reported.
// This method assumes that the driver has already successfully responded to a GetFormats request.
void AdminTest::RequestRingBufferChannelWithMinFormat() {
  ASSERT_FALSE(device_entry().isCodec());

  if (ring_buffer_pcm_formats().empty() && device_entry().isComposite()) {
    GTEST_SKIP() << "*** this audio device returns no ring_buffer_formats. Skipping this test. ***";
    __UNREACHABLE;
  }
  ASSERT_GT(ring_buffer_pcm_formats().size(), 0u);

  ring_buffer_pcm_format_ = min_ring_buffer_format();
  if (device_entry().isComposite() || device_entry().isDai()) {
    GetMinDaiFormat(dai_format_);
  }
  RequestRingBufferChannel();
}

// Request that driver set the highest bit-rate/channelization of the ranges reported.
// This method assumes that the driver has already successfully responded to a GetFormats request.
void AdminTest::RequestRingBufferChannelWithMaxFormat() {
  ASSERT_FALSE(device_entry().isCodec());

  if (ring_buffer_pcm_formats().empty() && device_entry().isComposite()) {
    GTEST_SKIP() << "*** this audio device returns no ring_buffer_formats. Skipping this test. ***";
    __UNREACHABLE;
  }
  ASSERT_GT(ring_buffer_pcm_formats().size(), 0u);

  ring_buffer_pcm_format_ = max_ring_buffer_format();
  if (device_entry().isComposite() || device_entry().isDai()) {
    GetMaxDaiFormat(dai_format_);
  }
  RequestRingBufferChannel();
}

// Ring-buffer channel requests
//
// Request the RingBufferProperties, at the current format (relies on the ring buffer channel).
// Validate the four fields that might be returned (only one is currently required).
void AdminTest::RequestRingBufferProperties() {
  ASSERT_FALSE(device_entry().isCodec());

  ring_buffer_->GetProperties(AddCallback(
      "RingBuffer::GetProperties", [this](fuchsia::hardware::audio::RingBufferProperties props) {
        ring_buffer_props_ = std::move(props);
      }));
  ExpectCallbacks();
  if (HasFailure()) {
    return;
  }
  ASSERT_TRUE(ring_buffer_props_.has_value()) << "No RingBufferProperties table received";

  // This field is required.
  EXPECT_TRUE(ring_buffer_props_->has_needs_cache_flush_or_invalidate());

  if (ring_buffer_props_->has_turn_on_delay()) {
    // As a zx::duration, a negative value is theoretically possible, but this is disallowed.
    EXPECT_GE(ring_buffer_props_->turn_on_delay(), 0);
  }

  // This field is required, and must be non-zero.
  ASSERT_TRUE(ring_buffer_props_->has_driver_transfer_bytes());
  EXPECT_GT(ring_buffer_props_->driver_transfer_bytes(), 0u);
}

// Request the ring buffer's VMO handle, at the current format (relies on the ring buffer channel).
// `RequestRingBufferProperties` must be called before `RequestBuffer`.
void AdminTest::RequestBuffer(uint32_t min_ring_buffer_frames,
                              uint32_t notifications_per_ring = 0) {
  ASSERT_FALSE(device_entry().isCodec());

  ASSERT_TRUE(ring_buffer_props_.has_value())
      << "RequestBuffer was called before RequestRingBufferChannel";

  min_ring_buffer_frames_ = min_ring_buffer_frames;
  notifications_per_ring_ = notifications_per_ring;
  zx::vmo ring_buffer_vmo;
  ring_buffer_->GetVmo(
      min_ring_buffer_frames, notifications_per_ring,
      AddCallback("GetVmo", [this, &ring_buffer_vmo](
                                fuchsia::hardware::audio::RingBuffer_GetVmo_Result result) {
        ring_buffer_frames_ = result.response().num_frames;
        ring_buffer_vmo = std::move(result.response().ring_buffer);
        EXPECT_TRUE(ring_buffer_vmo.is_valid());
      }));
  ExpectCallbacks();
  if (HasFailure()) {
    return;
  }

  ASSERT_TRUE(ring_buffer_props_->has_driver_transfer_bytes());
  uint32_t driver_transfer_frames =
      (ring_buffer_props_->driver_transfer_bytes() + (frame_size_ - 1)) / frame_size_;
  EXPECT_GE(ring_buffer_frames_, min_ring_buffer_frames_ + driver_transfer_frames)
      << "Driver (returned " << ring_buffer_frames_
      << " frames) must add at least driver_transfer_bytes (" << driver_transfer_frames
      << " frames) to the client-requested ring buffer size (" << min_ring_buffer_frames_
      << " frames)";

  ring_buffer_mapper_.Unmap();
  const zx_vm_option_t option_flags = ZX_VM_PERM_READ | ZX_VM_PERM_WRITE;

  zx_info_handle_basic_t info;
  auto status =
      ring_buffer_vmo.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  ASSERT_EQ(status, ZX_OK) << "vmo.get_info returned error";

  const zx_rights_t required_rights =
      ring_buffer_is_incoming_.value_or(true) ? kRightsVmoIncoming : kRightsVmoOutgoing;
  EXPECT_EQ((info.rights & required_rights), required_rights)
      << "VMO rights 0x" << std::hex << info.rights << " are insufficient (0x" << required_rights
      << " are required)";

  EXPECT_EQ(
      ring_buffer_mapper_.CreateAndMap(static_cast<uint64_t>(ring_buffer_frames_) * frame_size_,
                                       option_flags, nullptr, &ring_buffer_vmo, required_rights),
      ZX_OK);
}

void AdminTest::ActivateChannelsAndExpectOutcome(uint64_t active_channels_bitmask,
                                                 SetActiveChannelsOutcome expected_outcome) {
  zx_status_t status = ZX_OK;
  auto set_time = zx::time(0);
  auto send_time = zx::clock::get_monotonic();
  ring_buffer_->SetActiveChannels(
      active_channels_bitmask,
      AddCallback("SetActiveChannels",
                  [&status, &set_time](
                      fuchsia::hardware::audio::RingBuffer_SetActiveChannels_Result result) {
                    if (!result.is_err()) {
                      set_time = zx::time(result.response().set_time);
                    } else {
                      status = result.err();
                    }
                  }));
  ExpectCallbacks();

  if (status == ZX_ERR_NOT_SUPPORTED) {
    GTEST_SKIP() << "This driver does not support SetActiveChannels()";
    __UNREACHABLE;
  }

  SCOPED_TRACE(testing::Message() << "...during ring_buffer_fidl->SetActiveChannels(0x" << std::hex
                                  << active_channels_bitmask << ")");
  if (expected_outcome == SetActiveChannelsOutcome::FAILURE) {
    ASSERT_NE(status, ZX_OK) << "SetActiveChannels succeeded unexpectedly";
    EXPECT_EQ(status, ZX_ERR_INVALID_ARGS) << "Unexpected failure code";
  } else {
    ASSERT_EQ(status, ZX_OK) << "SetActiveChannels failed unexpectedly";
    if (expected_outcome == SetActiveChannelsOutcome::NO_CHANGE) {
      EXPECT_LT(set_time.get(), send_time.get());
    } else if (expected_outcome == SetActiveChannelsOutcome::CHANGE) {
      EXPECT_GT(set_time.get(), send_time.get());
    }
  }
}

// When disconnecting a RingBuffer, there's no signal to wait on before proceeding (potentially
// immediately executing other tests); insert a 100-ms wait. This wait is even more important for
// error cases that cause the RingBuffer to disconnect: without it, subsequent test cases that use
// the RingBuffer may receive unexpected errors (e.g. ZX_ERR_PEER_CLOSED or ZX_ERR_INVALID_ARGS).
//
// We need this wait when testing a "real hardware" driver (i.e. on realtime-capable systems). For
// this reason a hardcoded time constant, albeit a test antipattern, is (grudgingly) acceptable.
//
// TODO(https://fxbug.dev/42064975): investigate why we fail without this delay, fix the
// drivers/test as necessary, and eliminate this workaround.
void AdminTest::CooldownAfterRingBufferDisconnect() {
  zx::nanosleep(zx::deadline_after(kRingBufferDisconnectCooldownDuration));
}

void AdminTest::CooldownAfterSignalProcessingDisconnect() {
  zx::nanosleep(zx::deadline_after(kSignalProcessingDisconnectCooldownDuration));
}

void AdminTest::RetrieveRingBufferFormats() {
  if (!device_entry().isComposite()) {
    TestBase::RetrieveRingBufferFormats();
    return;
  }
  RequestTopologies();

  // If ring_buffer_id_ is set, then a ring-buffer element exists for this composite device.
  // Retrieve the supported ring buffer formats for that node.
  if (!ring_buffer_id_.has_value()) {
    // "No ring buffer" is valid (Composite can replace Codec); do nothing in that case.
    return;
  }
  composite()->GetRingBufferFormats(
      *ring_buffer_id_,
      AddCallback("GetRingBufferFormats",
                  [this](fuchsia::hardware::audio::Composite_GetRingBufferFormats_Result result) {
                    ASSERT_FALSE(result.is_err()) << static_cast<int32_t>(result.err());
                    auto& supported_formats = result.response().ring_buffer_formats;
                    EXPECT_FALSE(supported_formats.empty());

                    for (size_t i = 0; i < supported_formats.size(); ++i) {
                      SCOPED_TRACE(testing::Message()
                                   << "Composite supported_formats[" << i << "]");
                      ASSERT_TRUE(supported_formats[i].has_pcm_supported_formats());
                      auto& format_set = *supported_formats[i].mutable_pcm_supported_formats();
                      ring_buffer_pcm_formats().push_back(std::move(format_set));
                    }
                  }));
  ExpectCallbacks();

  ValidateRingBufferFormatSets(ring_buffer_pcm_formats());
  if (!HasFailure()) {
    SetMinMaxRingBufferFormats();
  }
}

// Request that the driver return the format ranges that it supports.
void AdminTest::RetrieveDaiFormats() {
  if (!device_entry().isComposite()) {
    TestBase::RetrieveDaiFormats();
    return;
  }

  RequestTopologies();

  // If there is a dai id, request the DAI formats for this interconnect.
  if (dai_id_.has_value()) {
    composite()->GetDaiFormats(
        *dai_id_,
        AddCallback("Composite::GetDaiFormats",
                    [this](fuchsia::hardware::audio::Composite_GetDaiFormats_Result result) {
                      ASSERT_FALSE(result.is_err());
                      auto& supported_formats = result.response().dai_formats;
                      EXPECT_FALSE(supported_formats.empty());

                      for (size_t i = 0; i < supported_formats.size(); ++i) {
                        SCOPED_TRACE(testing::Message()
                                     << "Composite supported_formats[" << i << "]");
                        dai_formats().push_back(std::move(supported_formats[i]));
                      }
                    }));
  } else {
    // "No DAI" is also valid (Composite can replace StreamConfig); do nothing in that case.
    return;
  }

  ExpectCallbacks();

  ValidateDaiFormatSets(dai_formats());
  if (!HasFailure()) {
    SetMinMaxDaiFormats();
  }
}

// Request that the driver start the ring buffer engine, responding with the start_time.
// This method assumes that GetVmo has previously been called and we are not already started.
zx::time AdminTest::RequestRingBufferStart() {
  if (ring_buffer_frames_ == 0u) {
    ADD_FAILURE() << "GetVmo must be called before RingBuffer::Start()";
    return {};
  }

  // Any position notifications that arrive before RingBuffer::Start callback should cause failures.
  ExpectNoPositionNotifications();

  auto send_time = zx::clock::get_monotonic();
  ring_buffer_->Start(AddCallback("RingBuffer::Start", [this](int64_t start_time) {
    ExpectPositionNotifications();
    start_time_ = zx::time(start_time);
  }));
  return send_time;
}

// Request that the driver start the ring buffer engine, responding with the start_time.
// This method assumes that GetVmo has previously been called and we are not already started.
void AdminTest::RequestRingBufferStartAndExpectCallback() {
  auto send_time = RequestRingBufferStart();

  ExpectCallbacks();
  if (!HasFailure()) {
    EXPECT_GT(start_time_, send_time);
  }
}

// Request that the driver start the ring buffer engine, but expect disconnect rather than response.
void AdminTest::RequestRingBufferStartAndExpectDisconnect(zx_status_t expected_error) {
  ring_buffer_->Start(
      [](int64_t start_time) { FAIL() << "Received unexpected RingBuffer::Start response"; });

  ExpectError(ring_buffer(), expected_error);
  CooldownAfterRingBufferDisconnect();
}

// Ensure that the RingBuffer is already running before returning.
void AdminTest::WaitUntilAfterStartTime() {
  auto start = start_time();
  zx::nanosleep(start + zx::msec(1));
}

// Request that driver stop the ring buffer. This assumes that GetVmo has previously been called.
void AdminTest::RequestRingBufferStopAndExpectCallback() {
  ASSERT_GT(ring_buffer_frames_, 0u) << "GetVmo must be called before RingBuffer::Stop()";
  ring_buffer_->Stop(AddCallback("RingBuffer::Stop"));

  ExpectCallbacks();
}

// Request that the driver start the ring buffer engine, but expect disconnect rather than response.
// We would expect this if calling RingBuffer::Stop before GetVmo, for example.
void AdminTest::RequestRingBufferStopAndExpectDisconnect(zx_status_t expected_error) {
  ring_buffer_->Stop(AddUnexpectedCallback("RingBuffer::Stop - expected disconnect instead"));

  ExpectError(ring_buffer(), expected_error);
  CooldownAfterRingBufferDisconnect();
}

// After RingBuffer::Stop is called, no position notification should be received.
// To validate this without any race windows: from within the next position notification itself,
// we call RingBuffer::Stop and flag that subsequent position notifications should FAIL.
void AdminTest::RequestRingBufferStopAndExpectNoPositionNotifications() {
  ring_buffer_->Stop(
      AddCallback("RingBuffer::Stop", [this]() { ExpectNoPositionNotifications(); }));

  ExpectCallbacks();
}

void AdminTest::RequestPositionNotification() {
  ring_buffer()->WatchClockRecoveryPositionInfo(
      [this](fuchsia::hardware::audio::RingBufferPositionInfo position_info) {
        PositionNotificationCallback(position_info);
      });
}

void AdminTest::PositionNotificationCallback(
    fuchsia::hardware::audio::RingBufferPositionInfo position_info) {
  // If this is an unexpected callback, fail and exit.
  if (fail_on_position_notification_) {
    FAIL() << "Unexpected position notification";
    __UNREACHABLE;
  }
  ASSERT_GT(notifications_per_ring(), 0u)
      << "Position notification received: notifications_per_ring() cannot be zero";
}

void AdminTest::WatchDelayAndExpectUpdate() {
  ring_buffer_->WatchDelayInfo(AddCallback(
      "WatchDelayInfo", [this](fuchsia::hardware::audio::RingBuffer_WatchDelayInfo_Result result) {
        ASSERT_TRUE(result.is_response());
        delay_info_ = std::move(result.response().delay_info);
      }));
  ExpectCallbacks();

  ASSERT_TRUE(delay_info_.has_value()) << "No DelayInfo table received";
}

void AdminTest::WatchDelayAndExpectNoUpdate() {
  ring_buffer_->WatchDelayInfo(
      [](fuchsia::hardware::audio::RingBuffer_WatchDelayInfo_Result result) {
        FAIL() << "Unexpected delay update received";
      });
}

// We've already validated that we received an overall response.
// Internal delay must be present and non-negative.
void AdminTest::ValidateInternalDelay() {
  ASSERT_TRUE(delay_info_.has_value());
  ASSERT_TRUE(delay_info_->has_internal_delay());
  EXPECT_GE(delay_info_->internal_delay(), 0ll)
      << "WatchDelayInfo `internal_delay` (" << delay_info_->internal_delay()
      << ") cannot be negative";
}

// We've already validated that we received an overall response.
// External delay (if present) simply must be non-negative.
void AdminTest::ValidateExternalDelay() {
  ASSERT_TRUE(delay_info_.has_value());
  if (delay_info_->has_external_delay()) {
    EXPECT_GE(delay_info_->external_delay(), 0ll)
        << "WatchDelayInfo `external_delay` (" << delay_info_->external_delay()
        << ") cannot be negative";
  }
}

void AdminTest::DropRingBuffer() {
  if (ring_buffer_.is_bound()) {
    ring_buffer_.Unbind();
    ring_buffer_ = nullptr;
  }

  CooldownAfterRingBufferDisconnect();
}

// All signalprocessing-related methods are in AdminTest, because only Composite drivers support the
// signalprocessing protocols, and Composite drivers have only AdminTest cases.
void AdminTest::SignalProcessingConnect() {
  if (signal_processing().is_bound()) {
    return;  // Already connected.
  }
  fidl::InterfaceHandle<fuchsia::hardware::audio::signalprocessing::SignalProcessing> sp_client;
  fidl::InterfaceRequest<fuchsia::hardware::audio::signalprocessing::SignalProcessing> sp_server =
      sp_client.NewRequest();
  composite()->SignalProcessingConnect(std::move(sp_server));
  signal_processing() = sp_client.Bind();

  AddErrorHandler(signal_processing(), "SignalProcessing");
}

// Retrieve the element list. If signalprocessing is not supported, exit early;
// otherwise save the ID of a RING_BUFFER element, and the ID of a DAI_INTERCONNECT element.
// We will use these IDs later, when performing Dai-specific and RingBuffer-specific checks.
void AdminTest::RequestElements() {
  SignalProcessingConnect();

  // If we've already checked for signalprocessing support, then no need to do it again.
  if (signalprocessing_is_supported_.has_value()) {
    return;
  }
  if (!elements_.empty()) {
    return;
  }

  zx_status_t status = ZX_OK;
  ring_buffer_id_.reset();
  dai_id_.reset();
  signal_processing()->GetElements(AddCallback(
      "signalprocessing::Reader::GetElements",
      [this,
       &status](fuchsia::hardware::audio::signalprocessing::Reader_GetElements_Result result) {
        status = result.is_err() ? result.err() : ZX_OK;
        if (status == ZX_OK) {
          elements_ = std::move(result.response().processing_elements);
        } else {
          signalprocessing_is_supported_ = false;
        }
      }));
  ExpectCallbacks();

  // Either we get elements or the API method is not supported.
  ASSERT_TRUE(status == ZX_OK || status == ZX_ERR_NOT_SUPPORTED);
  // We don't check for topologies if GetElements is not supported.
  if (status == ZX_ERR_NOT_SUPPORTED) {
    signalprocessing_is_supported_ = false;
    return;
  }

  // If supported, GetElements must return at least one element.
  if (elements_.empty()) {
    signalprocessing_is_supported_ = false;
    FAIL() << "elements list is empty";
  }

  signalprocessing_is_supported_ = true;

  std::unordered_set<fuchsia::hardware::audio::signalprocessing::ElementId> element_ids;
  for (auto& element : elements_) {
    // All elements must have an id and type
    ASSERT_TRUE(element.has_id());
    ASSERT_TRUE(element.has_type());
    if (element.type() == fuchsia::hardware::audio::signalprocessing::ElementType::RING_BUFFER) {
      ring_buffer_id_.emplace(element.id());  // Override any previous.
    } else if (element.type() ==
               fuchsia::hardware::audio::signalprocessing::ElementType::DAI_INTERCONNECT) {
      dai_id_.emplace(element.id());  // Override any previous.
    }

    // No element id may be a duplicate.
    ASSERT_FALSE(element_ids.contains(element.id())) << "Duplicate element id " << element.id();
    element_ids.insert(element.id());
  }
}

// First retrieve the element list. If signalprocessing is not supported, exit early;
// otherwise save the ID of a RING_BUFFER element, and the ID of a DAI_INTERCONNECT element.
// We will use these IDs later, when performing Dai-specific and RingBuffer-specific checks.
void AdminTest::RequestTopologies() {
  SignalProcessingConnect();
  RequestElements();

  if (!signalprocessing_is_supported_.value_or(false)) {
    return;
  }
  if (!topologies_.empty()) {
    return;
  }

  signal_processing()->GetTopologies(AddCallback(
      "signalprocessing::Reader::GetTopologies",
      [this](fuchsia::hardware::audio::signalprocessing::Reader_GetTopologies_Result result) {
        if (result.is_err()) {
          signalprocessing_is_supported_ = false;
          FAIL() << "GetTopologies returned err " << result.err();
          __UNREACHABLE;
        }
        topologies_ = std::move(result.response().topologies);
      }));
  ExpectCallbacks();

  // We only call GetTopologies if we have elements, so we must have at least one topology.
  if (topologies_.empty()) {
    signalprocessing_is_supported_ = false;
    FAIL() << "topologies list is empty";
    __UNREACHABLE;
  }

  std::unordered_set<fuchsia::hardware::audio::signalprocessing::TopologyId> topology_ids;
  for (const auto& topology : topologies_) {
    // All topologies must have an id and a non-empty list of edges.
    ASSERT_TRUE(topology.has_id());
    ASSERT_TRUE(topology.has_processing_elements_edge_pairs())
        << "Topology " << topology.id() << " processing_elements_edge_pairs is null";
    ASSERT_FALSE(topology.processing_elements_edge_pairs().empty())
        << "Topology " << topology.id() << " processing_elements_edge_pairs is empty";

    // No topology id may be a duplicate.
    ASSERT_FALSE(topology_ids.contains(topology.id())) << "Duplicate topology id " << topology.id();
    topology_ids.insert(topology.id());
  }
}

void AdminTest::RetrieveInitialTopology() {
  SignalProcessingConnect();
  RequestElements();
  RequestTopologies();

  ASSERT_TRUE(signalprocessing_is_supported_.value_or(false))
      << "signalprocessing is required for this test";

  if (initial_topology_id_.has_value()) {
    return;
  }

  signal_processing()->WatchTopology(AddCallback(
      "signalprocessing::Reader::WatchTopology",
      [this](fuchsia::hardware::audio::signalprocessing::Reader_WatchTopology_Result result) {
        ASSERT_TRUE(result.is_response());
        initial_topology_id_ = result.response().topology_id;
        current_topology_id_ = initial_topology_id_;
      }));
  ExpectCallbacks();

  // We only call WatchTopology if we support signalprocessing, so a topology should be set now.
  ASSERT_TRUE(current_topology_id_.has_value());
  if (std::ranges::none_of(topologies_, [id = *current_topology_id_](const auto& topo) {
        return (topo.has_id() && topo.id() == id);
      })) {
    // This topology_id is not in the list returned earlier.
    signalprocessing_is_supported_ = false;
    FAIL() << "WatchTopology returned " << *current_topology_id_
           << " which is not in our topology list";
  }
}

void AdminTest::WatchForTopology(fuchsia::hardware::audio::signalprocessing::TopologyId id) {
  ASSERT_TRUE(signalprocessing_is_supported_.value_or(false))
      << "signalprocessing is required for this test";
  // We only call WatchTopology if we support signalprocessing, so a topology should already be set.
  ASSERT_TRUE(current_topology_id_.has_value());
  // We should only expect a response if this represents a change. Don't call this method otherwise.
  ASSERT_NE(id, *current_topology_id_);

  // Make sure we're watching for a topology that is in the supported list.
  if (std::ranges::none_of(topologies_,
                           [id](const auto& t) { return (t.has_id() && t.id() == id); })) {
    // This topology_id is not in the list returned earlier.
    FAIL() << "We shouldn't wait for topology " << id << " which is not in our list";
    __UNREACHABLE;
  }

  signal_processing()->WatchTopology(AddCallbackUnordered(
      "signalprocessing::Reader::WatchTopology(update)",
      [this, id](fuchsia::hardware::audio::signalprocessing::Reader_WatchTopology_Result result) {
        ASSERT_TRUE(result.is_response());
        ASSERT_EQ(id, result.response().topology_id);
        current_topology_id_ = result.response().topology_id;
      }));
}

void AdminTest::FailOnWatchTopologyCompletion() {
  signal_processing()->WatchTopology(AddUnexpectedCallback("unexpected WatchTopology completion"));
}

void AdminTest::SetAllTopologies() {
  ASSERT_FALSE(topologies_.empty());
  ASSERT_TRUE(initial_topology_id_.has_value());
  ASSERT_TRUE(current_topology_id_.has_value());
  ASSERT_EQ(current_topology_id_, initial_topology_id_);

  if (topologies_.size() == 1) {
    GTEST_SKIP() << "Device has only one topology; we cannot test changing it";
    __UNREACHABLE;
  }

  // Ensure that all the supported topologies can be set.
  for (const auto& t : topologies_) {
    ASSERT_TRUE(t.has_id());
    // Try them all, except the current one (which _should_ be the initial topology).
    if (t.id() == *initial_topology_id_) {
      continue;
    }
    SetTopologyAndExpectCallback(t.id());
  }

  // Now restore the initial topology.
  SetTopologyAndExpectCallback(*initial_topology_id_);
}

void AdminTest::SetTopologyAndExpectCallback(
    fuchsia::hardware::audio::signalprocessing::TopologyId id) {
  ASSERT_NE(id, current_topology_id_);
  ASSERT_FALSE(pending_set_topology_id_.has_value());
  pending_set_topology_id_ = id;
  signal_processing()->SetTopology(
      id,
      AddCallbackUnordered(
          "SetTopology(" + std::to_string(id) + ")",
          [this, id](fuchsia::hardware::audio::signalprocessing::SignalProcessing_SetTopology_Result
                         result) {
            ASSERT_FALSE(result.is_err()) << "SetTopology(" << id << ") failed";
            ASSERT_TRUE(pending_set_topology_id_.has_value());
            ASSERT_EQ(*pending_set_topology_id_, id);
            pending_set_topology_id_.reset();
          }));
  ASSERT_TRUE(signal_processing().is_bound()) << "SignalProcessing failed to set the topology";
  WatchForTopology(id);
  ExpectCallbacks();
}

void AdminTest::WatchTopologyAndExpectDisconnect(zx_status_t expected_error) {
  FailOnWatchTopologyCompletion();
  ExpectError(signal_processing(), expected_error);
  CooldownAfterSignalProcessingDisconnect();
}

void AdminTest::SetUnknownTopologyAndExpectError() {
  RequestTopologies();
  ASSERT_FALSE(pending_set_topology_id_.has_value());

  fuchsia::hardware::audio::signalprocessing::TopologyId unknown_id = 0;
  while (std::ranges::any_of(topologies_, [unknown_id](const auto& topo) {
    return topo.has_id() && topo.id() == unknown_id;
  })) {
    ++unknown_id;
  }

  pending_set_topology_id_ = unknown_id;
  signal_processing()->SetTopology(
      unknown_id,
      AddCallback(
          "SetTopology(" + std::to_string(unknown_id) + ")",
          [this, unknown_id](
              fuchsia::hardware::audio::signalprocessing::SignalProcessing_SetTopology_Result
                  result) {
            pending_set_topology_id_.reset();
            ASSERT_TRUE(result.is_err()) << "SetTopology(" << unknown_id << ") did not fail";
            ASSERT_EQ(result.err(), ZX_ERR_INVALID_ARGS);
          }));
  ASSERT_TRUE(signal_processing().is_bound())
      << "SignalProcessing disconnected after setting an unknown topology";
  FailOnWatchTopologyCompletion();
  ExpectCallbacks();
}

// Validate that the collection of element IDs found in the topology list are complete and correct.
void AdminTest::ValidateElementTopologyClosure() {
  ASSERT_FALSE(elements().empty());
  std::unordered_set<fhasp::ElementId> unused_element_ids;
  for (const auto& element : elements()) {
    unused_element_ids.insert(element.id());
  }
  const std::unordered_set<fhasp::ElementId> all_element_ids = unused_element_ids;

  ASSERT_FALSE(topologies().empty());
  for (const auto& topology : topologies()) {
    std::unordered_set<fhasp::ElementId> edge_source_ids;
    std::unordered_set<fhasp::ElementId> edge_dest_ids;
    for (const auto& edge_pair : topology.processing_elements_edge_pairs()) {
      ASSERT_TRUE(all_element_ids.contains(edge_pair.processing_element_id_from))
          << "Topology " << topology.id() << " contains unknown element "
          << edge_pair.processing_element_id_from;
      ASSERT_TRUE(all_element_ids.contains(edge_pair.processing_element_id_to))
          << "Topology " << topology.id() << " contains unknown element "
          << edge_pair.processing_element_id_to;
      unused_element_ids.erase(edge_pair.processing_element_id_from);
      unused_element_ids.erase(edge_pair.processing_element_id_to);
      edge_source_ids.insert(edge_pair.processing_element_id_from);
      edge_dest_ids.insert(edge_pair.processing_element_id_to);
    }
    for (const auto& source_id : edge_source_ids) {
      fhasp::ElementType source_element_type;
      for (const auto& element : elements()) {
        if (element.id() == source_id) {
          source_element_type = element.type();
        }
      }
      if (edge_dest_ids.contains(source_id)) {
        if constexpr (!kTolerateNonTerminalDaiEndpoints) {
          ASSERT_NE(source_element_type, fhasp::ElementType::DAI_INTERCONNECT)
              << "Element " << source_id << " is not an endpoint in topology " << topology.id()
              << ", but is DAI_INTERCONNECT";
        }
        ASSERT_NE(source_element_type, fhasp::ElementType::RING_BUFFER)
            << "Element " << source_id << " is not an endpoint in topology " << topology.id()
            << ", but is RING_BUFFER";
        edge_dest_ids.erase(source_id);
      } else {
        ASSERT_TRUE(source_element_type == fhasp::ElementType::DAI_INTERCONNECT ||
                    source_element_type == fhasp::ElementType::RING_BUFFER)
            << "Element " << source_id << " is a terminal (source) endpoint in topology "
            << topology.id() << ", but is neither DAI_INTERCONNECT nor RING_BUFFER";
      }
    }
    for (const auto& dest_id : edge_dest_ids) {
      fhasp::ElementType dest_element_type;
      for (const auto& element : elements()) {
        if (element.id() == dest_id) {
          dest_element_type = element.type();
        }
      }
      ASSERT_TRUE(dest_element_type == fhasp::ElementType::DAI_INTERCONNECT ||
                  dest_element_type == fhasp::ElementType::RING_BUFFER)
          << "Element " << dest_id << " is a terminal (destination) endpoint in topology "
          << topology.id() << ", but is neither DAI_INTERCONNECT nor RING_BUFFER";
    }
  }
  ASSERT_TRUE(unused_element_ids.empty())
      << unused_element_ids.size() << "elements (including id " << *unused_element_ids.cbegin()
      << ") were not referenced in any topology";
}

bool AdminTest::ValidateElement(
    const fuchsia::hardware::audio::signalprocessing::Element& element) {
  if (!element.has_id()) {
    ADD_FAILURE() << "ElementId is a required field";
    return false;
  }
  if (!element.has_type()) {
    ADD_FAILURE() << "ElementType is a required field";
    return false;
  }
  bool should_have_type_specific =
      (element.type() ==
           fuchsia::hardware::audio::signalprocessing::ElementType::DAI_INTERCONNECT ||
       element.type() == fuchsia::hardware::audio::signalprocessing::ElementType::DYNAMICS ||
       element.type() == fuchsia::hardware::audio::signalprocessing::ElementType::EQUALIZER ||
       element.type() == fuchsia::hardware::audio::signalprocessing::ElementType::GAIN ||
       element.type() == fuchsia::hardware::audio::signalprocessing::ElementType::VENDOR_SPECIFIC);
  if (element.has_type_specific() != should_have_type_specific) {
    ADD_FAILURE() << "ElementTypeSpecific should " << (should_have_type_specific ? "" : "not")
                  << " be set, for this ElementType";
    return false;
  }
  return true;
}

void AdminTest::ValidateDaiElements() {
  ASSERT_FALSE(elements().empty());
  bool found_one = false;
  for (const auto& element : elements()) {
    ASSERT_TRUE(element.has_type());
    if (element.type() ==
        fuchsia::hardware::audio::signalprocessing::ElementType::DAI_INTERCONNECT) {
      ASSERT_TRUE(element.has_type_specific());
      ASSERT_TRUE(element.type_specific().is_dai_interconnect());
      ASSERT_TRUE(element.type_specific().dai_interconnect().has_plug_detect_capabilities());
    }
  }
  if (!found_one) {
    GTEST_SKIP() << "No DAI_INTERCONNECT elements found";
  }
}

void AdminTest::ValidateDynamicsElements() {
  ASSERT_FALSE(elements().empty());
  bool found_one = false;
  for (const auto& element : elements()) {
    ASSERT_TRUE(element.has_type());
    if (element.type() == fuchsia::hardware::audio::signalprocessing::ElementType::DYNAMICS) {
      ASSERT_TRUE(element.has_type_specific());
      ASSERT_TRUE(element.type_specific().is_dynamics());
      ASSERT_TRUE(element.type_specific().dynamics().has_bands());
      ASSERT_FALSE(element.type_specific().dynamics().bands().empty());
      for (const auto& band : element.type_specific().dynamics().bands()) {
        ASSERT_TRUE(band.has_id());
      }
    }
  }
  if (!found_one) {
    GTEST_SKIP() << "No DYNAMICS elements found";
  }
}

void AdminTest::ValidateEqualizerElements() {
  ASSERT_FALSE(elements().empty());
  bool found_one = false;
  for (const auto& element : elements()) {
    ASSERT_TRUE(element.has_type());
    if (element.type() == fuchsia::hardware::audio::signalprocessing::ElementType::EQUALIZER) {
      ASSERT_TRUE(element.has_type_specific());
      ASSERT_TRUE(element.type_specific().is_equalizer());
      const auto& eq = element.type_specific().equalizer();

      ASSERT_TRUE(eq.has_bands());
      ASSERT_FALSE(eq.bands().empty());
      for (const auto& band : element.type_specific().equalizer().bands()) {
        ASSERT_TRUE(band.has_id());
      }

      ASSERT_TRUE(eq.has_min_frequency());
      ASSERT_TRUE(eq.has_max_frequency());
      ASSERT_LE(eq.min_frequency(), eq.max_frequency());
      ASSERT_TRUE(!eq.has_max_q() || (std::isfinite(eq.max_q()) && eq.max_q() > 0.0f));

      bool requires_gain_db_vals =
          eq.has_supported_controls() && (fuchsia::hardware::audio::signalprocessing::
                                              EqualizerSupportedControls::SUPPORTS_TYPE_PEAK ||
                                          fuchsia::hardware::audio::signalprocessing::
                                              EqualizerSupportedControls::SUPPORTS_TYPE_LOW_SHELF ||
                                          fuchsia::hardware::audio::signalprocessing::
                                              EqualizerSupportedControls::SUPPORTS_TYPE_HIGH_SHELF);
      ASSERT_TRUE(
          !requires_gain_db_vals ||
          (eq.has_min_gain_db() && std::isfinite(eq.min_gain_db()) && eq.min_gain_db() > 0.0f));
      ASSERT_TRUE(
          !requires_gain_db_vals ||
          (eq.has_max_gain_db() && std::isfinite(eq.max_gain_db()) && eq.max_gain_db() > 0.0f));
      ASSERT_TRUE(!eq.has_min_gain_db() || !eq.has_max_gain_db() ||
                  eq.min_gain_db() <= eq.max_gain_db());
    }
  }
  if (!found_one) {
    GTEST_SKIP() << "No EQUALIZER elements found";
  }
}

void AdminTest::ValidateGainElements() {
  ASSERT_FALSE(elements().empty());
  bool found_one = false;
  for (const auto& element : elements()) {
    ASSERT_TRUE(element.has_type());
    if (element.type() == fuchsia::hardware::audio::signalprocessing::ElementType::GAIN) {
      ASSERT_TRUE(element.has_type_specific());
      ASSERT_TRUE(element.type_specific().is_gain());
      ASSERT_TRUE(element.type_specific().gain().has_type());

      ASSERT_TRUE(element.type_specific().gain().has_min_gain());
      ASSERT_TRUE(std::isfinite(element.type_specific().gain().min_gain()));

      ASSERT_TRUE(element.type_specific().gain().has_max_gain());
      ASSERT_TRUE(std::isfinite(element.type_specific().gain().max_gain()));
      ASSERT_LE(element.type_specific().gain().min_gain(),
                element.type_specific().gain().max_gain());

      ASSERT_TRUE(element.type_specific().gain().has_min_gain_step());
      ASSERT_TRUE(std::isfinite(element.type_specific().gain().min_gain_step()));
      ASSERT_LE(
          element.type_specific().gain().min_gain_step(),
          element.type_specific().gain().max_gain() - element.type_specific().gain().min_gain());
      ASSERT_GE(element.type_specific().gain().min_gain_step(), 0.0f);
    }
  }
  if (!found_one) {
    GTEST_SKIP() << "No GAIN elements found";
  }
}

void AdminTest::ValidateVendorSpecificElements() {
  ASSERT_FALSE(elements().empty());
  bool found_one = false;
  for (const auto& element : elements()) {
    ASSERT_TRUE(element.has_type());
    if (element.type() ==
        fuchsia::hardware::audio::signalprocessing::ElementType::VENDOR_SPECIFIC) {
      ASSERT_TRUE(element.has_type_specific());
      ASSERT_TRUE(element.type_specific().is_vendor_specific());
    }
  }
  if (!found_one) {
    GTEST_SKIP() << "No VENDOR_SPECIFIC elements found";
  }
}

void AdminTest::ValidateElements() {
  ASSERT_FALSE(elements().empty());
  for (const auto& element : elements()) {
    if (!ValidateElement(element)) {
      break;
    }
  }
}

#define DEFINE_ADMIN_TEST_CLASS(CLASS_NAME, CODE)                               \
  class CLASS_NAME : public AdminTest {                                         \
   public:                                                                      \
    explicit CLASS_NAME(const DeviceEntry& dev_entry) : AdminTest(dev_entry) {} \
    void TestBody() override { CODE }                                           \
  }

//
// Test cases that target each of the various admin commands
//
// Any case not ending in disconnect/error should WaitForError, in case the channel disconnects.

// Verify the driver responds to the GetHealthState query.
DEFINE_ADMIN_TEST_CLASS(CompositeHealth, { RequestHealthAndExpectHealthy(); });

// Verify a valid unique_id, manufacturer, product are successfully received.
DEFINE_ADMIN_TEST_CLASS(CompositeProperties, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ValidateProperties();
  WaitForError();
});

// Verify that a valid element list is successfully received.
DEFINE_ADMIN_TEST_CLASS(GetElements, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestElements());
  if (kDisplayElementsAndTopologies) {
    DisplayElements(elements());
  }
  ASSERT_NO_FAILURE_OR_SKIP(ValidateElements());
  WaitForError();
});

DEFINE_ADMIN_TEST_CLASS(DaiElements, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestElements());
  ASSERT_NO_FAILURE_OR_SKIP(ValidateDaiElements());
  WaitForError();
});

DEFINE_ADMIN_TEST_CLASS(DynamicsElements, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestElements());
  ASSERT_NO_FAILURE_OR_SKIP(ValidateDynamicsElements());
  WaitForError();
});

DEFINE_ADMIN_TEST_CLASS(EqualizerElements, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestElements());
  ASSERT_NO_FAILURE_OR_SKIP(ValidateEqualizerElements());
  WaitForError();
});

DEFINE_ADMIN_TEST_CLASS(GainElements, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestElements());
  ASSERT_NO_FAILURE_OR_SKIP(ValidateGainElements());
  WaitForError();
});

DEFINE_ADMIN_TEST_CLASS(VendorSpecificElements, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestElements());
  ASSERT_NO_FAILURE_OR_SKIP(ValidateVendorSpecificElements());
  WaitForError();
});

// Verify that a valid topology list is successfully received.
DEFINE_ADMIN_TEST_CLASS(GetTopologies, {
  RequestTopologies();
  if (kDisplayElementsAndTopologies) {
    DisplayTopologies(topologies());
  }
  WaitForError();
});

// Verify that a valid topology is successfully received.
DEFINE_ADMIN_TEST_CLASS(InitialTopology, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestTopologies());
  RetrieveInitialTopology();
  WaitForError();
});

// Verify that a valid topology is successfully received.
DEFINE_ADMIN_TEST_CLASS(SetTopology, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestTopologies());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveInitialTopology());

  SetAllTopologies();
  WaitForError();
});

// All elements should be in at least one topology, all topology elements should be known.
DEFINE_ADMIN_TEST_CLASS(ElementTopologyClosure, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestElements());
  ASSERT_NO_FAILURE_OR_SKIP(RequestTopologies());

  ValidateElementTopologyClosure();
  WaitForError();
});

// Verify that an unknown topology causes an error but does not disconnect.
DEFINE_ADMIN_TEST_CLASS(SetUnknownTopologyShouldError, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestTopologies());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveInitialTopology());

  SetUnknownTopologyAndExpectError();  // ... but we should remain connected.
  WaitForError();
});

// Verify that an unknown topology causes an error but does not disconnect.
DEFINE_ADMIN_TEST_CLASS(WatchTopologyWhilePendingShouldDisconnect, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestTopologies());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveInitialTopology());

  ASSERT_NO_FAILURE_OR_SKIP(FailOnWatchTopologyCompletion());
  ASSERT_NO_FAILURE_OR_SKIP(WaitForError());

  WatchTopologyAndExpectDisconnect(ZX_ERR_BAD_STATE);
});

// Verify that format-retrieval responses are successfully received and are complete and valid.
DEFINE_ADMIN_TEST_CLASS(CompositeRingBufferFormats, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  WaitForError();
});

// Verify that format-retrieval responses are successfully received and are complete and valid.
DEFINE_ADMIN_TEST_CLASS(CompositeDaiFormats, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveDaiFormats());
  WaitForError();
});

// Verify that a Reset() returns a valid completion.
DEFINE_ADMIN_TEST_CLASS(Reset, {
  ResetAndExpectResponse();
  WaitForError();
});

// Start-while-started should always succeed, so we test this twice.
DEFINE_ADMIN_TEST_CLASS(CodecStart, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestCodecStartAndExpectResponse());

  RequestCodecStartAndExpectResponse();
  WaitForError();
});

// Stop-while-stopped should always succeed, so we call Stop twice.
DEFINE_ADMIN_TEST_CLASS(CodecStop, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestCodecStopAndExpectResponse());

  RequestCodecStopAndExpectResponse();
  WaitForError();
});

// Verify valid responses: ring buffer properties
DEFINE_ADMIN_TEST_CLASS(GetRingBufferProperties, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());

  RequestRingBufferProperties();
  WaitForError();
});

// Verify valid responses: get ring buffer VMO.
DEFINE_ADMIN_TEST_CLASS(GetBuffer, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());

  RequestBuffer(100);
  WaitForError();
});

// Clients request minimum VMO sizes for their requirements, and drivers must respond with VMOs that
// satisfy those requests as well as their own constraints for proper operation. A driver or device
// reads/writes a ring buffer in batches, so it must reserve part of the ring buffer for safe
// copying. This test case validates that drivers set aside a non-zero amount of their ring buffers.
//
// Many drivers automatically "round up" their VMO to a memory page boundary, regardless of space
// needed for proper DMA. To factor this out, here the client requests enough frames to exactly fill
// an integral number of memory pages. The driver should nonetheless return a larger buffer.
DEFINE_ADMIN_TEST_CLASS(DriverReservesRingBufferSpace, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());

  uint32_t page_frame_aligned_rb_frames =
      std::lcm<uint32_t>(frame_size(), PAGE_SIZE) / frame_size();
  FX_LOGS(DEBUG) << "frame_size is " << frame_size() << ", requesting a ring buffer of "
                 << page_frame_aligned_rb_frames << " frames";
  RequestBuffer(page_frame_aligned_rb_frames);
  WaitForError();

  // Calculate the driver's needed ring-buffer space, from retrieved fifo_size|safe_offset values.
  EXPECT_GT(ring_buffer_frames(), page_frame_aligned_rb_frames);
});

// Verify valid responses: set active channels returns a set_time after the call is made.
DEFINE_ADMIN_TEST_CLASS(SetActiveChannelsChange, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());

  uint64_t all_channels_mask = (1 << ring_buffer_pcm_format().number_of_channels) - 1;
  ASSERT_NO_FAILURE_OR_SKIP(
      ActivateChannelsAndExpectOutcome(all_channels_mask, SetActiveChannelsOutcome::SUCCESS));

  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(8000));
  ASSERT_NO_FAILURE_OR_SKIP(ActivateChannelsAndExpectOutcome(0, SetActiveChannelsOutcome::CHANGE));

  WaitForError();
});

// If no change, the previous set-time should be returned.
DEFINE_ADMIN_TEST_CLASS(SetActiveChannelsNoChange, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));

  uint64_t all_channels_mask = (1 << ring_buffer_pcm_format().number_of_channels) - 1;
  ASSERT_NO_FAILURE_OR_SKIP(
      ActivateChannelsAndExpectOutcome(all_channels_mask, SetActiveChannelsOutcome::SUCCESS));

  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStartAndExpectCallback());
  ASSERT_NO_FAILURE_OR_SKIP(
      ActivateChannelsAndExpectOutcome(all_channels_mask, SetActiveChannelsOutcome::NO_CHANGE));

  RequestRingBufferStopAndExpectCallback();
  WaitForError();
});

// Verify an invalid input (out of range) for SetActiveChannels.
DEFINE_ADMIN_TEST_CLASS(SetActiveChannelsTooHigh, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());

  auto channel_mask_too_high = (1 << ring_buffer_pcm_format().number_of_channels);
  ActivateChannelsAndExpectOutcome(channel_mask_too_high, SetActiveChannelsOutcome::FAILURE);
});

// Verify that valid start responses are received.
DEFINE_ADMIN_TEST_CLASS(RingBufferStart, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(32000));

  RequestRingBufferStartAndExpectCallback();
  WaitForError();
});

// ring-buffer FIDL channel should disconnect, with ZX_ERR_BAD_STATE
DEFINE_ADMIN_TEST_CLASS(RingBufferStartBeforeGetVmoShouldDisconnect, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());

  RequestRingBufferStartAndExpectDisconnect(ZX_ERR_BAD_STATE);
});

// ring-buffer FIDL channel should disconnect, with ZX_ERR_BAD_STATE
DEFINE_ADMIN_TEST_CLASS(RingBufferStartWhileStartingShouldDisconnect, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(8000));

  // Don't wait for the reported start time, or even the callback itself...
  auto _ = RequestRingBufferStart();
  // ...just immediately call again.
  RequestRingBufferStartAndExpectDisconnect(ZX_ERR_BAD_STATE);
});

// ring-buffer FIDL channel should disconnect, with ZX_ERR_BAD_STATE
DEFINE_ADMIN_TEST_CLASS(RingBufferStartWhileStartedShouldDisconnect, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(8000));
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStartAndExpectCallback());
  ASSERT_NO_FAILURE_OR_SKIP(WaitUntilAfterStartTime());

  RequestRingBufferStartAndExpectDisconnect(ZX_ERR_BAD_STATE);
});

// Verify that valid stop responses are received.
DEFINE_ADMIN_TEST_CLASS(RingBufferStop, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStartAndExpectCallback());

  RequestRingBufferStopAndExpectCallback();
  WaitForError();
});

// ring-buffer FIDL channel should disconnect, with ZX_ERR_BAD_STATE
DEFINE_ADMIN_TEST_CLASS(RingBufferStopBeforeGetVmoShouldDisconnect, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());

  RequestRingBufferStopAndExpectDisconnect(ZX_ERR_BAD_STATE);
});

DEFINE_ADMIN_TEST_CLASS(RingBufferStopWhileStoppedIsPermitted, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStopAndExpectCallback());

  RequestRingBufferStopAndExpectCallback();
  WaitForError();
});

// Verify valid WatchDelayInfo internal_delay responses.
DEFINE_ADMIN_TEST_CLASS(InternalDelayIsValid, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());

  WatchDelayAndExpectUpdate();
  ValidateInternalDelay();
  WaitForError();
});

// Verify valid WatchDelayInfo external_delay response.
DEFINE_ADMIN_TEST_CLASS(ExternalDelayIsValid, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());

  WatchDelayAndExpectUpdate();
  ValidateExternalDelay();
  WaitForError();
});

// Verify valid responses: WatchDelayInfo does NOT respond a second time.
DEFINE_ADMIN_TEST_CLASS(GetDelayInfoSecondTimeNoResponse, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());

  WatchDelayAndExpectUpdate();
  WatchDelayAndExpectNoUpdate();

  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(8000));
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStartAndExpectCallback());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStopAndExpectCallback());

  WaitForError();
});

// Verify that valid WatchDelayInfo responses are received, even after RingBufferStart().
DEFINE_ADMIN_TEST_CLASS(GetDelayInfoAfterStart, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStartAndExpectCallback());

  WatchDelayAndExpectUpdate();
  WaitForError();
});

DEFINE_ADMIN_TEST_CLASS(PositionNotifyBeforeStart, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(8000));

  ASSERT_NO_FAILURE_OR_SKIP(ExpectNoPositionNotifications());

  RequestPositionNotification();
  WaitForError();
});

DEFINE_ADMIN_TEST_CLASS(PositionNotifyNone, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(8000, 0));

  ASSERT_NO_FAILURE_OR_SKIP(ExpectNoPositionNotifications());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStartAndExpectCallback());

  RequestPositionNotification();
  WaitForError();
});

DEFINE_ADMIN_TEST_CLASS(PositionNotifyAfterStop, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(8000));

  ASSERT_NO_FAILURE_OR_SKIP(ExpectNoPositionNotifications());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStartAndExpectCallback());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStopAndExpectCallback());

  RequestPositionNotification();
  WaitForError();
});

// Create a RingBuffer, drop it, recreate it, then interact with it in any way (e.g. GetProperties).
DEFINE_ADMIN_TEST_CLASS(GetRingBufferPropertiesAfterDroppingFirstRingBuffer, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(DropRingBuffer());

  // Dropped first ring buffer, creating second one.
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());

  RequestRingBufferProperties();
  WaitForError();
});

// Create RingBuffer, fully exercise it, drop it, recreate it, then validate GetDelayInfo.
DEFINE_ADMIN_TEST_CLASS(GetDelayInfoAfterDroppingFirstRingBuffer, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(WatchDelayAndExpectUpdate());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(WatchDelayAndExpectNoUpdate());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStartAndExpectCallback());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStopAndExpectCallback());
  ASSERT_NO_FAILURE_OR_SKIP(DropRingBuffer());

  // Dropped first ring buffer, creating second one, reverifying WatchDelayInfo.
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(WatchDelayAndExpectUpdate());

  WatchDelayAndExpectNoUpdate();
  WaitForError();
});

// Create RingBuffer, fully exercise it, drop it, recreate it, then validate SetActiveChannels.
DEFINE_ADMIN_TEST_CLASS(SetActiveChannelsAfterDroppingFirstRingBuffer, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));

  uint64_t all_channels_mask = (1 << ring_buffer_pcm_format().number_of_channels) - 1;
  ASSERT_NO_FAILURE_OR_SKIP(
      ActivateChannelsAndExpectOutcome(all_channels_mask, SetActiveChannelsOutcome::SUCCESS));
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStartAndExpectCallback());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStopAndExpectCallback());
  ASSERT_NO_FAILURE_OR_SKIP(DropRingBuffer());

  // Dropped first ring buffer, creating second one, reverifying SetActiveChannels.
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStartAndExpectCallback());
  ASSERT_NO_FAILURE_OR_SKIP(ActivateChannelsAndExpectOutcome(0, SetActiveChannelsOutcome::SUCCESS));

  RequestRingBufferStopAndExpectCallback();
  WaitForError();
});

// Register separate test case instances for each enumerated device.
//
// See googletest/docs/advanced.md for details.
#define REGISTER_ADMIN_TEST(CLASS_NAME, DEVICE)                                              \
  testing::RegisterTest("AdminTest", TestNameForEntry(#CLASS_NAME, DEVICE).c_str(), nullptr, \
                        DevNameForEntry(DEVICE).c_str(), __FILE__, __LINE__,                 \
                        [&]() -> AdminTest* { return new CLASS_NAME(DEVICE); })

#define REGISTER_DISABLED_ADMIN_TEST(CLASS_NAME, DEVICE)                                       \
  testing::RegisterTest(                                                                       \
      "AdminTest", (std::string("DISABLED_") + TestNameForEntry(#CLASS_NAME, DEVICE)).c_str(), \
      nullptr, DevNameForEntry(DEVICE).c_str(), __FILE__, __LINE__,                            \
      [&]() -> AdminTest* { return new CLASS_NAME(DEVICE); })

void RegisterAdminTestsForDevice(const DeviceEntry& device_entry) {
  if (device_entry.isCodec()) {
    REGISTER_ADMIN_TEST(Reset, device_entry);

    REGISTER_ADMIN_TEST(CodecStop, device_entry);
    REGISTER_ADMIN_TEST(CodecStart, device_entry);
  } else if (device_entry.isComposite()) {
    // Composite test cases
    //
    REGISTER_ADMIN_TEST(CompositeHealth, device_entry);
    REGISTER_ADMIN_TEST(CompositeProperties, device_entry);
    REGISTER_ADMIN_TEST(CompositeRingBufferFormats, device_entry);
    REGISTER_ADMIN_TEST(CompositeDaiFormats, device_entry);
    // TODO(https://fxbug.dev/42075676): Add Composite testing (e.g. Reset, SetDaiFormat).
    // REGISTER_ADMIN_TEST(SetDaiFormat, device_entry); // test all DAIs, not just the first.
    // Reset should close RingBuffers and revert SetTopology, SetElementState and SetDaiFormat.
    // REGISTER_ADMIN_TEST(CompositeReset, device_entry);

    // signalprocessing Element test cases
    //
    REGISTER_ADMIN_TEST(GetElements, device_entry);
    REGISTER_ADMIN_TEST(DaiElements, device_entry);
    REGISTER_ADMIN_TEST(DynamicsElements, device_entry);
    REGISTER_ADMIN_TEST(EqualizerElements, device_entry);
    REGISTER_ADMIN_TEST(GainElements, device_entry);
    REGISTER_ADMIN_TEST(VendorSpecificElements, device_entry);

    // signalprocessing Topology test cases
    //
    REGISTER_ADMIN_TEST(GetTopologies, device_entry);
    REGISTER_ADMIN_TEST(ElementTopologyClosure, device_entry);
    REGISTER_ADMIN_TEST(InitialTopology, device_entry);
    REGISTER_ADMIN_TEST(SetTopology, device_entry);
    REGISTER_ADMIN_TEST(SetUnknownTopologyShouldError, device_entry);
    REGISTER_ADMIN_TEST(WatchTopologyWhilePendingShouldDisconnect, device_entry);

    // signalprocessing ElementState test cases
    //
    // TODO(https://fxbug.dev/42077405): Add testing for SignalProcessing ElementState methods.

    // RingBuffer test cases
    //
    // TODO(https://fxbug.dev/42075676): Add Composite testing (all RingBuffers, not just first).
    REGISTER_ADMIN_TEST(GetRingBufferProperties, device_entry);
    REGISTER_ADMIN_TEST(GetBuffer, device_entry);
    REGISTER_ADMIN_TEST(DriverReservesRingBufferSpace, device_entry);

    REGISTER_ADMIN_TEST(InternalDelayIsValid, device_entry);
    REGISTER_ADMIN_TEST(ExternalDelayIsValid, device_entry);
    REGISTER_ADMIN_TEST(GetDelayInfoSecondTimeNoResponse, device_entry);

    REGISTER_ADMIN_TEST(SetActiveChannelsChange, device_entry);
    REGISTER_ADMIN_TEST(SetActiveChannelsTooHigh, device_entry);
    REGISTER_ADMIN_TEST(SetActiveChannelsNoChange, device_entry);

    REGISTER_ADMIN_TEST(PositionNotifyBeforeStart, device_entry);
    REGISTER_ADMIN_TEST(PositionNotifyNone, device_entry);
    REGISTER_ADMIN_TEST(PositionNotifyAfterStop, device_entry);

    REGISTER_ADMIN_TEST(RingBufferStart, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStartBeforeGetVmoShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStartWhileStartingShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStartWhileStartedShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(GetDelayInfoAfterStart, device_entry);

    REGISTER_ADMIN_TEST(RingBufferStop, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStopBeforeGetVmoShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStopWhileStoppedIsPermitted, device_entry);

    REGISTER_ADMIN_TEST(GetRingBufferPropertiesAfterDroppingFirstRingBuffer, device_entry);
    REGISTER_ADMIN_TEST(GetDelayInfoAfterDroppingFirstRingBuffer, device_entry);
    REGISTER_ADMIN_TEST(SetActiveChannelsAfterDroppingFirstRingBuffer, device_entry);

  } else if (device_entry.isDai()) {
    REGISTER_ADMIN_TEST(GetRingBufferProperties, device_entry);
    REGISTER_ADMIN_TEST(GetBuffer, device_entry);
    REGISTER_ADMIN_TEST(DriverReservesRingBufferSpace, device_entry);

    REGISTER_ADMIN_TEST(InternalDelayIsValid, device_entry);
    REGISTER_ADMIN_TEST(ExternalDelayIsValid, device_entry);
    REGISTER_ADMIN_TEST(GetDelayInfoSecondTimeNoResponse, device_entry);

    REGISTER_ADMIN_TEST(SetActiveChannelsChange, device_entry);
    REGISTER_ADMIN_TEST(SetActiveChannelsTooHigh, device_entry);
    REGISTER_ADMIN_TEST(SetActiveChannelsNoChange, device_entry);

    REGISTER_ADMIN_TEST(PositionNotifyBeforeStart, device_entry);
    REGISTER_ADMIN_TEST(PositionNotifyNone, device_entry);
    REGISTER_ADMIN_TEST(PositionNotifyAfterStop, device_entry);

    REGISTER_ADMIN_TEST(RingBufferStart, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStartBeforeGetVmoShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStartWhileStartingShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStartWhileStartedShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(GetDelayInfoAfterStart, device_entry);

    REGISTER_ADMIN_TEST(RingBufferStop, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStopBeforeGetVmoShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStopWhileStoppedIsPermitted, device_entry);

    REGISTER_ADMIN_TEST(GetRingBufferPropertiesAfterDroppingFirstRingBuffer, device_entry);
    REGISTER_ADMIN_TEST(GetDelayInfoAfterDroppingFirstRingBuffer, device_entry);
    REGISTER_ADMIN_TEST(SetActiveChannelsAfterDroppingFirstRingBuffer, device_entry);
  } else if (device_entry.isStreamConfig()) {
    REGISTER_ADMIN_TEST(GetRingBufferProperties, device_entry);
    REGISTER_ADMIN_TEST(GetBuffer, device_entry);
    REGISTER_ADMIN_TEST(DriverReservesRingBufferSpace, device_entry);

    REGISTER_ADMIN_TEST(InternalDelayIsValid, device_entry);
    REGISTER_ADMIN_TEST(ExternalDelayIsValid, device_entry);
    REGISTER_ADMIN_TEST(GetDelayInfoSecondTimeNoResponse, device_entry);

    REGISTER_ADMIN_TEST(SetActiveChannelsChange, device_entry);
    REGISTER_ADMIN_TEST(SetActiveChannelsTooHigh, device_entry);
    REGISTER_ADMIN_TEST(SetActiveChannelsNoChange, device_entry);

    REGISTER_ADMIN_TEST(PositionNotifyBeforeStart, device_entry);
    REGISTER_ADMIN_TEST(PositionNotifyNone, device_entry);
    REGISTER_ADMIN_TEST(PositionNotifyAfterStop, device_entry);

    REGISTER_ADMIN_TEST(RingBufferStart, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStartBeforeGetVmoShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStartWhileStartingShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStartWhileStartedShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(GetDelayInfoAfterStart, device_entry);

    REGISTER_ADMIN_TEST(RingBufferStop, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStopBeforeGetVmoShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStopWhileStoppedIsPermitted, device_entry);

    REGISTER_ADMIN_TEST(GetRingBufferPropertiesAfterDroppingFirstRingBuffer, device_entry);
    REGISTER_ADMIN_TEST(GetDelayInfoAfterDroppingFirstRingBuffer, device_entry);
    REGISTER_ADMIN_TEST(SetActiveChannelsAfterDroppingFirstRingBuffer, device_entry);
  } else {
    FAIL() << "Unknown device type";
  }
}

// TODO(https://fxbug.dev/42075676): Add remaining tests for Codec protocol methods.
//
// SetDaiFormatUnsupported
//    Codec::SetDaiFormat with bad format returns the expected ZX_ERR_INVALID_ARGS.
//    Codec should still be usable (protocol channel still open), after an error is returned.
// SetDaiFormatWhileUnplugged (not testable in automated environment)

// TODO(https://fxbug.dev/302704556): Add Watch-while-still-pending tests for delay and position.

// TODO(https://fxbug.dev/42075676): Add testing for Composite protocol methods.

// TODO(https://fxbug.dev/42077405): Add remaining testing for SignalProcessing methods.
//
// Proposed test cases for fuchsia.hardware.audio.signalprocessing listed below:
//
// SetTopologyInvalidated
//    First make a change that invalidates the SignalProcessing configuration, then
//    (hanging) WatchTopology should ... return ZX_ERR_BAD_STATE and not close channel?
//    SetTopology should return ZX_ERR_BAD_STATE and not close channel.
// SetTopologyReconfigured
//    First invalidate the SignalProcessing configuration, then retrieve the new topologies.
//    SetTopology returns callback (does not fail or close channel).
//    WatchTopology acknowledges the change made by SetTopology.

// InitialElementState
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If WatchElementState closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other
//    error. Else set a static var for this driver instance that SignalProcessing is supported.
//    WatchElementState immediately returns when initially called.
//    Callback contains a valid complete ElementState that matches the ElementType.
// WatchElementStateBadId
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    WatchElementState(badId) returns ZX_ERR_INVALID_ARGS and does not close. Fail on other
//    error.
// TODO(https://fxbug.dev/302704556): Add Watch-while-still-pending test for WatchElementState.
// WatchElementStateWhilePending
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If WatchElementState closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other
//    error. Else set a static var for this driver instance that SignalProcessing is supported.
//    WatchElementState immediately returns when initially called.
//    WatchElementState (again) closes the protocol channel with ZX_ERR_BAD_STATE

// SetElementState
//    If SetElementState closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    SetElementState returns callback.  Any other observable state?
// SetElementStateElementSpecific
//    Detailed checks of specific input or output fields that are unique to the element type.
//    ... likely multiple test cases here, one for each ElementType
// SetElementStateNoChange
//    If SetElementState closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    SetElementState does not returns callback, trigger WatchElementStateChange or close channel.
// SetElementStateBadId
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    SetElementState(badId) returns ZX_ERR_INVALID_ARGS, not close channel. Fail on other error.
// SetElementStateBadValues
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    SetElementState(badVal) returns ZX_ERR_INVALID_ARGS, not close channel. Fail on other error.
// SetElementStateInvalidated
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    First make a change that invalidates the SignalProcessing configuration, then
//    SetElementState should return ZX_ERR_BAD_STATE and not close channel.
// SetElementStateReconfigured
//    First invalidate the SignalProcessing configuration, then retrieve new elements/topologies.
//    SetElementState returns callback (does not fail or close channel).
// WatchElementStateChange
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    WatchElementState pends until SetElementState is called.
//    Upon change, returns callback with values that match SetElementState.

}  // namespace media::audio::drivers::test
