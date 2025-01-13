// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/audio_renderer_server.h"

#include <lib/async-testing/test_loop.h>
#include <lib/fidl/cpp/wire/unknown_interaction_handler.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>

#include <vector>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/media/audio/audio_core/v2/testing/fake_gain_control_server.h"
#include "src/media/audio/audio_core/v2/testing/fake_graph_server.h"
#include "src/media/audio/audio_core/v2/testing/matchers.h"
#include "src/media/audio/lib/format2/format.h"
#include "src/media/audio/services/common/fidl_thread.h"
#include "src/media/audio/services/common/logging.h"

using ::media::audio::RenderUsage;

using ::fuchsia_audio::GainControlSetGainRequest;
using ::fuchsia_audio_mixer::GraphBindProducerLeadTimeWatcherRequest;
using ::fuchsia_audio_mixer::GraphCreateGainControlRequest;
using ::fuchsia_audio_mixer::GraphCreateProducerRequest;
using ::fuchsia_audio_mixer::GraphDeleteGainControlRequest;
using ::fuchsia_audio_mixer::GraphDeleteNodeRequest;
using ::fuchsia_audio_mixer::GraphStartRequest;
using ::fuchsia_audio_mixer::GraphStopRequest;

using ::testing::UnorderedElementsAreArray;

namespace media_audio {
namespace {

const Format kFormat = Format::CreateOrDie({fuchsia_audio::SampleType::kInt32, 1, 1000});
constexpr zx::duration kRampUpOnPlayDuration = zx::msec(5);
constexpr zx::duration kRampDownOnPauseDuration = zx::msec(5);
constexpr float kMinimumGainForPlayPauseRamps = -120.0f;

class AudioRendererEventHandler : public fidl::WireAsyncEventHandler<fuchsia_media::AudioRenderer> {
 public:
  explicit AudioRendererEventHandler(fit::function<void(fidl::UnbindInfo)> on_fidl_error)
      : on_fidl_error_(std::move(on_fidl_error)) {}

  void on_fidl_error(fidl::UnbindInfo error) final { on_fidl_error_(error); }
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_media::AudioRenderer> metadata) override {
    FX_LOGS(WARNING) << "AudioRendererEventHandler: unknown event (AudioRenderer) ordinal "
                     << metadata.event_ordinal;
  }

 private:
  fit::function<void(fidl::UnbindInfo)> on_fidl_error_;
};

struct TestHarness {
  struct Args {
    RenderUsage usage = RenderUsage::MEDIA;
    std::optional<Format> format;
    bool ramp_on_play_pause = true;
  };
  TestHarness(Args args);
  ~TestHarness();

  async::TestLoop loop;
  fidl::Arena<> arena;

  std::shared_ptr<FakeGraphServer> graph_server;
  std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client;

