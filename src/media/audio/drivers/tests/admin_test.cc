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

#include <gtest/gtest.h>

namespace media::audio::drivers::test {

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
    RequestTopology();

    // If a ring_buffer_id exists, request it - but don't fail if the driver has no ring buffer.
    if (ring_buffer_id().has_value()) {
      composite()->CreateRingBuffer(
          ring_buffer_id().value(), std::move(rb_format), ring_buffer_handle.NewRequest(),
          AddCallback("CreateRingBuffer",
                      [](fuchsia::hardware::audio::Composite_CreateRingBuffer_Result result) {
                        EXPECT_FALSE(result.is_err());
                      }));
      if (!composite().is_bound()) {
        FAIL() << "Composite failed to get ring buffer channel";
      }
      SetRingBufferIncoming(IsIncoming(ring_buffer_id()));
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

  ASSERT_TRUE(ring_buffer_props_) << "RequestBuffer was called before RequestRingBufferChannel";

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

// Request that the driver start the ring buffer engine, responding with the start_time.
// This method assumes that GetVmo has previously been called and we are not already started.
void AdminTest::RequestRingBufferStart() {
  ASSERT_GT(ring_buffer_frames_, 0u) << "GetVmo must be called before RingBuffer::Start()";

  // Any position notifications that arrive before RingBuffer::Start callback should cause failures.
  FailOnPositionNotifications();

  auto send_time = zx::clock::get_monotonic();
  ring_buffer_->Start(AddCallback("RingBuffer::Start", [this](int64_t start_time) {
    AllowPositionNotifications();
    start_time_ = zx::time(start_time);
  }));

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
}

// Request that driver stop the ring buffer. This assumes that GetVmo has previously been called.
void AdminTest::RequestRingBufferStop() {
  ASSERT_GT(ring_buffer_frames_, 0u) << "GetVmo must be called before RingBuffer::Stop()";
  ring_buffer_->Stop(AddCallback("RingBuffer::Stop"));

  ExpectCallbacks();
}

// Request that the driver start the ring buffer engine, but expect disconnect rather than response.
// We would expect this if calling RingBuffer::Stop before GetVmo, for example.
void AdminTest::RequestRingBufferStopAndExpectDisconnect(zx_status_t expected_error) {
  ring_buffer_->Stop(AddUnexpectedCallback("RingBuffer::Stop - expected disconnect instead"));

  ExpectError(ring_buffer(), expected_error);
}

// After RingBuffer::Stop is called, no position notification should be received.
// To validate this without any race windows: from within the next position notification itself,
// we call RingBuffer::Stop and flag that subsequent position notifications should FAIL.
void AdminTest::RequestRingBufferStopAndExpectNoPositionNotifications() {
  ring_buffer_->Stop(AddCallback("RingBuffer::Stop", [this]() { FailOnPositionNotifications(); }));

  ExpectCallbacks();
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
  ASSERT_TRUE(delay_info_->has_internal_delay());
  EXPECT_GE(delay_info_->internal_delay(), 0ll)
      << "WatchDelayInfo `internal_delay` (" << delay_info_->internal_delay()
      << ") cannot be negative";
}

// We've already validated that we received an overall response.
// External delay (if present) simply must be non-negative.
void AdminTest::ValidateExternalDelay() {
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
  zx::nanosleep(zx::deadline_after(zx::msec(100)));
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

// Verify that a Reset() returns a valid completion.
DEFINE_ADMIN_TEST_CLASS(Reset, { ResetAndExpectResponse(); });

// Start-while-started should always succeed, so we test this twice.
DEFINE_ADMIN_TEST_CLASS(CodecStart, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestCodecStartAndExpectResponse());

  RequestCodecStartAndExpectResponse();
  WaitForError();
});

// Stop-while-stopped should always succeed, so we test this twice.
DEFINE_ADMIN_TEST_CLASS(CodecStop, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestCodecStopAndExpectResponse());

