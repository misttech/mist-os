// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_TEST_API_AUDIO_RENDERER_TEST_SHARED_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_TEST_API_AUDIO_RENDERER_TEST_SHARED_H_

#include <fuchsia/media/cpp/fidl.h>
#include <fuchsia/media/tuning/cpp/fidl.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>
#include <zircon/syscalls.h>

#include <utility>

#include <gtest/gtest.h>

#include "src/media/audio/audio_core/shared/device_id.h"
#include "src/media/audio/audio_core/testing/integration/hermetic_audio_test.h"
#include "src/media/audio/lib/analysis/generators.h"
#include "src/media/audio/lib/test/comparators.h"

namespace media::audio::test {

// AudioRenderer contains an internal state machine; setting both the buffer and the audio format
// play a central role.
// - Upon construction, a renderer is in the "Initialized" state.
// - To enter "Configured" state, it must receive and successfully execute both SetPcmStreamType and
// AddPayloadBuffer (if only one or the other is called, we remain Initialized).
// - Once Configured, it transitions to "Operating" state, when packets are enqueued (received from
// SendPacket, but not yet played and/or released).
// - Once no enqueued packets remain, it transitions back to Configured state. Packets may be
// cancelled (by DiscardAllPackets), or completed (successfully played); either way their completion
// (if provided) is invoked.

// Additional restrictions on the allowed sequence of API calls:
// SetReferenceClock may only be called once for a given AudioRenderer.
// SetUsage and SetReferenceClock may only be called before SetPcmStreamType.
// SetPcmStreamType, AddPayloadBuffer/RemovePayloadBuffer may only be called when not Operating.
// A renderer must be Configured/Operating before calling SendPacket, Play, Pause.

// Note: the distinction between Configured/Operating is entirely orthogonal to Play/Pause state,
// although Play does cause the timeline to progress, leading to packet completion.

//
// AudioRendererTest
//
// This base class is reused by child classes that provide grouping of specific test areas.
//
// As currently implemented, AudioRenderer's four "NoReply" methods (PlayNoReply, PauseNoReply,
// SendPacketNoReply, DiscardAllPacketsNoReply) each simply redirect to their counterpart with a
// 'nullptr' callback parameter. For this reason, we don't exhaustively test the NoReply variants,
// instead covering them with 1-2 representative test cases each (in addition to those places where
// they are used instead of the "reply" variants for test simplicity).
class AudioRendererTest : public HermeticAudioTest {
 protected:
  // A valid but arbitrary |AudioStreamType|, for tests that don't care about the audio content.
  static constexpr fuchsia::media::AudioStreamType kTestStreamType{
      .sample_format = fuchsia::media::AudioSampleFormat::FLOAT,
      .channels = 2,
      .frames_per_second = 48000,
  };

  // The following are valid/invalid when used with |kTestStreamType|.
  // In bytes: payload buffer 40960 (~ 106 ms); default packet 3840 (10 ms).
  static inline size_t DefaultPayloadBufferSize() { return zx_system_get_page_size() * 10ul; }
  static constexpr uint64_t kDefaultPacketSize =
      sizeof(float) * kTestStreamType.channels * kTestStreamType.frames_per_second / 100;

  // Convenience packet of 10 ms, starting at the beginning of payload buffer 0.
  static constexpr fuchsia::media::StreamPacket kTestPacket{
      .payload_buffer_id = 0,
      .payload_offset = 0,
      .payload_size = kDefaultPacketSize,
  };

  void SetUp() override {
    HermeticAudioTest::SetUp();

    audio_core_->CreateAudioRenderer(audio_renderer_.NewRequest());
    AddErrorHandler(audio_renderer_, "AudioRenderer");
  }

  void TearDown() override {
    audio_renderer_.Unbind();

    HermeticAudioTest::TearDown();
  }