  std::shared_ptr<AudioRendererServer> renderer_server;
  std::shared_ptr<fidl::WireSharedClient<fuchsia_media::AudioRenderer>> renderer_client;
  std::optional<zx_status_t> renderer_close_status;
  bool renderer_fully_created = false;  // true when callback runs
  bool renderer_shutdown = false;       // true when callback runs
};

TestHarness::TestHarness(Args args) {
  auto renderer_endpoints = fidl::CreateEndpoints<fuchsia_media::AudioRenderer>();
  if (!renderer_endpoints.is_ok()) {
    FX_PLOGS(FATAL, renderer_endpoints.status_value()) << "fidl::CreateEndpoints failed";
  }

  auto graph_endpoints = fidl::CreateEndpoints<fuchsia_audio_mixer::Graph>();
  if (!graph_endpoints.is_ok()) {
    FX_PLOGS(FATAL, graph_endpoints.status_value()) << "fidl::CreateEndpoints failed";
  }

  auto thread = FidlThread::CreateFromCurrentThread("test", loop.dispatcher());

  zx::clock clock;
  EXPECT_EQ(zx::clock::create(ZX_CLOCK_OPT_MONOTONIC | ZX_CLOCK_OPT_CONTINUOUS, nullptr, &clock),
            ZX_OK);

  graph_server = FakeGraphServer::Create(thread, std::move(graph_endpoints->server));
  graph_client = std::make_shared<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>>(
      std::move(graph_endpoints->client), loop.dispatcher());

  renderer_server = AudioRendererServer::Create(thread, std::move(renderer_endpoints->server),
                                                {
                                                    .graph_client = graph_client,
                                                    .usage = args.usage,
                                                    .format = args.format,
                                                    .default_reference_clock = std::move(clock),
                                                    .ramp_on_play_pause = args.ramp_on_play_pause,
                                                    .on_fully_created =
                                                        [this](auto server) {
                                                          EXPECT_EQ(server, renderer_server);
                                                          renderer_fully_created = true;
                                                        },
                                                    .on_shutdown =
                                                        [this](auto server) {
                                                          EXPECT_EQ(server, renderer_server);
                                                          renderer_shutdown = true;
                                                        },
                                                });
  renderer_client = std::make_shared<fidl::WireSharedClient<fuchsia_media::AudioRenderer>>(
      std::move(renderer_endpoints->client), loop.dispatcher(),
      std::make_unique<AudioRendererEventHandler>(
          [this](fidl::UnbindInfo info) { this->renderer_close_status = info.status(); }));
}

TestHarness::~TestHarness() {
  renderer_client = nullptr;
  graph_client = nullptr;
  loop.RunUntilIdle();
  EXPECT_TRUE(renderer_server->WaitForShutdown(zx::nsec(0)));
  EXPECT_TRUE(graph_server->WaitForShutdown(zx::nsec(0)));
  EXPECT_TRUE(renderer_shutdown);
}

TEST(AudioRendererServerTest, ErrorSendPacketBeforeConfigured) {
  TestHarness h({});
  (*h.renderer_client)->SendPacket({}).Then([](auto&) {});
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.renderer_close_status, ZX_ERR_BAD_STATE);
}

TEST(AudioRendererServerTest, ErrorPlayBeforeConfigured) {
  TestHarness h({});
  (*h.renderer_client)->Play(0, 0).Then([](auto&) {});
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.renderer_close_status, ZX_ERR_BAD_STATE);
}

TEST(AudioRendererServerTest, ErrorPauseBeforeConfigured) {
  TestHarness h({});
  (*h.renderer_client)->Pause().Then([](auto&) {});
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.renderer_close_status, ZX_ERR_BAD_STATE);
}

TEST(AudioRendererServerTest, ErrorUltrasoundForbidsSetReferenceClock) {
  TestHarness h({
      .usage = RenderUsage::ULTRASOUND,
      .format = kFormat,
  });
  std::ignore = (*h.renderer_client)->SetReferenceClock(zx::clock());
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.renderer_close_status, ZX_ERR_NOT_SUPPORTED);
}

TEST(AudioRendererServerTest, ErrorUltrasoundForbidsSetUsage) {
  TestHarness h({
      .usage = RenderUsage::ULTRASOUND,
      .format = kFormat,
  });
  std::ignore = (*h.renderer_client)
                    ->SetUsage(static_cast<fuchsia_media::AudioRenderUsage>(RenderUsage::MEDIA));
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.renderer_close_status, ZX_ERR_NOT_SUPPORTED);
}

TEST(AudioRendererServerTest, ErrorUltrasoundForbidsSetUsage2) {
  TestHarness h({
      .usage = RenderUsage::ULTRASOUND,
      .format = kFormat,
  });
  std::ignore = (*h.renderer_client)
                    ->SetUsage2(static_cast<fuchsia_media::AudioRenderUsage2>(RenderUsage::MEDIA));
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.renderer_close_status, ZX_ERR_NOT_SUPPORTED);
}

