// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/media/cpp/fidl.h>

#include <cmath>

#include <gmock/gmock.h>

#include "src/media/audio/audio_core/shared/stream_usage.h"
#include "src/media/audio/audio_core/testing/integration/hermetic_audio_test.h"

using fuchsia::media::AudioRenderUsage;
using fuchsia::media::AudioRenderUsage2;
using fuchsia::media::AudioSampleFormat;
using testing::UnorderedElementsAre;
using testing::UnorderedElementsAreArray;

namespace media::audio::test {

class ActivityReporterTest : public HermeticAudioTest {
 protected:
  void SetUp() override {
    HermeticAudioTest::SetUp();

    realm().Connect(activity_reporter_.NewRequest());
    AddErrorHandler(activity_reporter_, "ActivityReporter");
  }

  AudioRendererShim<AudioSampleFormat::SIGNED_16>* CreateAndPlayWithUsage(AudioRenderUsage2 usage) {
    auto format = Format::Create<AudioSampleFormat::SIGNED_16>(1, 8000).value();  // arbitrary
    auto r = CreateAudioRenderer(format, 1024, usage);
    r->fidl()->PlayNoReply(0, 0);
    return r;
  }

  fuchsia::media::ActivityReporterPtr activity_reporter_;
};

// Test that the user is connected to the activity reporter.
TEST_F(ActivityReporterTest, AddAndRemove) {
  std::vector<AudioRenderUsage> active_usages;
  auto add_callback = [this, &active_usages](std::string name) {
    active_usages.clear();
    activity_reporter_->WatchRenderActivity(AddCallback(
        name, [&active_usages](std::vector<AudioRenderUsage> u) { active_usages = u; }));
  };

  // First call should return immediately, others should wait for updates.
  add_callback("WatchRenderActivity InitialCall");
  ExpectCallbacks();
  EXPECT_TRUE(active_usages.empty());

  add_callback("WatchRenderActivity AfterPlayBackground");
  auto r1 = CreateAndPlayWithUsage(AudioRenderUsage2::BACKGROUND);
  ExpectCallbacks();
  EXPECT_THAT(active_usages,
              UnorderedElementsAreArray({*FromFidlRenderUsage2(AudioRenderUsage2::BACKGROUND)}));

  add_callback("WatchRenderActivity AfterPlayMedia");
  auto r2 = CreateAndPlayWithUsage(AudioRenderUsage2::MEDIA);
  ExpectCallbacks();
  EXPECT_THAT(active_usages,
              UnorderedElementsAreArray({*FromFidlRenderUsage2(AudioRenderUsage2::BACKGROUND),
                                         *FromFidlRenderUsage2(AudioRenderUsage2::MEDIA)}));

  add_callback("WatchRenderActivity AfterPauseBackground");
  r1->fidl()->PauseNoReply();
  ExpectCallbacks();
  EXPECT_THAT(active_usages,
              UnorderedElementsAreArray({*FromFidlRenderUsage2(AudioRenderUsage2::MEDIA)}));

  add_callback("WatchRenderActivity AfterDisconnectMedia");
  Unbind(r2);
  ExpectCallbacks();
  EXPECT_TRUE(active_usages.empty());
  Unbind(r1);
}

TEST_F(ActivityReporterTest, DisconnectOnMultipleConcurrentCalls) {
  activity_reporter_->WatchRenderActivity(AddCallback("WatchRenderActivity"));
  ExpectCallbacks();

  activity_reporter_->WatchRenderActivity(AddUnexpectedCallback("WatchRenderActivity Unexpected1"));
  activity_reporter_->WatchRenderActivity(AddUnexpectedCallback("WatchRenderActivity Unexpected2"));
  ExpectDisconnect(activity_reporter_);
}

}  // namespace media::audio::test
