// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/media/cpp/fidl.h>

#include <utility>

#include "src/media/audio/audio_core/shared/stream_usage.h"
#include "src/media/audio/audio_core/testing/integration/hermetic_audio_test.h"

using fuchsia::media::AudioCaptureUsage2;
using fuchsia::media::AudioRenderUsage2;
using fuchsia::media::AudioSampleFormat;

namespace media::audio::test {

namespace {
class FakeUsageWatcher : public fuchsia::media::UsageWatcher {
 public:
  explicit FakeUsageWatcher(TestFixture* fixture) : binding_(this) {
    fixture->AddErrorHandler(binding_, "FakeUsageWatcher");
  }

  fidl::InterfaceHandle<fuchsia::media::UsageWatcher> NewBinding() { return binding_.NewBinding(); }

  using Handler =
      std::function<void(fuchsia::media::Usage2 usage, fuchsia::media::UsageState usage_state)>;

  void SetNextHandler(Handler h) { next_handler_ = std::move(h); }

 private:
  void OnStateChanged(fuchsia::media::Usage _usage, fuchsia::media::UsageState usage_state,
                      OnStateChangedCallback callback) override {
    auto usage = ToFidlUsage2(_usage);
    if (next_handler_) {
      next_handler_(std::move(usage), std::move(usage_state));
      next_handler_ = nullptr;
    }
    callback();
  }

  fidl::Binding<fuchsia::media::UsageWatcher> binding_;
  Handler next_handler_;
};

class FakeUsageWatcher2 : public fuchsia::media::UsageWatcher2 {
 public:
  explicit FakeUsageWatcher2(TestFixture* fixture) : binding_(this) {
    fixture->AddErrorHandler(binding_, "FakeUsageWatcher2");
  }

  fidl::InterfaceHandle<fuchsia::media::UsageWatcher2> NewBinding() {
    return binding_.NewBinding();
  }

  using Handler =
      std::function<void(fuchsia::media::Usage2 usage, fuchsia::media::UsageState usage_state)>;

  void SetNextHandler(Handler h) { next_handler_ = std::move(h); }

 private:
  void OnStateChanged2(fuchsia::media::Usage2 usage, fuchsia::media::UsageState usage_state,
                       OnStateChanged2Callback callback) override {
    if (next_handler_) {
      next_handler_(std::move(usage), std::move(usage_state));
      next_handler_ = nullptr;
    }
    callback();
  }

  fidl::Binding<fuchsia::media::UsageWatcher2> binding_;
  Handler next_handler_;
};
}  // namespace

class UsageReporterTest : public HermeticAudioTest {
 protected:
  void SetUp() {
    HermeticAudioTest::SetUp();
    audio_core_->ResetInteractions();
  }

  struct Controller {
    explicit Controller(TestFixture* fixture) : fake_watcher(fixture) {}

    fuchsia::media::UsageReporterPtr usage_reporter;
    FakeUsageWatcher fake_watcher;
  };
  struct Controller2 {
    explicit Controller2(TestFixture* fixture) : fake_watcher(fixture) {}

    fuchsia::media::UsageReporterPtr usage_reporter;
    FakeUsageWatcher2 fake_watcher;
  };

  template <typename T, typename U>
  std::unique_ptr<T> CreateController(U u) {
    auto c = std::make_unique<T>(this);
    realm().Connect(c->usage_reporter.NewRequest());
    AddErrorHandler(c->usage_reporter, "UsageReporter");

    if constexpr (std::is_same_v<T, Controller>) {
      fuchsia::media::Usage usage;
      if constexpr (std::is_same_v<U, fuchsia::media::AudioRenderUsage2>) {
        usage = fuchsia::media::Usage::WithRenderUsage(*ToFidlRenderUsageTry(u));
      } else if constexpr (std::is_same_v<U, fuchsia::media::AudioCaptureUsage2>) {
        usage = fuchsia::media::Usage::WithCaptureUsage(*ToFidlCaptureUsageTry(u));
      }
      c->usage_reporter->Watch(std::move(usage), c->fake_watcher.NewBinding());
    } else if constexpr (std::is_same_v<T, Controller2>) {
      fuchsia::media::Usage2 usage;
      if constexpr (std::is_same_v<U, fuchsia::media::AudioRenderUsage2>) {
        usage = fuchsia::media::Usage2::WithRenderUsage(fidl::Clone(u));
      } else if constexpr (std::is_same_v<U, fuchsia::media::AudioCaptureUsage2>) {
        usage = fuchsia::media::Usage2::WithCaptureUsage(fidl::Clone(u));
      }
      c->usage_reporter->Watch2(std::move(usage), c->fake_watcher.NewBinding());
    } else {
      FAIL() << "Template parameter must be Controller or Controller2";
    }
    return c;
  }