TEST(AudioRendererServerTest, ErrorUltrasoundForbidsSetPcmStreamType) {
  TestHarness h({
      .usage = RenderUsage::ULTRASOUND,
      .format = kFormat,
  });
  std::ignore = (*h.renderer_client)
                    ->SetPcmStreamType(fuchsia_media::wire::AudioStreamType{
                        .sample_format = fuchsia_media::AudioSampleFormat::kSigned16,
                        .channels = 2,
                        .frames_per_second = 1000,
                    });
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.renderer_close_status, ZX_ERR_NOT_SUPPORTED);
}

TEST(AudioRendererServerTest, ConfigureWithDefaults) {
  TestHarness h({
      .format = kFormat,
      .ramp_on_play_pause = true,
  });

  zx::vmo vmo;
  ASSERT_EQ(zx::vmo::create(1024, 0, &vmo), ZX_OK);
  ASSERT_EQ((*h.renderer_client)->AddPayloadBuffer(0, std::move(vmo)).status(), ZX_OK);

  h.loop.RunUntilIdle();
  EXPECT_EQ(h.renderer_close_status, std::nullopt);
  EXPECT_TRUE(h.renderer_server->IsFullyCreated());
  EXPECT_TRUE(h.renderer_fully_created);

  const auto& calls = h.graph_server->calls();
  ASSERT_EQ(calls.size(), 4u);

  static constexpr NodeId kProducerId = 1;
  static constexpr NodeId kStreamGainControlId = 1;
  static constexpr NodeId kPlayPauseRampGainControlId = 2;

  {
    SCOPED_TRACE("calls[0] is CreateProducer");
    auto call = std::get_if<GraphCreateProducerRequest>(&calls[0]);
    ASSERT_TRUE(call);
    EXPECT_EQ(call->direction(), fuchsia_audio_mixer::PipelineDirection::kOutput);
    ASSERT_TRUE(call->data_source());
    ASSERT_TRUE(call->data_source()->stream_sink());
    ASSERT_TRUE(call->data_source()->stream_sink()->server_end());
    EXPECT_TRUE(call->data_source()->stream_sink()->server_end()->is_valid());
    EXPECT_THAT(call->data_source()->stream_sink()->format(), FidlFormatEq(kFormat));
    EXPECT_THAT(call->data_source()->stream_sink()->reference_clock(),
                ValidReferenceClock(fuchsia_hardware_audio::kClockDomainExternal));
    ASSERT_TRUE(call->data_source()->stream_sink()->payload_buffer());
    EXPECT_TRUE(call->data_source()->stream_sink()->payload_buffer()->is_valid());
    ASSERT_FALSE(call->external_delay_watcher());

    auto& media_ticks_per_second = call->data_source()->stream_sink()->media_ticks_per_second();
    ASSERT_TRUE(media_ticks_per_second);
    EXPECT_EQ(media_ticks_per_second->numerator(), 1'000'000'000u);
    EXPECT_EQ(media_ticks_per_second->denominator(), 1u);
  }

  {
    SCOPED_TRACE("calls[1] is CreateGainControl (stream gain control)");
    auto call = std::get_if<GraphCreateGainControlRequest>(&calls[1]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->control());
    ASSERT_TRUE(call->control()->is_valid());
    EXPECT_THAT(call->reference_clock(),
                ValidReferenceClock(fuchsia_hardware_audio::kClockDomainExternal));
  }

  {
    SCOPED_TRACE("calls[2] is CreateGainControl (play/pause ramp control)");
    auto call = std::get_if<GraphCreateGainControlRequest>(&calls[2]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->control());
    ASSERT_TRUE(call->control()->is_valid());
    EXPECT_THAT(call->reference_clock(),
                ValidReferenceClock(fuchsia_hardware_audio::kClockDomainExternal));
  }

  {
    SCOPED_TRACE("calls[3] is BindProducerLeadTimeWatcher");
    auto call = std::get_if<GraphBindProducerLeadTimeWatcherRequest>(&calls[3]);
    ASSERT_TRUE(call);
    EXPECT_EQ(call->id(), kProducerId);
    ASSERT_TRUE(call->server_end());
    ASSERT_TRUE(call->server_end()->is_valid());
  }

  // Closing the connection should delete everything.
  h.renderer_client = nullptr;
  h.loop.RunUntilIdle();

  ASSERT_EQ(calls.size(), 7u);

  {
    SCOPED_TRACE("calls[4] is DeleteNode (producer)");
    auto call = std::get_if<GraphDeleteNodeRequest>(&calls[4]);
    ASSERT_TRUE(call);
    EXPECT_EQ(call->id(), kProducerId);
  }

  {
    SCOPED_TRACE("calls[5] is DeleteGainControl (stream gain)");
    auto call = std::get_if<GraphDeleteGainControlRequest>(&calls[5]);
    ASSERT_TRUE(call);
    EXPECT_EQ(call->id(), kStreamGainControlId);
  }

  {
    SCOPED_TRACE("calls[6] is DeleteGainControl (play-pause-ramp gain)");
    auto call = std::get_if<GraphDeleteGainControlRequest>(&calls[6]);
    ASSERT_TRUE(call);
    EXPECT_EQ(call->id(), kPlayPauseRampGainControlId);
  }
}

TEST(AudioRendererServerTest, ConfigureWithExplicitCalls) {
  TestHarness h({
      .usage = RenderUsage::MEDIA,
      .ramp_on_play_pause = false,
  });

  zx::clock clock;
  EXPECT_EQ(zx::clock::create(ZX_CLOCK_OPT_MONOTONIC | ZX_CLOCK_OPT_CONTINUOUS, nullptr, &clock),
            ZX_OK);

  ASSERT_EQ((*h.renderer_client)->SetPtsUnits(1, 2).status(), ZX_OK);
  ASSERT_EQ((*h.renderer_client)->SetPtsContinuityThreshold(0.2f).status(), ZX_OK);
  ASSERT_EQ((*h.renderer_client)->SetReferenceClock(std::move(clock)).status(), ZX_OK);
  ASSERT_EQ((*h.renderer_client)
                ->SetUsage2(static_cast<fuchsia_media::AudioRenderUsage2>(RenderUsage::BACKGROUND))
                .status(),
            ZX_OK);

  const Format kExpectedFormat =
      Format::CreateOrDie({fuchsia_audio::SampleType::kFloat32, 2, 48000});
  ASSERT_EQ((*h.renderer_client)
                ->SetPcmStreamType({
                    .sample_format = fuchsia_media::AudioSampleFormat::kFloat,
                    .channels = 2,
                    .frames_per_second = 48000,
                })
                .status(),
            ZX_OK);

  zx::vmo vmo;
  ASSERT_EQ(zx::vmo::create(1024, 0, &vmo), ZX_OK);
  ASSERT_EQ((*h.renderer_client)->AddPayloadBuffer(0, std::move(vmo)).status(), ZX_OK);

  h.loop.RunUntilIdle();
  EXPECT_EQ(h.renderer_close_status, std::nullopt);
  EXPECT_TRUE(h.renderer_server->IsFullyCreated());
  EXPECT_TRUE(h.renderer_fully_created);

  const auto& calls = h.graph_server->calls();
  ASSERT_EQ(calls.size(), 3u);

  static constexpr NodeId kProducerId = 1;

  {
    SCOPED_TRACE("calls[0] is CreateProducer");
    auto call = std::get_if<GraphCreateProducerRequest>(&calls[0]);
    ASSERT_TRUE(call);
    EXPECT_EQ(call->direction(), fuchsia_audio_mixer::PipelineDirection::kOutput);
    ASSERT_TRUE(call->data_source());
    ASSERT_TRUE(call->data_source()->stream_sink());
    ASSERT_TRUE(call->data_source()->stream_sink()->server_end());
    EXPECT_TRUE(call->data_source()->stream_sink()->server_end()->is_valid());
    EXPECT_THAT(call->data_source()->stream_sink()->format(), FidlFormatEq(kExpectedFormat));
    EXPECT_THAT(call->data_source()->stream_sink()->reference_clock(),
                ValidReferenceClock(fuchsia_hardware_audio::kClockDomainExternal));
    ASSERT_TRUE(call->data_source()->stream_sink()->payload_buffer());
    EXPECT_TRUE(call->data_source()->stream_sink()->payload_buffer()->is_valid());
    ASSERT_FALSE(call->external_delay_watcher());

    auto& media_ticks_per_second = call->data_source()->stream_sink()->media_ticks_per_second();
    ASSERT_TRUE(media_ticks_per_second);
    EXPECT_EQ(media_ticks_per_second->numerator(), 1u);
    EXPECT_EQ(media_ticks_per_second->denominator(), 2u);
  }

  {
    SCOPED_TRACE("calls[1] is CreateGainControl (stream gain control)");
    auto call = std::get_if<GraphCreateGainControlRequest>(&calls[1]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->control());
    ASSERT_TRUE(call->control()->is_valid());
    EXPECT_THAT(call->reference_clock(),
                ValidReferenceClock(fuchsia_hardware_audio::kClockDomainExternal));
  }

  // there is no play/pause ramp control

  {
    SCOPED_TRACE("calls[2] is BindProducerLeadTimeWatcher");
    auto call = std::get_if<GraphBindProducerLeadTimeWatcherRequest>(&calls[2]);
    ASSERT_TRUE(call);
    EXPECT_EQ(call->id(), kProducerId);
    ASSERT_TRUE(call->server_end());
    ASSERT_TRUE(call->server_end()->is_valid());
  }
}

TEST(AudioRendererServerTest, SendPacket) {
  TestHarness h({
      .format = kFormat,
      .ramp_on_play_pause = false,
  });

  zx::vmo vmo;
  ASSERT_EQ(zx::vmo::create(1024, 0, &vmo), ZX_OK);
  ASSERT_EQ((*h.renderer_client)->AddPayloadBuffer(0, std::move(vmo)).status(), ZX_OK);

  // Send a best-effort packet.
  bool released_packet1 = false;
  (*h.renderer_client)
      ->SendPacket({
          .pts = fuchsia_media::kNoTimestamp,
          .payload_offset = 100,
          .payload_size = 10,
          .flags = fuchsia_media::kStreamPacketFlagDiscontinuity,
      })
      .Then([&released_packet1](auto&) { released_packet1 = true; });

  // Send a continuous packet.
  bool released_packet2 = false;
  (*h.renderer_client)
      ->SendPacket({
          .pts = fuchsia_media::kNoTimestamp,
          .payload_offset = 200,
          .payload_size = 20,
      })
      .Then([&released_packet2](auto&) { released_packet2 = true; });

  // Send a packet with an explicit timestamp.
  bool released_packet3 = false;
  (*h.renderer_client)
      ->SendPacket({
          .pts = 50,
          .payload_offset = 300,
          .payload_size = 30,
      })
      .Then([&released_packet3](auto&) { released_packet3 = true; });

  // Wait for the AudioRendererServer to receive all requests.
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.renderer_close_status, std::nullopt);
  EXPECT_TRUE(h.renderer_server->IsFullyCreated());
  EXPECT_TRUE(h.renderer_fully_created);

  // Check that a ProducerNode was created.
  auto& calls = h.graph_server->calls();
  ASSERT_GE(calls.size(), 1u);
  fidl::ServerEnd<fuchsia_audio::StreamSink> stream_sink_server_end;

  {
    SCOPED_TRACE("calls[0] is CreateProducer");
    auto call = std::get_if<GraphCreateProducerRequest>(&calls[0]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->data_source());
    ASSERT_TRUE(call->data_source()->stream_sink());
    ASSERT_TRUE(call->data_source()->stream_sink()->server_end());
    ASSERT_TRUE(call->data_source()->stream_sink()->server_end()->is_valid());
    stream_sink_server_end = std::move(*call->data_source()->stream_sink()->server_end());
  }

  // Intercept the packets sent to the ProducerNode.
  class StreamSinkServer : public fidl::WireServer<fuchsia_audio::StreamSink> {
   public:
    void PutPacket(PutPacketRequestView request, PutPacketCompleter::Sync& completer) final {
      ASSERT_TRUE(request->has_packet());
      ASSERT_TRUE(request->packet().has_payload());
      ASSERT_TRUE(request->packet().has_timestamp());
      EXPECT_EQ(request->packet().payload().buffer_id, 0u);

      using ::fuchsia_audio::wire::Timestamp;
      switch (++num_packets_) {
        case 1:
          EXPECT_EQ(request->packet().timestamp().Which(), Timestamp::Tag::kUnspecifiedBestEffort);
          EXPECT_EQ(request->packet().payload().offset, 100u);
          EXPECT_EQ(request->packet().payload().size, 10u);
          break;
        case 2:
          EXPECT_EQ(request->packet().timestamp().Which(), Timestamp::Tag::kUnspecifiedContinuous);
          EXPECT_EQ(request->packet().payload().offset, 200u);
          EXPECT_EQ(request->packet().payload().size, 20u);
          break;
        case 3:
          EXPECT_EQ(request->packet().timestamp().Which(), Timestamp::Tag::kSpecified);
          EXPECT_EQ(request->packet().timestamp().specified(), 50);
          EXPECT_EQ(request->packet().payload().offset, 300u);
          EXPECT_EQ(request->packet().payload().size, 30u);
          break;
        default:
          ADD_FAILURE() << "unexpected packet " << num_packets_;
      }

      ASSERT_TRUE(request->has_release_fence());
      ASSERT_TRUE(request->release_fence().is_valid());
      fences_.push_back(std::move(request->release_fence()));
    }

    void End(EndCompleter::Sync& completer) final { ADD_FAILURE() << "unexpected End call"; }
    void StartSegment(StartSegmentRequestView request,
                      StartSegmentCompleter::Sync& completer) final {
      ADD_FAILURE() << "unexpected StartSegment call";
    }
    void WillClose(WillCloseRequestView request, WillCloseCompleter::Sync& completer) final {
      ADD_FAILURE() << "unexpected WillClose call";
    }

    int64_t num_packets() const { return num_packets_; }
    void DropFence(size_t index) {
      ASSERT_GT(fences_.size(), index);
      fences_[index] = zx::eventpair();
    }

   private:
    int64_t num_packets_ = 0;
    std::vector<zx::eventpair> fences_;
  };

  auto stream_sink_server = std::make_shared<StreamSinkServer>();
  auto stream_sink_server_binding =
      fidl::BindServer(h.loop.dispatcher(), std::move(stream_sink_server_end), stream_sink_server);

  // Should receive three packets.
  h.loop.RunUntilIdle();
  EXPECT_EQ(stream_sink_server->num_packets(), 3);

  // Should not have released any packets yet.
  EXPECT_FALSE(released_packet1);
  EXPECT_FALSE(released_packet2);
  EXPECT_FALSE(released_packet3);

  // Release packets one-by-one.
  stream_sink_server->DropFence(0);
  h.loop.RunUntilIdle();
  EXPECT_TRUE(released_packet1);
  EXPECT_FALSE(released_packet2);
  EXPECT_FALSE(released_packet3);

  stream_sink_server->DropFence(1);
  h.loop.RunUntilIdle();
  EXPECT_TRUE(released_packet1);
  EXPECT_TRUE(released_packet2);
  EXPECT_FALSE(released_packet3);

  stream_sink_server->DropFence(2);
  h.loop.RunUntilIdle();
  EXPECT_TRUE(released_packet1);
  EXPECT_TRUE(released_packet2);
  EXPECT_TRUE(released_packet3);
}

