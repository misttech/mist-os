// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/test/admin_test.h"

#include <lib/media/cpp/timeline_rate.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/vmo.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>

#include <algorithm>
#include <cstring>
#include <optional>

#include <gtest/gtest.h>

namespace media::audio::drivers::test {

void AdminTest::TearDown() {
  DropRingBuffer();

  TestBase::TearDown();
}

void AdminTest::DropRingBuffer() {
  ring_buffer_.Unbind();

  // When disconnecting a RingBuffer, there's no signal to wait on before proceeding (potentially
  // immediately executing other tests); insert a 100-ms wait. This wait is even more important for
  // error cases that cause the RingBuffer to disconnect: without it, subsequent test cases that use
  // the RingBuffer may receive unexpected errors (e.g. ZX_ERR_PEER_CLOSED or ZX_ERR_INVALID_ARGS).
  //
  // We need this wait when testing a "real hardware" driver (i.e. on realtime-capable systems). For
  // this reason a hardcoded time constant, albeit a test antipattern, is (grudgingly) acceptable.
  //
  // TODO(fxbug.dev/113683): investigate why we fail without this delay, fix the drivers/test as
  // necessary, and eliminate this workaround.
  zx::nanosleep(zx::deadline_after(zx::msec(100)));
}

// For the channelization and sample_format that we've set, determine the size of each frame.
// This method assumes that SetFormat has already been sent to the driver.
void AdminTest::CalculateFrameSize() {
  EXPECT_LE(pcm_format_.valid_bits_per_sample, pcm_format_.bytes_per_sample * 8);
  frame_size_ = pcm_format_.number_of_channels * pcm_format_.bytes_per_sample;
}

void AdminTest::RequestRingBufferChannel() {
  fuchsia::hardware::audio::Format format = {};
  format.set_pcm_format(pcm_format_);

  fidl::InterfaceHandle<fuchsia::hardware::audio::RingBuffer> ring_buffer_handle;
  stream_config()->CreateRingBuffer(std::move(format), ring_buffer_handle.NewRequest());

  zx::channel channel = ring_buffer_handle.TakeChannel();
  ring_buffer_ =
      fidl::InterfaceHandle<fuchsia::hardware::audio::RingBuffer>(std::move(channel)).Bind();

  if (!stream_config().is_bound() || !ring_buffer_.is_bound()) {
    FAIL() << "Failed to get ring buffer channel";
  }

  AddErrorHandler(ring_buffer_, "RingBuffer");

  CalculateFrameSize();
}

// Request that driver set format to the lowest bit-rate/channelization of the ranges reported.
// This method assumes that the driver has already successfully responded to a GetFormats request.
void AdminTest::RequestRingBufferChannelWithMinFormat() {
  ASSERT_GT(pcm_formats().size(), 0u);

  pcm_format_ = min_format();
  RequestRingBufferChannel();
}

// Request that driver set the highest bit-rate/channelization of the ranges reported.
// This method assumes that the driver has already successfully responded to a GetFormats request.
void AdminTest::RequestRingBufferChannelWithMaxFormat() {
  ASSERT_GT(pcm_formats().size(), 0u);

  pcm_format_ = max_format();
  RequestRingBufferChannel();
}

// Ring-buffer channel requests
//
// Request the RingBufferProperties, at the current format (relies on the ring buffer channel).
// Validate the four fields that might be returned (only one is currently required).
void AdminTest::RequestRingBufferProperties() {
  ring_buffer_->GetProperties(AddCallback(
      "RingBuffer::GetProperties", [this](fuchsia::hardware::audio::RingBufferProperties props) {
        ring_buffer_props_ = std::move(props);
      }));
  ExpectCallbacks();
  if (HasFailure()) {
    return;
  }
  ASSERT_TRUE(ring_buffer_props_.has_value()) << "No RingBufferProperties table received";

  if (ring_buffer_props_->has_external_delay()) {
    // As a zx::duration, a negative value is theoretically possible, but this is disallowed.
    EXPECT_GE(ring_buffer_props_->external_delay(), 0);
  }

  // This field is required.
  EXPECT_TRUE(ring_buffer_props_->has_needs_cache_flush_or_invalidate());

  if (ring_buffer_props_->has_turn_on_delay()) {
    // As a zx::duration, a negative value is theoretically possible, but this is disallowed.
    EXPECT_GE(ring_buffer_props_->turn_on_delay(), 0);
  }
}

// Request the ring buffer's VMO handle, at the current format (relies on the ring buffer channel).
void AdminTest::RequestBuffer(uint32_t min_ring_buffer_frames,
                              uint32_t notifications_per_ring = 0) {
  min_ring_buffer_frames_ = min_ring_buffer_frames;
  notifications_per_ring_ = notifications_per_ring;
  zx::vmo ring_buffer_vmo;
  ring_buffer_->GetVmo(
      min_ring_buffer_frames, notifications_per_ring,
      AddCallback("GetVmo", [this, &ring_buffer_vmo](
                                fuchsia::hardware::audio::RingBuffer_GetVmo_Result result) {
        EXPECT_GE(result.response().num_frames, min_ring_buffer_frames_);
        ring_buffer_frames_ = result.response().num_frames;
        ring_buffer_vmo = std::move(result.response().ring_buffer);
        EXPECT_TRUE(ring_buffer_vmo.is_valid());
      }));
  ExpectCallbacks();
  if (HasFailure()) {
    return;
  }

  ring_buffer_mapper_.Unmap();
  const zx_vm_option_t option_flags = ZX_VM_PERM_READ | ZX_VM_PERM_WRITE;
  EXPECT_EQ(ring_buffer_mapper_.CreateAndMap(ring_buffer_frames_ * frame_size_, option_flags,
                                             nullptr, &ring_buffer_vmo,
                                             ZX_RIGHT_READ | ZX_RIGHT_MAP | ZX_RIGHT_TRANSFER),
            ZX_OK);
}

void AdminTest::ActivateChannelsAndExpectSuccess(uint64_t active_channels_bitmask) {
  ActivateChannels(active_channels_bitmask, true);
}

void AdminTest::ActivateChannelsAndExpectFailure(uint64_t active_channels_bitmask) {
  ActivateChannels(active_channels_bitmask, false);
}

void AdminTest::ActivateChannels(uint64_t active_channels_bitmask, bool expect_success) {
  zx_status_t status = ZX_OK;
  auto send_time = zx::clock::get_monotonic();
  auto set_time = zx::time(0);
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
  ASSERT_EQ(status, expect_success ? ZX_OK : ZX_ERR_INVALID_ARGS);
  if (expect_success) {
    EXPECT_GT(set_time, send_time);
  }
}

// Request that the driver start the ring buffer engine, responding with the start_time.
// This method assumes that GetVmo has previously been called and we are not already started.
void AdminTest::RequestStart() {
  // Any position notifications that arrive before the Start callback should cause failures.
  FailOnPositionNotifications();

  auto send_time = zx::clock::get_monotonic();
  ring_buffer_->Start(AddCallback("Start", [this](int64_t start_time) {
    AllowPositionNotifications();
    start_time_ = zx::time(start_time);
  }));

  ExpectCallbacks();
  if (!HasFailure()) {
    EXPECT_GT(start_time_, send_time);
  }
}

// Request that the driver start the ring buffer engine, but expect disconnect rather than response.
void AdminTest::RequestStartAndExpectDisconnect(zx_status_t expected_error) {
  ring_buffer_->Start([](int64_t start_time) { FAIL() << "Received unexpected Start response"; });

  ExpectError(ring_buffer(), expected_error);
}

// Request that driver stop the ring buffer. This assumes that GetVmo has previously been called.
void AdminTest::RequestStop() {
  ring_buffer_->Stop(AddCallback("Stop"));

  ExpectCallbacks();
}

// Request that the driver start the ring buffer engine, but expect disconnect rather than response.
// We would expect this if calling Stop before GetVmo, for example.
void AdminTest::RequestStopAndExpectDisconnect(zx_status_t expected_error) {
  ring_buffer_->Stop(AddUnexpectedCallback("Stop - expected disconnect instead"));

  ExpectError(ring_buffer(), expected_error);
}

// After Stop is called, no position notification should be received.
// To validate this without any race windows: from within the next position notification itself,
// we call Stop and flag that subsequent position notifications should FAIL.
void AdminTest::RequestStopAndExpectNoPositionNotifications() {
  ring_buffer_->Stop(AddCallback("Stop", [this]() { FailOnPositionNotifications(); }));

  ExpectCallbacks();
}

void AdminTest::PositionNotificationCallback(
    fuchsia::hardware::audio::RingBufferPositionInfo position_info) {
  // If this is an unexpected callback, fail and exit.
  if (fail_on_position_notification_) {
    FAIL() << "Unexpected position notification";
  }
  ASSERT_GT(notifications_per_ring(), 0u)
      << "Position notification received: notifications_per_ring() cannot be zero";
}

void AdminTest::WatchDelayAndExpectUpdate() {
  ring_buffer_->WatchDelayInfo(
      AddCallback("WatchDelayInfo", [this](fuchsia::hardware::audio::DelayInfo delay_info) {
        delay_info_ = std::move(delay_info);
      }));
  ExpectCallbacks();

  ASSERT_TRUE(delay_info_.has_value()) << "No DelayInfo table received";
}

void AdminTest::WatchDelayAndExpectNoUpdate() {
  ring_buffer_->WatchDelayInfo([](fuchsia::hardware::audio::DelayInfo delay_info) {
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

// As of SDK version 9, `RingBufferProperties.fifo_depth` is deprecated; it is replaced by the new
// `DelayInfo.internal_delay`. If fifo_depth is present, it must be an equivalent duration.
// TODO(fxbug.dev/116898): eliminate this method once fifo_depth is entirely removed.
void AdminTest::ExpectInternalDelayMatchesFifoDepth() {
  ASSERT_TRUE(ring_buffer_props_.has_value())
      << "Internal test error: called before receiving RingBuffer/GetProperties response";
  ASSERT_TRUE(delay_info_.has_value())
      << "Internal test error: called before receiving RingBuffer/WatchDelayInfo response";

  if (ring_buffer_props_->has_fifo_depth()) {
    const auto kOneFrameInNs = (ZX_SEC(1) + pcm_format().frame_rate - 1) / pcm_format().frame_rate;

    // nsec = bytes * nsec/sec * sec/frame * frames/byte
    int64_t fifo_delay_ns =
        ring_buffer_props_->fifo_depth() * ZX_SEC(1) / pcm_format().frame_rate / frame_size_;

    // A driver's byte->duration calculation can vary, depending on how they floor/round/ceiling.
    // Further, FIFO depth may not align perfectly with frame size.
    // Thus, we allow internal delays to exceed a size-based calculation by nearly a frame duration.

    std::ostringstream oss;
    oss << "WatchDelayInfo `internal_delay` (" << delay_info_->internal_delay()
        << ") must equal (or exceed by less than a frame) GetProperties `fifo_depth` ("
        << fifo_delay_ns << ").  fifo_depth " << ring_buffer_props_->fifo_depth() << ", frame_size "
        << frame_size_ << ", frame_rate " << pcm_format().frame_rate
        << " (1 frame = " << kOneFrameInNs << " nsec)";
    EXPECT_GE(delay_info_->internal_delay(), fifo_delay_ns) << oss.str();
    EXPECT_LT(delay_info_->internal_delay(), fifo_delay_ns + kOneFrameInNs) << oss.str();

    return;
  }
  // Otherwise we Pass; "fifo_depth not specified" is valid and expected.
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

// As of SDK version 9, `RingBufferProperties.external_delay` is deprecated; it is replaced by the
// new `DelayInfo.external_delay`. If both are present, they must match. If only the new is present,
// then we Pass. If the driver chose to specify neither, we Skip (we have no values to compare).
// TODO(fxbug.dev/116898): remove this method once RingBufferProperties.external_delay is gone.
void AdminTest::ExpectExternalDelayMatchesRingBufferProperties() {
  ASSERT_TRUE(ring_buffer_props_.has_value())
      << "Internal test error: should have called RequestRingBufferProperties by now";
  ASSERT_TRUE(delay_info_.has_value())
      << "Internal test error: should have called WatchDelayAndExpectUpdate by now";

  if (ring_buffer_props_->has_external_delay()) {
    EXPECT_EQ(ring_buffer_props_->external_delay(),
              delay_info_->has_external_delay() ? delay_info_->external_delay() : 0ll)
        << "WatchDelayInfo `external_delay` must match GetProperties `external_delay`";
    return;
  }

  if (!delay_info_->has_external_delay()) {
    // If neither RingBufferProps nor DelayInfo specified external_delay, then we just Skip.
    // Although theoretically valid, we cannot actually test anything (no values to compare).
    GTEST_SKIP() << "Neither DelayInfo nor RingBufferProperties specified external_delay; "
                 << "they cannot be compared";
    __UNREACHABLE;
  }
  // Otherwise we Pass; "RingBufferProps.external_delay not specified" is valid and expected.
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

// Verify valid responses: ring buffer properties
DEFINE_ADMIN_TEST_CLASS(GetRingBufferProperties, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());

  RequestRingBufferProperties();
  WaitForError();
});

// Verify valid responses: get ring buffer VMO.
DEFINE_ADMIN_TEST_CLASS(GetBuffer, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());

  RequestBuffer(100);
  WaitForError();
});

// Verify valid responses: set active channels
DEFINE_ADMIN_TEST_CLASS(SetActiveChannels, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(ActivateChannelsAndExpectSuccess(0));

  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(8000));
  ASSERT_NO_FAILURE_OR_SKIP(RequestStart());

  uint64_t all_channels = (1 << pcm_format().number_of_channels) - 1;
  ActivateChannelsAndExpectSuccess(all_channels);
  WaitForError();
});

