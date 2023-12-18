// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_TESTS_BASIC_TEST_H_
#define SRC_MEDIA_AUDIO_DRIVERS_TESTS_BASIC_TEST_H_

#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <zircon/device/audio.h>

#include <optional>

#include "src/media/audio/drivers/tests/test_base.h"

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

  void RequestHealthAndExpectHealthy();
  void GetHealthState(fuchsia::hardware::audio::Health::GetHealthStateCallback cb);

  void RetrieveProperties();
  void ValidateProperties();

  void WatchGainStateAndExpectUpdate();
  void WatchGainStateAndExpectNoUpdate();
  void RequestSetGain();

  void WatchPlugStateAndExpectUpdate();
  void WatchPlugStateAndExpectNoUpdate();

 private:
  void ValidatePlugState(const fuchsia::hardware::audio::PlugState& plug_state);

  void DisplayBaseProperties();
  // The union of [CodecProperties, CompositeProperties, DaiProperties, StreamProperties].
  struct BaseProperties {
    //       On codec/composite/dai/stream, member is   (o)ptional (r)equired (.)absent
    std::optional<bool> is_input;                                   // o.rr
    std::optional<std::array<uint8_t, kUniqueIdLength>> unique_id;  // oooo
    std::optional<std::string> manufacturer;                        // oooo
    std::optional<std::string> product;                             // oooo
    std::optional<uint32_t> clock_domain;                           // .rrr

    std::optional<fuchsia::hardware::audio::PlugDetectCapabilities>
        plug_detect_capabilities;       // r..r
    std::optional<bool> can_mute;       // ...o
    std::optional<bool> can_agc;        // ...o
    std::optional<float> min_gain_db;   // ...r
    std::optional<float> max_gain_db;   // ...r
    std::optional<float> gain_step_db;  // ...r
  };
  std::optional<BaseProperties> properties_;

  // BasicTest cannot permanently change device state. Optionals ensure we fetch initial gain
  // state (to later restore it), before calling any method that alters device gain.
  std::optional<fuchsia::hardware::audio::GainState> initial_gain_state_;
  bool set_gain_state_ = false;
};

}  // namespace media::audio::drivers::test

#endif  // SRC_MEDIA_AUDIO_DRIVERS_TESTS_BASIC_TEST_H_