  RequestCodecStopAndExpectResponse();
  WaitForError();
});

// Verify valid responses: ring buffer properties
DEFINE_ADMIN_TEST_CLASS(GetRingBufferProperties, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());

  RequestRingBufferProperties();
  WaitForError();
});

// Verify valid responses: get ring buffer VMO.
DEFINE_ADMIN_TEST_CLASS(GetBuffer, {
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
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));

  uint64_t all_channels_mask = (1 << ring_buffer_pcm_format().number_of_channels) - 1;
  ASSERT_NO_FAILURE_OR_SKIP(
      ActivateChannelsAndExpectOutcome(all_channels_mask, SetActiveChannelsOutcome::SUCCESS));

  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStart());
  ASSERT_NO_FAILURE_OR_SKIP(
      ActivateChannelsAndExpectOutcome(all_channels_mask, SetActiveChannelsOutcome::NO_CHANGE));

  RequestRingBufferStop();
  WaitForError();
});

// Verify an invalid input (out of range) for SetActiveChannels.
DEFINE_ADMIN_TEST_CLASS(SetActiveChannelsTooHigh, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());

  auto channel_mask_too_high = (1 << ring_buffer_pcm_format().number_of_channels);
  ActivateChannelsAndExpectOutcome(channel_mask_too_high, SetActiveChannelsOutcome::FAILURE);
});

// Verify that valid start responses are received.
DEFINE_ADMIN_TEST_CLASS(RingBufferStart, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(32000));

  RequestRingBufferStart();
  WaitForError();
});

// ring-buffer FIDL channel should disconnect, with ZX_ERR_BAD_STATE
DEFINE_ADMIN_TEST_CLASS(RingBufferStartBeforeGetVmoShouldDisconnect, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());

  RequestRingBufferStartAndExpectDisconnect(ZX_ERR_BAD_STATE);
});

// ring-buffer FIDL channel should disconnect, with ZX_ERR_BAD_STATE
DEFINE_ADMIN_TEST_CLASS(RingBufferStartWhileStartedShouldDisconnect, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(8000));
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStart());

  RequestRingBufferStartAndExpectDisconnect(ZX_ERR_BAD_STATE);
});

// Verify that valid stop responses are received.
DEFINE_ADMIN_TEST_CLASS(RingBufferStop, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStart());

  RequestRingBufferStop();
  WaitForError();
});

// ring-buffer FIDL channel should disconnect, with ZX_ERR_BAD_STATE
DEFINE_ADMIN_TEST_CLASS(RingBufferStopBeforeGetVmoShouldDisconnect, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());

  RequestRingBufferStopAndExpectDisconnect(ZX_ERR_BAD_STATE);
});

DEFINE_ADMIN_TEST_CLASS(RingBufferStopWhileStoppedIsPermitted, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStop());

  RequestRingBufferStop();
  WaitForError();
});

// Verify valid WatchDelayInfo internal_delay responses.
DEFINE_ADMIN_TEST_CLASS(InternalDelayIsValid, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());

  WatchDelayAndExpectUpdate();
  ValidateInternalDelay();
  WaitForError();
});

// Verify valid WatchDelayInfo external_delay response.
DEFINE_ADMIN_TEST_CLASS(ExternalDelayIsValid, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());

  WatchDelayAndExpectUpdate();
  ValidateExternalDelay();
  WaitForError();
});

// Verify valid responses: WatchDelayInfo does NOT respond a second time.
DEFINE_ADMIN_TEST_CLASS(GetDelayInfoSecondTimeNoResponse, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());

  WatchDelayAndExpectUpdate();
  WatchDelayAndExpectNoUpdate();

  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(8000));
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStart());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStop());

  WaitForError();
});

// Verify that valid WatchDelayInfo responses are received, even after RingBufferStart().
DEFINE_ADMIN_TEST_CLASS(GetDelayInfoAfterStart, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStart());

  WatchDelayAndExpectUpdate();
  WaitForError();
});