// Verify an invalid input (out of range) for SetActiveChannels.
DEFINE_ADMIN_TEST_CLASS(SetActiveChannelsTooHigh, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());

  auto channel_mask_too_high = (1 << pcm_format().number_of_channels);
  ActivateChannelsAndExpectFailure(channel_mask_too_high);
});

// Verify that valid start responses are received.
DEFINE_ADMIN_TEST_CLASS(Start, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(32000));

  RequestStart();
  WaitForError();
});

// ring-buffer FIDL channel should disconnect, with ZX_ERR_BAD_STATE
DEFINE_ADMIN_TEST_CLASS(StartBeforeGetVmoShouldDisconnect, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());

  RequestStartAndExpectDisconnect(ZX_ERR_BAD_STATE);
});

// ring-buffer FIDL channel should disconnect, with ZX_ERR_BAD_STATE
DEFINE_ADMIN_TEST_CLASS(StartWhileStartedShouldDisconnect, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(8000));
  ASSERT_NO_FAILURE_OR_SKIP(RequestStart());

  RequestStartAndExpectDisconnect(ZX_ERR_BAD_STATE);
});

// Verify that valid stop responses are received.
DEFINE_ADMIN_TEST_CLASS(Stop, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(RequestStart());

  RequestStop();
  WaitForError();
});