  // This can be used as a simple round-trip to indicate that all FIDL messages have been read out
  // of the channel, and thus have been handled successfully (i.e. no disconnect was triggered).
  void ExpectConnected() {
    audio_renderer_->GetMinLeadTime(AddCallback("GetMinLeadTime"));

    ExpectCallbacks();
  }

  // Discard in-flight packets and await a renderer response. This checks that the completions for
  // all enqueued packets are received, and that the Discard completion is received only afterward.
  // Thus, this also verifies more generally that the renderer is still connected.
  void ExpectConnectedAndDiscardAllPackets() {
    audio_renderer_->DiscardAllPackets(AddCallback("DiscardAllPackets"));

    ExpectCallbacks();
  }

  // Creates a VMO and passes it to |AudioRenderer::AddPayloadBuffer| with a given |id|. This is
  // purely a convenience method and doesn't provide access to the buffer VMO.
  void CreateAndAddPayloadBuffer(uint32_t id) {
    zx::vmo payload_buffer;
    constexpr uint32_t kVmoOptionsNone = 0;
    ASSERT_EQ(zx::vmo::create(DefaultPayloadBufferSize(), kVmoOptionsNone, &payload_buffer), ZX_OK);
    audio_renderer_->AddPayloadBuffer(id, std::move(payload_buffer));
  }

  fuchsia::media::AudioRendererPtr& audio_renderer() { return audio_renderer_; }

 private:
  fuchsia::media::AudioRendererPtr audio_renderer_;
};

// AudioRenderer implements the base classes StreamBufferSet and StreamSink.

// Thin wrapper around AudioRendererTest for test case grouping only. This group validates
// AudioRenderer's implementation of StreamBufferSet (AddPayloadBuffer, RemovePayloadBuffer)
class AudioRendererBufferTest : public AudioRendererTest {};

//
// StreamSink validation
//

// Thin wrapper around AudioRendererTest for test case grouping only. This group validates
// AudioRenderer's implementation of StreamSink (SendPacket, DiscardAllPackets, EndOfStream).
class AudioRendererPacketTest : public AudioRendererTest {
 protected:
  // SetPcmStreamType and AddPayloadBuffer are callable in either order, as long as both are called
  // before Play. Thus, in these tests you see a mixture.
  void SendPacketCancellation(bool reply) {
    CreateAndAddPayloadBuffer(0);
    audio_renderer()->SetPcmStreamType(kTestStreamType);

    // Send a packet (we don't care about the actual packet data here).
    if (reply) {
      audio_renderer()->SendPacket(kTestPacket, AddCallback("SendPacket"));
    } else {
      audio_renderer()->SendPacketNoReply(kTestPacket);
    }

    ExpectConnectedAndDiscardAllPackets();
  }
};

// Thin wrapper around AudioRendererTest for test case grouping only. This group tests
// AudioRenderer's implementation of SetReferenceClock and GetReferenceClock.
class AudioRendererClockTest : public AudioRendererTest {
 protected:
  // The clock received from GetRefClock is read-only, but the original can still be adjusted.
  static constexpr auto kClockRights = ZX_RIGHT_DUPLICATE | ZX_RIGHT_TRANSFER | ZX_RIGHT_READ;