// Create a RingBuffer, drop it, recreate it, then interact with it in any way (e.g. GetProperties).
DEFINE_ADMIN_TEST_CLASS(GetRingBufferPropertiesAfterDroppingFirstRingBuffer, {
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
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(WatchDelayAndExpectUpdate());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(WatchDelayAndExpectNoUpdate());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStart());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStop());
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
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));

  uint64_t all_channels_mask = (1 << ring_buffer_pcm_format().number_of_channels) - 1;
  ASSERT_NO_FAILURE_OR_SKIP(
      ActivateChannelsAndExpectOutcome(all_channels_mask, SetActiveChannelsOutcome::SUCCESS));
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStart());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStop());
  ASSERT_NO_FAILURE_OR_SKIP(DropRingBuffer());

  // Dropped first ring buffer, creating second one, reverifying SetActiveChannels.
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferStart());
  ASSERT_NO_FAILURE_OR_SKIP(ActivateChannelsAndExpectOutcome(0, SetActiveChannelsOutcome::SUCCESS));

  RequestRingBufferStop();
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

void RegisterAdminTestsForDevice(const DeviceEntry& device_entry,
                                 bool expect_audio_svcs_not_connected) {
  // If audio_core or audio_device_registry is connected to the audio driver, admin tests will fail.
  // We test a hermetic instance of the A2DP driver, so audio services are never connected to it --
  // thus we can always run the admin tests on it.
  if (!(device_entry.isA2DP() || expect_audio_svcs_not_connected)) {
    return;
  }

  if (device_entry.isCodec()) {
    REGISTER_ADMIN_TEST(Reset, device_entry);

    REGISTER_ADMIN_TEST(CodecStop, device_entry);
    REGISTER_ADMIN_TEST(CodecStart, device_entry);
  } else if (device_entry.isComposite()) {
    REGISTER_ADMIN_TEST(GetRingBufferProperties, device_entry);
    REGISTER_ADMIN_TEST(GetBuffer, device_entry);
    REGISTER_ADMIN_TEST(DriverReservesRingBufferSpace, device_entry);

    REGISTER_ADMIN_TEST(InternalDelayIsValid, device_entry);
    REGISTER_ADMIN_TEST(ExternalDelayIsValid, device_entry);
    REGISTER_ADMIN_TEST(GetDelayInfoSecondTimeNoResponse, device_entry);

    REGISTER_ADMIN_TEST(SetActiveChannelsChange, device_entry);
    REGISTER_ADMIN_TEST(SetActiveChannelsTooHigh, device_entry);
    REGISTER_ADMIN_TEST(SetActiveChannelsNoChange, device_entry);

    REGISTER_ADMIN_TEST(RingBufferStart, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStartBeforeGetVmoShouldDisconnect, device_entry);
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

    REGISTER_ADMIN_TEST(RingBufferStart, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStartBeforeGetVmoShouldDisconnect, device_entry);
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

    REGISTER_ADMIN_TEST(RingBufferStart, device_entry);
    REGISTER_ADMIN_TEST(RingBufferStartBeforeGetVmoShouldDisconnect, device_entry);
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

// TODO(https://fxbug.dev/42077405): Add testing for SignalProcessing methods.

// TODO(https://fxbug.dev/42075676): Add more Composite testing (e.g. Reset, SetDaiFormat).

// TODO(https://fxbug.dev/302704556): Add Watch-while-still-pending tests (delay and position).

// TODO(https://fxbug.dev/42075676): Add remaining tests for Codec protocol methods.
//
// SetDaiFormatUnsupported
//    Codec::SetDaiFormat with bad format returns the expected ZX_ERR_INVALID_ARGS.
//    Codec should still be usable (protocol channel still open), after an error is returned.
// SetDaiFormatWhileUnplugged (not testable in automated environment)

}  // namespace media::audio::drivers::test