// ring-buffer FIDL channel should disconnect, with ZX_ERR_BAD_STATE
DEFINE_ADMIN_TEST_CLASS(StopBeforeGetVmoShouldDisconnect, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());

  RequestStopAndExpectDisconnect(ZX_ERR_BAD_STATE);
});

DEFINE_ADMIN_TEST_CLASS(StopWhileStoppedIsPermitted, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMinFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(RequestStop());

  RequestStop();
  WaitForError();
});

// Verify valid WatchDelayInfo internal_delay responses.
DEFINE_ADMIN_TEST_CLASS(InternalDelayIsValid, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());

  WatchDelayAndExpectUpdate();
  ValidateInternalDelay();
  WaitForError();
});

// Verify valid WatchDelayInfo external_delay response.
DEFINE_ADMIN_TEST_CLASS(ExternalDelayIsValid, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());

  WatchDelayAndExpectUpdate();
  ValidateExternalDelay();
  WaitForError();
});

// Verify valid responses: WatchDelayInfo does NOT respond a second time.
DEFINE_ADMIN_TEST_CLASS(GetDelayInfoSecondTimeNoResponse, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());

  WatchDelayAndExpectUpdate();
  WatchDelayAndExpectNoUpdate();

  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(8000));
  ASSERT_NO_FAILURE_OR_SKIP(RequestStart());
  ASSERT_NO_FAILURE_OR_SKIP(RequestStop());

  WaitForError();
});