  zx::clock GetAndValidateReferenceClock() {
    zx::clock clock;

    audio_renderer()->GetReferenceClock(
        AddCallback("GetReferenceClock",
                    [&clock](zx::clock received_clock) { clock = std::move(received_clock); }));

    ExpectCallbacks();

    return clock;
  }
};

// Thin wrapper around AudioRendererTest for grouping only. This tests EnableMinLeadTimeEvents,
// GetMinLeadTime and OnMinLeadTimeChanged, as well as SetPtsUnits and SetPtsContinuityThreshold.
class AudioRendererPtsLeadTimeTest : public AudioRendererTest {};

// Thin wrapper around AudioRendererTest for test case grouping only.
// This group validates AudioRenderer's implementation of SetUsage and SetPcmStreamType.
class AudioRendererFormatUsageTest : public AudioRendererTest {};

// Thin wrapper around AudioRendererTest for test case grouping only.
// This group validates AudioRenderer's implementation of Play and Pause.
class AudioRendererTransportTest : public AudioRendererTest {};

// Thin wrapper around AudioRendererTest for test grouping, to test BindGainControl.
class AudioRendererGainTest : public AudioRendererTest {
  // Most gain tests were moved to gain_control_test.cc. Keep this test fixture intact for now, in
  // anticipation of cases that check interactions between SetGain and Play/Pause gain-ramping.
 protected:
  void SetUp() override {
    AudioRendererTest::SetUp();

    audio_renderer()->BindGainControl(gain_control_.NewRequest());
    AddErrorHandler(gain_control_, "AudioRenderer::GainControl");

    audio_core_->CreateAudioRenderer(audio_renderer_2_.NewRequest());
    AddErrorHandler(audio_renderer_2_, "AudioRenderer2");

    audio_renderer_2_->BindGainControl(gain_control_2_.NewRequest());
    AddErrorHandler(gain_control_2_, "AudioRenderer::GainControl2");
  }

  void TearDown() override {
    gain_control_.Unbind();

    AudioRendererTest::TearDown();
  }

  fuchsia::media::audio::GainControlPtr& gain_control() { return gain_control_; }
  fuchsia::media::AudioRendererPtr& audio_renderer_2() { return audio_renderer_2_; }
  fuchsia::media::audio::GainControlPtr& gain_control_2() { return gain_control_2_; }

 private:
  fuchsia::media::audio::GainControlPtr gain_control_;
  fuchsia::media::AudioRendererPtr audio_renderer_2_;
  fuchsia::media::audio::GainControlPtr gain_control_2_;
};

template <fuchsia::media::AudioSampleFormat SampleType>
class AudioRendererPipelineTest : public HermeticAudioTest {
 protected:
  static constexpr zx::duration kPacketLength = zx::msec(10);
  static constexpr int64_t kNumPacketsInPayload = 50;
  static constexpr int64_t kFramesPerPacketForDisplay = 480;
  // Tolerance to account for scheduling latency.
  static constexpr int64_t kToleranceInPackets = 2;
  // The one-sided filter width of the SincSampler.
  static constexpr int64_t kSincSamplerHalfFilterWidth = 13;
  // The length of gain ramp for each volume change.
  // Must match the constant in audio_core.
  static constexpr zx::duration kVolumeRampDuration = zx::msec(5);

  static constexpr int32_t kFrameRate = 48000;
  static constexpr auto kPacketFrames = kFrameRate / 100;
  static constexpr audio_stream_unique_id_t kUniqueId{{0xff, 0x00}};

  static void SetUpTestSuite() {
    HermeticAudioTest::SetTestSuiteRealmOptions([] {
      return HermeticAudioRealm::Options{
          .audio_core_config_data = MakeAudioCoreConfig({
              .output_device_config = R"x(
                "device_id": "*",
                "supported_stream_types": [
                  "render:media",
                  "render:background",
                  "render:interruption",
                  "render:system_agent",
                  "render:communications"
                ],
                "pipeline": {
                  "name": "default",
                  "streams": [
                    "render:media",
                    "render:background",
                    "render:interruption",
                    "render:system_agent",
                    "render:communications"
                  ],
                  "effects": [
                    {
                      "lib": "audio-core-api-test-effects.so",
                      "effect": "sleeper_filter",
                      "name": "sleeper"
                    }
                  ]
                }
              )x",
          }),
      };
    });
  }

  AudioRendererPipelineTest() : format_(Format::Create<SampleType>(2, kFrameRate).value()) {}

  void SetUp() override {
    HermeticAudioTest::SetUp();
    // The output can store exactly 1s of audio data.
    output_ = CreateOutput(kUniqueId, format_, kFrameRate);
    renderer_ = CreateAudioRenderer(format_, kFrameRate);
  }