  void StartRendererWithUsage(AudioRenderUsage2 usage) {
    auto format = Format::Create<AudioSampleFormat::SIGNED_16>(1, 8000).value();  // arbitrary
    auto r = CreateAudioRenderer(format, 1024, usage);
    r->fidl()->PlayNoReply(0, 0);
  }

  void StartCapturerWithUsage(AudioCaptureUsage2 _usage) {
    auto format = Format::Create<AudioSampleFormat::SIGNED_16>(1, 8000).value();  // arbitrary
    fuchsia::media::InputAudioCapturerConfiguration cfg;
    auto usage = ToFidlCaptureUsageTry(_usage);
    cfg.set_usage(*usage);
    auto c = CreateAudioCapturer(
        format, 1024, fuchsia::media::AudioCapturerConfiguration::WithInput(std::move(cfg)));
    c->fidl()->StartAsyncCapture(1024);
  }

  template <typename T>
  void TestRenderInitialState() {
    fuchsia::media::Usage2 last_usage;
    fuchsia::media::UsageState last_state;

    auto c = CreateController<T>(AudioRenderUsage2::MEDIA);
    c->fake_watcher.SetNextHandler(AddCallback(
        "OnStateChanged",
        [&last_usage, &last_state](fuchsia::media::Usage2 usage, fuchsia::media::UsageState state) {
          last_usage = std::move(usage);
          last_state = std::move(state);
        }));

    // The initial callback happens immediately.
    ExpectCallbacks();
    EXPECT_TRUE(last_state.is_unadjusted());
    EXPECT_TRUE(last_usage.is_render_usage());
    EXPECT_EQ(last_usage.render_usage(), AudioRenderUsage2::MEDIA);
  }

  template <typename T>
  void TestRenderDucked() {
    fuchsia::media::Usage2 last_usage;
    fuchsia::media::UsageState last_state;

    // The initial callback happens immediately.
    auto c = CreateController<T>(AudioRenderUsage2::MEDIA);
    c->fake_watcher.SetNextHandler(AddCallback("OnStateChanged InitialCall"));
    ExpectCallbacks();

    c->fake_watcher.SetNextHandler(AddCallback(
        "OnStateChanged",
        [&last_usage, &last_state](fuchsia::media::Usage2 usage, fuchsia::media::UsageState state) {
          last_usage = std::move(usage);
          last_state = std::move(state);
        }));

    // Duck MEDIA when SYSTEM_AGENT is active.
    audio_core_->SetInteraction2(ToFidlUsage2(RenderUsage::SYSTEM_AGENT),
                                 ToFidlUsage2(RenderUsage::MEDIA), fuchsia::media::Behavior::DUCK);

    StartRendererWithUsage(AudioRenderUsage2::SYSTEM_AGENT);
    ExpectCallbacks();
    EXPECT_TRUE(last_state.is_ducked());
    EXPECT_TRUE(last_usage.is_render_usage());
    EXPECT_EQ(last_usage.render_usage(), AudioRenderUsage2::MEDIA);
  }

  template <typename T>
  void TestRenderMuted() {
    fuchsia::media::Usage2 last_usage;
    fuchsia::media::UsageState last_state;

    // The initial callback happens immediately.
    auto c = CreateController<T>(AudioRenderUsage2::MEDIA);
    c->fake_watcher.SetNextHandler(AddCallback("OnStateChanged InitialCall"));
    ExpectCallbacks();

    c->fake_watcher.SetNextHandler(AddCallback(
        "OnStateChange",
        [&last_usage, &last_state](fuchsia::media::Usage2 usage, fuchsia::media::UsageState state) {
          last_usage = std::move(usage);
          last_state = std::move(state);
        }));

    // Mute MEDIA when SYSTEM_AGENT is active.
    audio_core_->SetInteraction2(ToFidlUsage2(RenderUsage::SYSTEM_AGENT),
                                 ToFidlUsage2(RenderUsage::MEDIA), fuchsia::media::Behavior::MUTE);

    StartRendererWithUsage(AudioRenderUsage2::SYSTEM_AGENT);
    ExpectCallbacks();
    EXPECT_TRUE(last_state.is_muted());
    EXPECT_TRUE(last_usage.is_render_usage());
    EXPECT_EQ(last_usage.render_usage(), AudioRenderUsage2::MEDIA);
  }