// Verify that valid WatchDelayInfo responses are received, even after Start().
DEFINE_ADMIN_TEST_CLASS(GetDelayInfoAfterStart, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(RequestStart());

  WatchDelayAndExpectUpdate();
  WaitForError();
});

// If a driver states RingBufferProperties.fifo_depth, it must match DelayInfo.internal_delay.
// TODO(fxbug.dev/116898): eliminate this case once fifo_depth is entirely removed.
DEFINE_ADMIN_TEST_CLASS(GetDelayInfoInternalDelayMatchesFifoDepth, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());

  WatchDelayAndExpectUpdate();
  ExpectInternalDelayMatchesFifoDepth();
});

// If a driver states RingBufferProperties.external_delay, then DelayInfo.external_delay must exist
// and match. If only DelayInfo (not RingBufferProperties) specifies it, the case passes.
// If neither includes it, the test case is skipped; this is how we treat other "cannot test this
// optional aspect since driver does not support it" cases such as SetActiveChannels or SetGain.
// TODO(fxbug.dev/116898): eliminate this case once RingBufferProperties.external_delay is removed.
DEFINE_ADMIN_TEST_CLASS(GetDelayInfoExternalDelayMatchesRingBufferProps, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());

  WatchDelayAndExpectUpdate();
  ExpectExternalDelayMatchesRingBufferProperties();
});

