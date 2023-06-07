// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_TEST_BASIC_TEST_H_
#define SRC_MEDIA_AUDIO_DRIVERS_TEST_BASIC_TEST_H_

#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <zircon/device/audio.h>

#include <optional>

#include "src/lib/fsl/io/device_watcher.h"
#include "src/media/audio/drivers/test/test_base.h"

namespace media::audio::drivers::test {

// BasicTest cases must run in environments where an audio driver may already have an active client.
// For most hardware functions, testing is limited to validating the correctness and consistency of
// the declared capabilities and current state. BasicTest cases CAN _change_ a device's state, but
// only if it fully restores the previous state afterward (as it does when testing SetGain).
//
// A driver can have only one RingBuffer client connection at any time, so BasicTest avoids any
// usage of the RingBuffer interface. (Note: AdminTest is not limited to RingBuffer cases.)
class BasicTest : public TestBase {
 public:
  explicit BasicTest(const DeviceEntry& dev_entry) : TestBase(dev_entry) {}

 protected:
  void TearDown() override;

  void RequestHealthState();
  void GetHealthState(fuchsia::hardware::audio::Health::GetHealthStateCallback cb);

  void RequestStreamProperties();

  void WatchGainStateAndExpectUpdate();
  void WatchGainStateAndExpectNoUpdate();

  void RequestSetGain();

  void WatchPlugStateAndExpectUpdate();
  void WatchPlugStateAndExpectNoUpdate();

  void ValidateFormatCorrectness();
  void ValidateFormatOrdering();

 private:
  static constexpr size_t kUniqueIdLength = 16;

  // BasicTest cannot permanently change device state. Optionals ensure we fetch stream props and
  // initial gain state (to later restore it), before calling any method that alters device state.
  std::optional<fuchsia::hardware::audio::StreamProperties> stream_props_;
  std::optional<fuchsia::hardware::audio::GainState> previous_gain_state_;

  bool changed_gain_state_ = false;
  fuchsia::hardware::audio::PlugState plug_state_;
};

}  // namespace media::audio::drivers::test

#endif  // SRC_MEDIA_AUDIO_DRIVERS_TEST_BASIC_TEST_H_