  void TearDown() override {
    if constexpr (kEnableAllOverflowAndUnderflowChecksInRealtimeTests) {
      ExpectNoOverflowsOrUnderflows();
    } else {
      // We expect no renderer underflows: we pre-submit the whole signal. Keep that check enabled.
      ExpectNoRendererUnderflows();
    }

    HermeticAudioTest::TearDown();
  }

  static constexpr int32_t kOutputFrameRate = 48000;
  static constexpr int32_t kNumChannels = 2;

  static constexpr int64_t PacketsToFrames(int64_t num_packets, int32_t frame_rate) {
    auto numerator = num_packets * frame_rate * kPacketLength.to_msecs();
    FX_CHECK(numerator % 1000 == 0);
    return numerator / 1000;
  }

  std::pair<AudioRendererShim<SampleType>*, TypedFormat<SampleType>> CreateRenderer(
      int32_t frame_rate,
      fuchsia::media::AudioRenderUsage usage = fuchsia::media::AudioRenderUsage::MEDIA) {
    auto format = Format::Create<SampleType>(2, frame_rate).take_value();
    return std::make_pair(
        CreateAudioRenderer(format, PacketsToFrames(kNumPacketsInPayload, frame_rate), usage),
        format);
  }

  // All pipeline tests send batches of packets. This method returns the suggested size for
  // each batch. We want each batch to be large enough such that the output driver needs to
  // wake multiple times to mix the batch -- this ensures we're testing the timing paths in
  // the driver. We don't have direct access to the driver's timers, however, we know that
  // the driver must wake up at least once every MinLeadTime. Therefore, we return enough
  // packets to exceed one MinLeadTime.
  std::pair<int64_t, int64_t> NumPacketsAndFramesPerBatch(AudioRendererShim<SampleType>* renderer) {
    auto min_lead_time = renderer->min_lead_time();
    FX_CHECK(min_lead_time.get() > 0);
    // In exceptional cases, min_lead_time might be smaller than one packet.
    // Ensure we have at least a handful of packets.
    auto num_packets = std::max(5l, static_cast<int64_t>(min_lead_time / kPacketLength));
    FX_CHECK(num_packets < kNumPacketsInPayload);
    return std::make_pair(num_packets,
                          PacketsToFrames(num_packets, renderer->format().frames_per_second()));
  }

  const TypedFormat<SampleType>& format() const { return format_; }
  VirtualOutput<SampleType>* output() const { return output_; }
  AudioRendererShim<SampleType>* renderer() const { return renderer_; }

 private:
  const TypedFormat<SampleType> format_;
  VirtualOutput<SampleType>* output_ = nullptr;
  AudioRendererShim<SampleType>* renderer_ = nullptr;
};