// Create RingBuffer, fully exercise it, drop it, recreate it, then validate GetDelayInfo.
DEFINE_ADMIN_TEST_CLASS(GetDelayInfoAfterDroppingFirstRingBuffer, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(WatchDelayAndExpectUpdate());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(WatchDelayAndExpectNoUpdate());
  ASSERT_NO_FAILURE_OR_SKIP(RequestStart());
  ASSERT_NO_FAILURE_OR_SKIP(RequestStop());
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
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(RequestStart());
  ASSERT_NO_FAILURE_OR_SKIP(
      ActivateChannelsAndExpectSuccess((1 << pcm_format().number_of_channels) - 1));
  ASSERT_NO_FAILURE_OR_SKIP(RequestStop());
  ASSERT_NO_FAILURE_OR_SKIP(DropRingBuffer());

  // Dropped first ring buffer, creating second one, reverifying SetActiveChannels.
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferChannelWithMaxFormat());
  ASSERT_NO_FAILURE_OR_SKIP(RequestRingBufferProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestBuffer(100));
  ASSERT_NO_FAILURE_OR_SKIP(RequestStart());
  ASSERT_NO_FAILURE_OR_SKIP(
      ActivateChannelsAndExpectSuccess((1 << pcm_format().number_of_channels) - 1));

  RequestStop();
  WaitForError();
});

// Register separate test case instances for each enumerated device
//
// See googletest/docs/advanced.md for details
#define REGISTER_ADMIN_TEST(CLASS_NAME, DEVICE)                                              \
  testing::RegisterTest("AdminTest", TestNameForEntry(#CLASS_NAME, DEVICE).c_str(), nullptr, \
                        DevNameForEntry(DEVICE).c_str(), __FILE__, __LINE__,                 \
                        [=]() -> AdminTest* { return new CLASS_NAME(DEVICE); })

#define REGISTER_DISABLED_ADMIN_TEST(CLASS_NAME, DEVICE)                                       \
  testing::RegisterTest(                                                                       \
      "AdminTest", (std::string("DISABLED_") + TestNameForEntry(#CLASS_NAME, DEVICE)).c_str(), \
      nullptr, DevNameForEntry(DEVICE).c_str(), __FILE__, __LINE__,                            \
      [=]() -> AdminTest* { return new CLASS_NAME(DEVICE); })

void RegisterAdminTestsForDevice(const DeviceEntry& device_entry,
                                 bool expect_audio_core_connected) {
  // If audio_core is connected to the audio driver, admin tests will fail.
  // We test a hermetic instance of the A2DP driver, so audio_core is never connected.
  if (device_entry.dir_fd == DeviceEntry::kA2dp || !expect_audio_core_connected) {
    REGISTER_ADMIN_TEST(GetRingBufferProperties, device_entry);
    REGISTER_ADMIN_TEST(GetBuffer, device_entry);
    REGISTER_ADMIN_TEST(InternalDelayIsValid, device_entry);
    REGISTER_ADMIN_TEST(ExternalDelayIsValid, device_entry);

    REGISTER_ADMIN_TEST(SetActiveChannels, device_entry);
    REGISTER_ADMIN_TEST(SetActiveChannelsTooHigh, device_entry);
    REGISTER_ADMIN_TEST(GetDelayInfoSecondTimeNoResponse, device_entry);

    REGISTER_ADMIN_TEST(Start, device_entry);
    REGISTER_ADMIN_TEST(StartBeforeGetVmoShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(StartWhileStartedShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(GetDelayInfoAfterStart, device_entry);

    REGISTER_ADMIN_TEST(Stop, device_entry);
    REGISTER_ADMIN_TEST(StopBeforeGetVmoShouldDisconnect, device_entry);
    REGISTER_ADMIN_TEST(StopWhileStoppedIsPermitted, device_entry);

    REGISTER_ADMIN_TEST(GetDelayInfoInternalDelayMatchesFifoDepth, device_entry);
    REGISTER_ADMIN_TEST(GetDelayInfoExternalDelayMatchesRingBufferProps, device_entry);

    REGISTER_ADMIN_TEST(GetDelayInfoAfterDroppingFirstRingBuffer, device_entry);
    REGISTER_ADMIN_TEST(SetActiveChannelsAfterDroppingFirstRingBuffer, device_entry);
  }
}

}  // namespace media::audio::drivers::test