void TestPlayPause(const bool with_ramp) {
  TestHarness h({
      .format = kFormat,
      .ramp_on_play_pause = with_ramp,
  });

  zx::vmo vmo;
  ASSERT_EQ(zx::vmo::create(1024, 0, &vmo), ZX_OK);
  ASSERT_EQ((*h.renderer_client)->AddPayloadBuffer(0, std::move(vmo)).status(), ZX_OK);

  static constexpr zx_time_t kPlayReferenceTime = 100;
  static constexpr int64_t kPlayMediaTime = 200;

  static constexpr zx_time_t kStopResponseReferenceTime = 500;
  static constexpr int64_t kStopResponseMediaTime = 600;

  h.graph_server->set_start_response(fuchsia_audio_mixer::GraphStartResponse({
      .reference_time = kPlayReferenceTime,
      .packet_timestamp = kPlayMediaTime,
  }));

  h.graph_server->set_stop_response(fuchsia_audio_mixer::GraphStopResponse({
      .reference_time = kStopResponseReferenceTime,
      .packet_timestamp = kStopResponseMediaTime,
  }));

  (*h.renderer_client)->Play(kPlayReferenceTime, kPlayMediaTime).Then([](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_EQ(result->reference_time, kPlayReferenceTime);
    EXPECT_EQ(result->media_time, kPlayMediaTime);
  });
  (*h.renderer_client)->Pause().Then([](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_EQ(result->reference_time, kStopResponseReferenceTime);
    EXPECT_EQ(result->media_time, kStopResponseMediaTime);
  });

  h.loop.RunUntilIdle();
  EXPECT_EQ(h.renderer_close_status, std::nullopt);
  EXPECT_TRUE(h.renderer_server->IsFullyCreated());
  EXPECT_TRUE(h.renderer_fully_created);

  // Expected GraphServer calls: CreateProducer, CreateGainControl (stream gain), CreateGainControl
  // (ramp gain), BindProducerLeadTimeWatcher, Start, Stop.
  auto& graph_calls = h.graph_server->calls();
  ASSERT_EQ(graph_calls.size(), with_ramp ? 6u : 5u);
  static constexpr NodeId kProducerId = 1;

  // Get the server ends of the two GainControls.
  fidl::ServerEnd<fuchsia_audio::GainControl> stream_gain_control_server_end;
  fidl::ServerEnd<fuchsia_audio::GainControl> ramp_gain_control_server_end;

  {
    auto call = std::get_if<GraphCreateGainControlRequest>(&graph_calls[1]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->control());
    ASSERT_TRUE(call->control()->is_valid());
    stream_gain_control_server_end = std::move(*call->control());
  }

  if (with_ramp) {
    auto call = std::get_if<GraphCreateGainControlRequest>(&graph_calls[2]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->control());
    ASSERT_TRUE(call->control()->is_valid());
    ramp_gain_control_server_end = std::move(*call->control());
  }

  // Check Start/Stop commands.
  {
    auto call = std::get_if<GraphStartRequest>(&graph_calls[with_ramp ? 4 : 3]);
    ASSERT_TRUE(call);
    EXPECT_EQ(call->node_id(), kProducerId);
    ASSERT_TRUE(call->when()->reference_time());
    EXPECT_EQ(call->when()->reference_time().value(), kPlayReferenceTime);
    ASSERT_TRUE(call->stream_time()->packet_timestamp());
    EXPECT_EQ(call->stream_time()->packet_timestamp().value(), kPlayMediaTime);
  }

  zx_time_t pause_reference_time;
  {
    auto call = std::get_if<GraphStopRequest>(&graph_calls[with_ramp ? 5 : 4]);
    ASSERT_TRUE(call);
    EXPECT_EQ(call->node_id(), kProducerId);
    ASSERT_TRUE(call->when()->reference_time());
    pause_reference_time = call->when()->reference_time().value();
  }

  // Check gain commands.
  auto stream_gain_control_server = FakeGainControlServer::Create(
      h.graph_server->thread_ptr(), std::move(stream_gain_control_server_end));

  h.loop.RunUntilIdle();
  EXPECT_EQ(stream_gain_control_server->calls().size(), 0u);

  if (!with_ramp) {
    return;
  }

  auto ramp_gain_control_server = FakeGainControlServer::Create(
      h.graph_server->thread_ptr(), std::move(ramp_gain_control_server_end));

  h.loop.RunUntilIdle();
  EXPECT_EQ(ramp_gain_control_server->calls().size(), 4u);

  // Set+ramp for play.
  {
    auto call = std::get_if<GainControlSetGainRequest>(&ramp_gain_control_server->calls()[0]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->how());
    ASSERT_TRUE(call->how()->gain_db());
    EXPECT_EQ(call->how()->gain_db().value(), kMinimumGainForPlayPauseRamps);
    ASSERT_TRUE(call->when());
    ASSERT_TRUE(call->when()->reference_time());
    EXPECT_EQ(call->when()->reference_time().value(), kPlayReferenceTime);
  }
  {
    auto call = std::get_if<GainControlSetGainRequest>(&ramp_gain_control_server->calls()[1]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->how());
    ASSERT_TRUE(call->how()->ramped());
    EXPECT_EQ(call->how()->ramped().value().target_gain_db(), 0.0f);
    EXPECT_EQ(call->how()->ramped().value().duration(), kRampUpOnPlayDuration.get());
    ASSERT_TRUE(call->how()->ramped().value().function());
    ASSERT_TRUE(call->how()->ramped().value().function()->linear_slope());
    ASSERT_TRUE(call->when());
    ASSERT_TRUE(call->when()->reference_time());
    EXPECT_EQ(call->when()->reference_time().value(), kPlayReferenceTime);
  }

  // Set+ramp for pause.
  {
    auto call = std::get_if<GainControlSetGainRequest>(&ramp_gain_control_server->calls()[2]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->how());
    ASSERT_TRUE(call->how()->gain_db());
    EXPECT_EQ(call->how()->gain_db().value(), 0.0f);
    ASSERT_TRUE(call->when());
    ASSERT_TRUE(call->when()->reference_time());
    EXPECT_EQ(call->when()->reference_time().value(),
              pause_reference_time - kRampDownOnPauseDuration.get());
  }
  {
    auto call = std::get_if<GainControlSetGainRequest>(&ramp_gain_control_server->calls()[3]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->how());
    ASSERT_TRUE(call->how()->ramped());
    EXPECT_EQ(call->how()->ramped().value().target_gain_db(), kMinimumGainForPlayPauseRamps);
    EXPECT_EQ(call->how()->ramped().value().duration(), kRampDownOnPauseDuration.get());
    ASSERT_TRUE(call->how()->ramped().value().function());
    ASSERT_TRUE(call->how()->ramped().value().function()->linear_slope());
    ASSERT_TRUE(call->when());
    ASSERT_TRUE(call->when()->reference_time());
    EXPECT_EQ(call->when()->reference_time().value(),
              pause_reference_time - kRampDownOnPauseDuration.get());
  }
}

TEST(AudioRendererServerTest, PlayPauseWithRamp) { TestPlayPause(/*with_ramp=*/true); }
TEST(AudioRendererServerTest, PlayPauseWithoutRamp) { TestPlayPause(/*with_ramp=*/false); }

}  // namespace
}  // namespace media_audio