  template <typename T>
  void TestCaptureInitialState() {
    fuchsia::media::Usage2 last_usage;
    fuchsia::media::UsageState last_state;

    auto c = CreateController<T>(AudioCaptureUsage2::COMMUNICATION);
    c->fake_watcher.SetNextHandler(AddCallback(
        "OnStateChanged",
        [&last_usage, &last_state](fuchsia::media::Usage2 usage, fuchsia::media::UsageState state) {
          last_usage = std::move(usage);
          last_state = std::move(state);
        }));

    // The initial callback happens immediately.
    ExpectCallbacks();
    EXPECT_TRUE(last_state.is_unadjusted());
    EXPECT_TRUE(last_usage.is_capture_usage());
    EXPECT_EQ(last_usage.capture_usage(), AudioCaptureUsage2::COMMUNICATION);
  }

  template <typename T>
  void TestCaptureMuted() {
    fuchsia::media::Usage2 last_usage;
    fuchsia::media::UsageState last_state;

    // The initial callback happens immediately.
    auto c = CreateController<T>(AudioCaptureUsage2::COMMUNICATION);
    c->fake_watcher.SetNextHandler(AddCallback("OnStateChanged InitialCall"));
    ExpectCallbacks();
    c->fake_watcher.SetNextHandler(AddCallback(
        "OnStateChanged",
        [&last_usage, &last_state](fuchsia::media::Usage2 usage, fuchsia::media::UsageState state) {
          last_usage = std::move(usage);
          last_state = std::move(state);
        }));

    // Duck COMMUNICATION when SYSTEM_AGENT is active.
    audio_core_->SetInteraction2(ToFidlUsage2(CaptureUsage::SYSTEM_AGENT),
                                 ToFidlUsage2(CaptureUsage::COMMUNICATION),
                                 fuchsia::media::Behavior::DUCK);

    StartCapturerWithUsage(AudioCaptureUsage2::SYSTEM_AGENT);
    ExpectCallbacks();
    EXPECT_TRUE(last_state.is_ducked());
    EXPECT_TRUE(last_usage.is_capture_usage());
    EXPECT_EQ(last_usage.capture_usage(), AudioCaptureUsage2::COMMUNICATION);
  }

  template <typename T>
  void TestCaptureDucked() {
    fuchsia::media::Usage2 last_usage;
    fuchsia::media::UsageState last_state;

    // The initial callback happens immediately.
    auto c = CreateController<T>(AudioCaptureUsage2::COMMUNICATION);
    c->fake_watcher.SetNextHandler(AddCallback("OnStateChanged InitialCall"));
    ExpectCallbacks();
    c->fake_watcher.SetNextHandler(AddCallback(
        "OnStateChanged",
        [&last_usage, &last_state](fuchsia::media::Usage2 usage, fuchsia::media::UsageState state) {
          last_usage = std::move(usage);
          last_state = std::move(state);
        }));

    // Mute COMMUNICATION when SYSTEM_AGENT is active.
    audio_core_->SetInteraction2(ToFidlUsage2(CaptureUsage::SYSTEM_AGENT),
                                 ToFidlUsage2(CaptureUsage::COMMUNICATION),
                                 fuchsia::media::Behavior::MUTE);

    StartCapturerWithUsage(AudioCaptureUsage2::SYSTEM_AGENT);
    ExpectCallbacks();
    EXPECT_TRUE(last_state.is_muted());
    EXPECT_TRUE(last_usage.is_capture_usage());
    EXPECT_EQ(last_usage.capture_usage(), AudioCaptureUsage2::COMMUNICATION);
  }
};

TEST_F(UsageReporterTest, RenderUsageInitialState) { TestRenderInitialState<Controller>(); }
TEST_F(UsageReporterTest, RenderUsage2InitialState) { TestRenderInitialState<Controller2>(); }

TEST_F(UsageReporterTest, RenderUsageDucked) { TestRenderDucked<Controller>(); }
TEST_F(UsageReporterTest, RenderUsage2Ducked) { TestRenderDucked<Controller2>(); }

TEST_F(UsageReporterTest, RenderUsageMuted) { TestRenderMuted<Controller>(); }
TEST_F(UsageReporterTest, RenderUsage2Muted) { TestRenderMuted<Controller2>(); }

TEST_F(UsageReporterTest, CaptureUsageInitialState) { TestCaptureInitialState<Controller>(); }
TEST_F(UsageReporterTest, CaptureUsage2InitialState) { TestCaptureInitialState<Controller2>(); }

TEST_F(UsageReporterTest, CaptureUsageDucked) { TestCaptureDucked<Controller>(); }
TEST_F(UsageReporterTest, CaptureUsage2Ducked) { TestCaptureDucked<Controller2>(); }

TEST_F(UsageReporterTest, CaptureUsageMuted) { TestCaptureMuted<Controller>(); }
TEST_F(UsageReporterTest, CaptureUsage2Muted) { TestCaptureMuted<Controller2>(); }

}  // namespace media::audio::test