class AudioRendererGainLimitsTest
    : public AudioRendererPipelineTest<fuchsia::media::AudioSampleFormat::FLOAT> {
 protected:
  static void SetUpTestSuite() {
    HermeticAudioTest::SetTestSuiteRealmOptions([] {
      return HermeticAudioRealm::Options{
          .audio_core_config_data = MakeAudioCoreConfig({
              .output_device_config = R"x(
                "device_id": "*",
                "supported_stream_types": [
                  "render:media",
                  "render:background",
                  "render:interruption",
                  "render:system_agent",
                  "render:communications"
                ],
                "pipeline": {
                  "name": "default",
                  "streams": [
                    "render:media",
                    "render:background",
                    "render:interruption",
                    "render:system_agent",
                    "render:communications"
                  ],
                  "min_gain_db": -20,
                  "max_gain_db": -10
                }
              )x",
          }),
      };
    });
  }

  // The test plays a sequence of constant values with amplitude 1.0. This output waveform's
  // amplitude will be adjusted by the specified stream and usage gains.
  struct TestCase {
    // Calls SetMute(true) if |input_stream_mute|, otherwise SetGain.
    float input_stream_gain_db = 0;
    bool input_stream_mute = false;
    // Calls SetMute(true) if |media_mute|, otherwise SetGain.
    float media_gain_db = 0;
    bool media_mute = false;
    float expected_output_sample = 1.0;
  };

  void Run(TestCase tc) {
    auto [renderer, format] = CreateRenderer(kOutputFrameRate);
    const auto [num_packets, num_frames] = NumPacketsAndFramesPerBatch(renderer);
    const auto frames_per_packet = num_frames / num_packets;
    const auto kSilentPrefix = frames_per_packet;

    // Set stream gain/mute.
    fuchsia::media::audio::GainControlPtr gain_control;
    renderer->fidl()->BindGainControl(gain_control.NewRequest());
    AddErrorHandler(gain_control, "AudioRenderer::GainControl");
    if (tc.input_stream_mute) {
      gain_control->SetMute(true);
    } else {
      gain_control->SetGain(tc.input_stream_gain_db);
    }

    // Set usage gain/mute.
    if (tc.media_mute) {
      fuchsia::media::audio::VolumeControlPtr volume_control;
      audio_core_->BindUsageVolumeControl(
          fuchsia::media::Usage::WithRenderUsage(fuchsia::media::AudioRenderUsage::MEDIA),
          volume_control.NewRequest());
      volume_control->SetMute(true);
    } else {
      audio_core_->SetRenderUsageGain(fuchsia::media::AudioRenderUsage::MEDIA, tc.media_gain_db);
    }

    // Render.
    auto input_buffer = GenerateSilentAudio(format, kSilentPrefix);
    auto signal = GenerateConstantAudio(format, num_frames - kSilentPrefix, 1.0);
    input_buffer.Append(&signal);

    auto packets = renderer->AppendSlice(input_buffer, frames_per_packet);
    renderer->PlaySynchronized(this, output(), 0);
    renderer->WaitForPackets(this, packets);
    auto ring_buffer = output()->SnapshotRingBuffer();
    gain_control.Unbind();
    Unbind(renderer);

    if constexpr (!kEnableAllOverflowAndUnderflowChecksInRealtimeTests) {
      // In case of underflows, exit NOW (don't assess this buffer).
      // TODO(https://fxbug.dev/80003): Remove workarounds when underflow conditions are fixed.
      if (DeviceHasUnderflows(DeviceUniqueIdToString(kUniqueId))) {
        GTEST_SKIP() << "Skipping data checks due to underflows";
        __builtin_unreachable();
      }
    }

    auto expected_output_buffer =
        GenerateConstantAudio(format, num_frames - kSilentPrefix, tc.expected_output_sample);

    CompareAudioBufferOptions opts;
    opts.num_frames_per_packet = kFramesPerPacketForDisplay;
    opts.test_label = "check initial silence";
    CompareAudioBuffers(AudioBufferSlice(&ring_buffer, 0, kSilentPrefix),
                        AudioBufferSlice<fuchsia::media::AudioSampleFormat::FLOAT>(), opts);
    opts.test_label = "check data";
    CompareAudioBuffers(AudioBufferSlice(&ring_buffer, kSilentPrefix, num_frames - kSilentPrefix),
                        AudioBufferSlice(&expected_output_buffer, 0, num_frames - kSilentPrefix),
                        opts);
    opts.test_label = "check final silence";
    CompareAudioBuffers(AudioBufferSlice(&ring_buffer, num_frames, output()->frame_count()),
                        AudioBufferSlice<fuchsia::media::AudioSampleFormat::FLOAT>(), opts);
  }
};

class AudioRendererPipelineUnderflowTest : public HermeticAudioTest {
 protected:
  static constexpr int32_t kFrameRate = 48000;
  static constexpr auto kPacketFrames = kFrameRate / 100;
  static constexpr audio_stream_unique_id_t kUniqueId{{0xff, 0x00}};

  static void SetUpTestSuite() {
    HermeticAudioTest::SetTestSuiteRealmOptions([] {
      return HermeticAudioRealm::Options{
          .audio_core_config_data = MakeAudioCoreConfig({
              .output_device_config = R"x(
                "device_id": "*",
                "supported_stream_types": [
                  "render:media",
                  "render:background",
                  "render:interruption",
                  "render:system_agent",
                  "render:communications"
                ],
                "pipeline": {
                  "name": "default",
                  "streams": [
                    "render:media",
                    "render:background",
                    "render:interruption",
                    "render:system_agent",
                    "render:communications"
                  ],
                  "effects": [
                    {
                      "lib": "audio-core-api-test-effects.so",
                      "effect": "sleeper_filter",
                      "name": "sleeper"
                    }
                  ]
                }
              )x",
          }),
      };
    });
  }

  AudioRendererPipelineUnderflowTest()
      : format_(
            Format::Create<fuchsia::media::AudioSampleFormat::SIGNED_16>(2, kFrameRate).value()) {}

  void SetUp() override {
    HermeticAudioTest::SetUp();
    output_ = CreateOutput(kUniqueId, format_, kFrameRate);
    renderer_ = CreateAudioRenderer(format_, kFrameRate);
  }

  const TypedFormat<fuchsia::media::AudioSampleFormat::SIGNED_16>& format() const {
    return format_;
  }
  VirtualOutput<fuchsia::media::AudioSampleFormat::SIGNED_16>* output() const { return output_; }
  AudioRendererShim<fuchsia::media::AudioSampleFormat::SIGNED_16>* renderer() const {
    return renderer_;
  }

 private:
  const TypedFormat<fuchsia::media::AudioSampleFormat::SIGNED_16> format_;
  VirtualOutput<fuchsia::media::AudioSampleFormat::SIGNED_16>* output_ = nullptr;
  AudioRendererShim<fuchsia::media::AudioSampleFormat::SIGNED_16>* renderer_ = nullptr;
};

class AudioRendererEffectsV1Test
    : public AudioRendererPipelineTest<fuchsia::media::AudioSampleFormat::SIGNED_16> {
 protected:
  // Matches the value in audio_core_config_with_inversion_filter.json
  static constexpr const char* kInverterEffectName = "inverter";

  static void SetUpTestSuite() {
    HermeticAudioTest::SetTestSuiteRealmOptions([] {
      return HermeticAudioRealm::Options{
          .audio_core_config_data = MakeAudioCoreConfig({
              .output_device_config = R"x(
                "device_id": "*",
                "supported_stream_types": [
                  "render:media",
                  "render:background",
                  "render:interruption",
                  "render:system_agent",
                  "render:communications"
                ],
                "pipeline": {
                  "name": "default",
                  "streams": [
                    "render:media",
                    "render:background",
                    "render:interruption",
                    "render:system_agent",
                    "render:communications"
                  ],
                  "effects": [
                    {
                      "lib": "audio-core-api-test-effects.so",
                      "effect": "inversion_filter",
                      "name": "inverter"
                    }
                  ]
                }
              )x",
          }),
      };
    });
  }

  void SetUp() override {
    AudioRendererPipelineTest<fuchsia::media::AudioSampleFormat::SIGNED_16>::SetUp();
    realm().Connect(effects_controller_.NewRequest());
  }

  static void RunInversionFilter(
      AudioBuffer<fuchsia::media::AudioSampleFormat::SIGNED_16>* audio_buffer_ptr) {
    auto& samples = audio_buffer_ptr->samples();
    for (std::remove_pointer_t<decltype(audio_buffer_ptr)>::SampleT& sample : samples) {
      sample = -sample;
    }
  }

  fuchsia::media::audio::EffectsControllerSyncPtr& effects_controller() {
    return effects_controller_;
  }

 private:
  fuchsia::media::audio::EffectsControllerSyncPtr effects_controller_;
};

class AudioRendererEffectsV2Test
    : public AudioRendererPipelineTest<fuchsia::media::AudioSampleFormat::FLOAT> {
 protected:
  static void SetUpTestSuite() {
    HermeticAudioTest::SetTestSuiteRealmOptions([] {
      return HermeticAudioRealm::Options{
          .audio_core_config_data = MakeAudioCoreConfig({
              .output_device_config = R"x(
                "device_id": "*",
                "supported_stream_types": [
                  "render:media",
                  "render:background",
                  "render:interruption",
                  "render:system_agent",
                  "render:communications"
                ],
                "pipeline": {
                  "name": "default",
                  "streams": [
                    "render:media",
                    "render:background",
                    "render:interruption",
                    "render:system_agent",
                    "render:communications"
                  ],
                  "effect_over_fidl": {
                    "name": "inverter"
                  }
                }
              )x",
          }),
          .test_effects_v2 = std::vector<TestEffectsV2::Effect>{{
              .name = "inverter",
              .process = &Invert,
              .process_in_place = true,
              .max_frames_per_call = 1024,
              .frames_per_second = kOutputFrameRate,
              .input_channels = kNumChannels,
              .output_channels = kNumChannels,
          }},
      };
    });
  }

  static zx_status_t Invert(uint64_t num_frames, const float* input, float* output,
                            float total_applied_gain_for_input,
                            std::vector<fuchsia_audio_effects::wire::ProcessMetrics>& metrics) {
    for (uint64_t k = 0; k < num_frames; k++) {
      for (int c = 0; c < kNumChannels; c++) {
        output[k * kNumChannels + c] = -input[k * kNumChannels + c];
      }
    }
    return ZX_OK;
  }
};

class AudioRendererPipelineTuningTest
    : public AudioRendererPipelineTest<fuchsia::media::AudioSampleFormat::SIGNED_16> {
 protected:
  // Matches the value in audio_core_config_with_inversion_filter.json
  static constexpr const char* kInverterEffectName = "inverter";

  static void SetUpTestSuite() {
    HermeticAudioTest::SetTestSuiteRealmOptions([] {
      return HermeticAudioRealm::Options{
          .audio_core_config_data = MakeAudioCoreConfig({
              .output_device_config = R"x(
                "device_id": "*",
                "supported_stream_types": [
                  "render:media",
                  "render:background",
                  "render:interruption",
                  "render:system_agent",
                  "render:communications"
                ],
                "pipeline": {
                  "name": "default",
                  "streams": [
                    "render:media",
                    "render:background",
                    "render:interruption",
                    "render:system_agent",
                    "render:communications"
                  ],
                  "effects": [
                    {
                      "lib": "audio-core-api-test-effects.so",
                      "effect": "inversion_filter",
                      "name": "inverter"
                    }
                  ]
                }
              )x",
          }),
      };
    });
  }

  void SetUp() override {
    AudioRendererPipelineTest<fuchsia::media::AudioSampleFormat::SIGNED_16>::SetUp();
    realm().Connect(audio_tuner_.NewRequest());
  }

  static void RunInversionFilter(
      AudioBuffer<fuchsia::media::AudioSampleFormat::SIGNED_16>* audio_buffer_ptr) {
    auto& samples = audio_buffer_ptr->samples();
    for (std::remove_pointer_t<decltype(audio_buffer_ptr)>::SampleT& sample : samples) {
      sample = -sample;
    }
  }

  fuchsia::media::tuning::AudioTunerPtr& audio_tuner() { return audio_tuner_; }

 private:
  fuchsia::media::tuning::AudioTunerPtr audio_tuner_;
};

}  // namespace media::audio::test

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_TEST_API_AUDIO_RENDERER_TEST_SHARED_H_
