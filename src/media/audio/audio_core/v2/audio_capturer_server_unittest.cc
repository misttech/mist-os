// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/audio_capturer_server.h"

#include <lib/async-testing/test_loop.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>

#include <vector>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/media/audio/audio_core/v2/testing/fake_graph_server.h"
#include "src/media/audio/audio_core/v2/testing/matchers.h"
#include "src/media/audio/lib/format2/format.h"
#include "src/media/audio/services/common/fidl_thread.h"
#include "src/media/audio/services/common/logging.h"
#include "src/media/audio/services/common/memory_mapped_buffer.h"

using ::media::audio::CaptureUsage;

using ::fuchsia_audio::wire::Timestamp;
using ::fuchsia_audio_mixer::GraphCreateConsumerRequest;
using ::fuchsia_audio_mixer::GraphCreateGainControlRequest;
using ::fuchsia_audio_mixer::GraphDeleteGainControlRequest;
using ::fuchsia_audio_mixer::GraphDeleteNodeRequest;
using ::fuchsia_audio_mixer::GraphStartRequest;
using ::fuchsia_audio_mixer::GraphStopRequest;

using ::testing::UnorderedElementsAreArray;

namespace media_audio {
namespace {

const Format kFormat = Format::CreateOrDie({fuchsia_audio::SampleType::kInt32, 1, 1000});

class AudioCapturerEventHandler : public fidl::WireAsyncEventHandler<fuchsia_media::AudioCapturer> {
 public:
  struct Args {
    std::shared_ptr<std::vector<fuchsia_media::wire::StreamPacket>> captured_packets;
    fit::function<void(fidl::UnbindInfo)> on_fidl_error;
  };
  explicit AudioCapturerEventHandler(Args args) : args_(std::move(args)) {}

  void OnPacketProduced(
      ::fidl::WireEvent<::fuchsia_media::AudioCapturer::OnPacketProduced>* event) final {
    args_.captured_packets->push_back(event->packet);
  }
  void OnEndOfStream() final {}
  void on_fidl_error(fidl::UnbindInfo error) final { args_.on_fidl_error(error); }
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_media::AudioCapturer> metadata) override {
    FX_LOGS(WARNING) << "AudioCapturerEventHandler: unknown event (AudioCapturer) ordinal "
                     << metadata.event_ordinal;
  }

 private:
  Args args_;
};

struct TestHarness {
  struct Args {
    CaptureUsage usage = CaptureUsage::FOREGROUND;
    std::optional<Format> format;
    fit::function<void(const fuchsia_media::wire::StreamPacket&)> on_packet_produced = [](auto&) {};
  };
  TestHarness(Args args);
  ~TestHarness();

  async::TestLoop loop;
  fidl::Arena<> arena;

  std::shared_ptr<FakeGraphServer> graph_server;
  std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client;

  std::shared_ptr<AudioCapturerServer> capturer_server;
  std::shared_ptr<fidl::WireSharedClient<fuchsia_media::AudioCapturer>> capturer_client;
  std::shared_ptr<std::vector<fuchsia_media::wire::StreamPacket>> captured_packets;
  std::optional<zx_status_t> capturer_close_status;
  bool capturer_fully_created = false;  // true when callback runs
  bool capturer_shutdown = false;       // true when callback runs
};

TestHarness::TestHarness(Args args) {
  auto capturer_endpoints = fidl::CreateEndpoints<fuchsia_media::AudioCapturer>();
  if (!capturer_endpoints.is_ok()) {
    FX_PLOGS(FATAL, capturer_endpoints.status_value()) << "fidl::CreateEndpoints failed";
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

  captured_packets = std::make_shared<std::vector<fuchsia_media::wire::StreamPacket>>();
  capturer_server = AudioCapturerServer::Create(thread, std::move(capturer_endpoints->server),
                                                {
                                                    .graph_client = graph_client,
                                                    .usage = args.usage,
                                                    .format = args.format,
                                                    .default_reference_clock = std::move(clock),
                                                    .on_fully_created =
                                                        [this](auto server) {
                                                          EXPECT_EQ(server, capturer_server);
                                                          capturer_fully_created = true;
                                                        },
                                                    .on_shutdown =
                                                        [this](auto server) {
                                                          EXPECT_EQ(server, capturer_server);
                                                          capturer_shutdown = true;
                                                        },
                                                });
  capturer_client = std::make_shared<fidl::WireSharedClient<fuchsia_media::AudioCapturer>>(
      std::move(capturer_endpoints->client), loop.dispatcher(),
      std::make_unique<AudioCapturerEventHandler>(AudioCapturerEventHandler::Args{
          .captured_packets = captured_packets,
          .on_fidl_error =
              [this](fidl::UnbindInfo info) { this->capturer_close_status = info.status(); },
      }));
}

TestHarness::~TestHarness() {
  capturer_client = nullptr;
  graph_client = nullptr;
  loop.RunUntilIdle();
  EXPECT_TRUE(capturer_server->WaitForShutdown(zx::nsec(0)));
  EXPECT_TRUE(graph_server->WaitForShutdown(zx::nsec(0)));
  EXPECT_TRUE(capturer_shutdown);
}

void WriteSequentialFrames(const zx::vmo& vmo) {
  auto buffer_result = MemoryMappedBuffer::CreateWithFullSize(vmo, true);
  ASSERT_TRUE(buffer_result.is_ok()) << buffer_result.error();
  auto buffer = buffer_result.take_value();

  // Fill the payload buffer with an integer sequence.
  const int64_t num_frames = static_cast<int64_t>(buffer->size()) / kFormat.bytes_per_frame();
  int32_t* frames = static_cast<int32_t*>(buffer->start());
  for (int32_t k = 0; k < num_frames; k++) {
    frames[k] = k;
  }
}

TEST(AudioCapturerServerTest, ErrorCaptureAtBeforeConfigured) {
  TestHarness h({});
  (*h.capturer_client)->CaptureAt(0, 0, 10).Then([](auto&) {});
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, ZX_ERR_BAD_STATE);
}

TEST(AudioCapturerServerTest, ErrorStartAsyncCaptureBeforeConfigured) {
  TestHarness h({});
  std::ignore = (*h.capturer_client)->StartAsyncCapture(10);
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, ZX_ERR_BAD_STATE);
}

TEST(AudioCapturerServerTest, ErrorStopAsyncCaptureBeforeConfigured) {
  TestHarness h({});
  (*h.capturer_client)->StopAsyncCapture().Then([](auto&) {});
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, ZX_ERR_BAD_STATE);
}

TEST(AudioCapturerServerTest, ErrorReleasePacketBeforeConfigured) {
  TestHarness h({});
  std::ignore = (*h.capturer_client)->ReleasePacket({});
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, ZX_ERR_BAD_STATE);
}

TEST(AudioCapturerServerTest, ErrorUltrasoundForbidsSetReferenceClock) {
  TestHarness h({
      .usage = CaptureUsage::ULTRASOUND,
      .format = kFormat,
  });
  std::ignore = (*h.capturer_client)->SetReferenceClock(zx::clock());
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, ZX_ERR_NOT_SUPPORTED);
}

TEST(AudioCapturerServerTest, ErrorUltrasoundForbidsSetUsage) {
  TestHarness h({
      .usage = CaptureUsage::ULTRASOUND,
      .format = kFormat,
  });
  std::ignore = (*h.capturer_client)->SetUsage(fuchsia_media::AudioCaptureUsage::kForeground);
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, ZX_ERR_NOT_SUPPORTED);
}

TEST(AudioCapturerServerTest, ErrorUltrasoundForbidsSetUsage2) {
  TestHarness h({
      .usage = CaptureUsage::ULTRASOUND,
      .format = kFormat,
  });
  std::ignore = (*h.capturer_client)->SetUsage2(fuchsia_media::AudioCaptureUsage2::kForeground);
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, ZX_ERR_NOT_SUPPORTED);
}

TEST(AudioCapturerServerTest, ErrorUltrasoundForbidsSetPcmStreamType) {
  TestHarness h({
      .usage = CaptureUsage::ULTRASOUND,
      .format = kFormat,
  });
  std::ignore = (*h.capturer_client)
                    ->SetPcmStreamType(fuchsia_media::wire::AudioStreamType{
                        .sample_format = fuchsia_media::AudioSampleFormat::kSigned16,
                        .channels = 2,
                        .frames_per_second = 1000,
                    });
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, ZX_ERR_NOT_SUPPORTED);
}

TEST(AudioCapturerServerTest, ErrorLoopbackForbidsSetUsage) {
  TestHarness h({
      .usage = CaptureUsage::LOOPBACK,
      .format = kFormat,
  });
  std::ignore = (*h.capturer_client)->SetUsage(fuchsia_media::AudioCaptureUsage::kForeground);
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, ZX_ERR_NOT_SUPPORTED);
}

TEST(AudioCapturerServerTest, ErrorLoopbackForbidsSetUsage2) {
  TestHarness h({
      .usage = CaptureUsage::LOOPBACK,
      .format = kFormat,
  });
  std::ignore = (*h.capturer_client)->SetUsage2(fuchsia_media::AudioCaptureUsage2::kForeground);
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, ZX_ERR_NOT_SUPPORTED);
}

TEST(AudioCapturerServerTest, ConfigureWithDefaults) {
  TestHarness h({
      .usage = CaptureUsage::FOREGROUND,
      .format = kFormat,
  });

  zx::vmo vmo;
  ASSERT_EQ(zx::vmo::create(1024, 0, &vmo), ZX_OK);
  ASSERT_EQ((*h.capturer_client)->AddPayloadBuffer(0, std::move(vmo)).status(), ZX_OK);

  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, std::nullopt);
  EXPECT_TRUE(h.capturer_server->IsFullyCreated());
  EXPECT_TRUE(h.capturer_fully_created);

  const auto& calls = h.graph_server->calls();
  ASSERT_EQ(calls.size(), 2u);

  {
    SCOPED_TRACE("calls[0] is CreateConsumer");
    auto call = std::get_if<GraphCreateConsumerRequest>(&calls[0]);
    ASSERT_TRUE(call);
    EXPECT_EQ(call->direction(), fuchsia_audio_mixer::PipelineDirection::kInput);
    ASSERT_TRUE(call->data_sink());
    ASSERT_TRUE(call->data_sink()->stream_sink());
    ASSERT_TRUE(call->data_sink()->stream_sink()->client_end());
    EXPECT_TRUE(call->data_sink()->stream_sink()->client_end()->is_valid());
    EXPECT_THAT(call->data_sink()->stream_sink()->format(), FidlFormatEq(kFormat));
    EXPECT_THAT(call->data_sink()->stream_sink()->reference_clock(),
                ValidReferenceClock(fuchsia_hardware_audio::kClockDomainExternal));
    ASSERT_TRUE(call->data_sink()->stream_sink()->payload_buffer());
    EXPECT_TRUE(call->data_sink()->stream_sink()->payload_buffer()->is_valid());
    EXPECT_EQ(call->data_sink()->stream_sink()->frames_per_packet(), 10 /* 10 ms @ 1k fps */);
    ASSERT_FALSE(call->external_delay_watcher());

    auto& media_ticks_per_second = call->data_sink()->stream_sink()->media_ticks_per_second();
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

  // Closing the connection should delete everything.
  h.capturer_client = nullptr;
  h.loop.RunUntilIdle();

  ASSERT_EQ(calls.size(), 4u);

  {
    SCOPED_TRACE("calls[2] is DeleteNode (consumer)");
    auto call = std::get_if<GraphDeleteNodeRequest>(&calls[2]);
    ASSERT_TRUE(call);
    EXPECT_EQ(call->id(), /*consumer_id=*/1);
  }

  {
    SCOPED_TRACE("calls[3] is DeleteGainControl (stream gain)");
    auto call = std::get_if<GraphDeleteGainControlRequest>(&calls[3]);
    ASSERT_TRUE(call);
    EXPECT_EQ(call->id(), /*stream_gain_control_id=*/1);
  }
}

TEST(AudioCapturerServerTest, ConfigureWithExplicitCalls) {
  TestHarness h({
      .usage = CaptureUsage::FOREGROUND,
      .format = kFormat,
  });

  zx::clock clock;
  EXPECT_EQ(zx::clock::create(ZX_CLOCK_OPT_MONOTONIC | ZX_CLOCK_OPT_CONTINUOUS, nullptr, &clock),
            ZX_OK);

  ASSERT_EQ((*h.capturer_client)->SetReferenceClock(std::move(clock)).status(), ZX_OK);
  ASSERT_EQ(
      (*h.capturer_client)->SetUsage2(fuchsia_media::AudioCaptureUsage2::kBackground).status(),
      ZX_OK);

  const Format kExpectedFormat =
      Format::CreateOrDie({fuchsia_audio::SampleType::kFloat32, 2, 48000});
  ASSERT_EQ((*h.capturer_client)
                ->SetPcmStreamType({
                    .sample_format = fuchsia_media::AudioSampleFormat::kFloat,
                    .channels = 2,
                    .frames_per_second = 48000,
                })
                .status(),
            ZX_OK);

  zx::vmo vmo;
  ASSERT_EQ(zx::vmo::create(1024, 0, &vmo), ZX_OK);
  ASSERT_EQ((*h.capturer_client)->AddPayloadBuffer(0, std::move(vmo)).status(), ZX_OK);

  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, std::nullopt);
  EXPECT_TRUE(h.capturer_server->IsFullyCreated());
  EXPECT_TRUE(h.capturer_fully_created);

  const auto& calls = h.graph_server->calls();
  ASSERT_EQ(calls.size(), 2u);

  {
    SCOPED_TRACE("calls[0] is CreateConsumer");
    auto call = std::get_if<GraphCreateConsumerRequest>(&calls[0]);
    ASSERT_TRUE(call);
    EXPECT_EQ(call->direction(), fuchsia_audio_mixer::PipelineDirection::kInput);
    ASSERT_TRUE(call->data_sink());
    ASSERT_TRUE(call->data_sink()->stream_sink());
    ASSERT_TRUE(call->data_sink()->stream_sink()->client_end());
    EXPECT_TRUE(call->data_sink()->stream_sink()->client_end()->is_valid());
    EXPECT_THAT(call->data_sink()->stream_sink()->format(), FidlFormatEq(kExpectedFormat));
    EXPECT_THAT(call->data_sink()->stream_sink()->reference_clock(),
                ValidReferenceClock(fuchsia_hardware_audio::kClockDomainExternal));
    ASSERT_TRUE(call->data_sink()->stream_sink()->payload_buffer());
    EXPECT_TRUE(call->data_sink()->stream_sink()->payload_buffer()->is_valid());
    EXPECT_EQ(call->data_sink()->stream_sink()->frames_per_packet(), 480 /* 10 ms @ 48k fps */);
    ASSERT_FALSE(call->external_delay_watcher());

    auto& media_ticks_per_second = call->data_sink()->stream_sink()->media_ticks_per_second();
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
}

TEST(AudioCapturerServerTest, CaptureAt) {
  TestHarness h({
      .usage = CaptureUsage::FOREGROUND,
      .format = kFormat,
  });

  zx::vmo vmo, vmo_dup;
  ASSERT_EQ(zx::vmo::create(1024, 0, &vmo), ZX_OK);
  ASSERT_EQ(vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo_dup), ZX_OK);
  ASSERT_EQ((*h.capturer_client)->AddPayloadBuffer(0, std::move(vmo_dup)).status(), ZX_OK);

  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, std::nullopt);
  EXPECT_TRUE(h.capturer_server->IsFullyCreated());
  EXPECT_TRUE(h.capturer_fully_created);

  // Check that a ConsumerNode was created.
  auto& calls = h.graph_server->calls();
  ASSERT_EQ(calls.size(), 2u);
  fidl::ClientEnd<fuchsia_audio::StreamSink> stream_sink_client_end;

  {
    SCOPED_TRACE("calls[0] is CreateConsumer");
    auto call = std::get_if<GraphCreateConsumerRequest>(&calls[0]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->data_sink());
    ASSERT_TRUE(call->data_sink()->stream_sink());
    ASSERT_TRUE(call->data_sink()->stream_sink()->client_end());
    ASSERT_TRUE(call->data_sink()->stream_sink()->client_end()->is_valid());
    stream_sink_client_end = std::move(*call->data_sink()->stream_sink()->client_end());

    ASSERT_TRUE(call->data_sink()->stream_sink()->payload_buffer());
    EXPECT_TRUE(call->data_sink()->stream_sink()->payload_buffer()->is_valid());
    WriteSequentialFrames(*call->data_sink()->stream_sink()->payload_buffer());
  }

  auto stream_sink_client =
      fidl::WireClient(std::move(stream_sink_client_end), h.loop.dispatcher());

  static constexpr auto kFramesPerPacket = 10;
  static const auto kBytesPerPacket =
      kFramesPerPacket * static_cast<uint32_t>(kFormat.bytes_per_frame());
  static const std::vector<int64_t> packet_timestamps{
      kFormat.duration_per(Fixed(0 * kFramesPerPacket)).get(),
      kFormat.duration_per(Fixed(1 * kFramesPerPacket)).get(),
      kFormat.duration_per(Fixed(2 * kFramesPerPacket)).get(),
  };

  auto mapped_vmo_result = MemoryMappedBuffer::CreateWithFullSize(vmo, true);
  ASSERT_TRUE(mapped_vmo_result.is_ok()) << mapped_vmo_result.error();
  auto mapped_vmo = mapped_vmo_result.take_value();

  // These three captures should receive three packets.
  int capture_count = 0;
  for (uint32_t k = 0; k < 3; k++) {
    (*h.capturer_client)
        ->CaptureAt(0, /*offset=*/k * kBytesPerPacket, /*frames=*/10)
        .Then([k, &capture_count, mapped_vmo](auto& result) mutable {
          SCOPED_TRACE("received captured packet " + std::to_string(k));
          ASSERT_TRUE(result.ok());
          auto& packet = result->captured_packet;
          EXPECT_EQ(packet.pts, packet_timestamps[k]);
          EXPECT_EQ(packet.payload_buffer_id, 0u);
          EXPECT_EQ(packet.payload_offset, k * kBytesPerPacket);
          EXPECT_EQ(packet.payload_size, kBytesPerPacket);

          // The packet data should be an increasing sequence starting from payload_offset.
          const int32_t first_value = static_cast<int32_t>(packet.payload_offset) /
                                      static_cast<int32_t>(kFormat.bytes_per_sample());

          int32_t* data = static_cast<int32_t*>(mapped_vmo->offset(packet.payload_offset));
          for (auto i = 0; i < kFramesPerPacket * kFormat.channels(); i++) {
            EXPECT_EQ(data[i], first_value + i) << "at data[" << i << "]";
          }

          capture_count++;
        });
  }

  // Must have called Graph.Start.
  h.loop.RunUntilIdle();
  ASSERT_EQ(calls.size(), 3u);
  EXPECT_TRUE(std::holds_alternative<GraphStartRequest>(calls[2]));

  // Simulate three packets from the ConsumerNode.
  // These will be captured by the above three CaptureAt calls.
  for (uint32_t k = 0; k < 3; k++) {
    SCOPED_TRACE("queue source packet " + std::to_string(k));
    auto result = stream_sink_client->PutPacket(
        fuchsia_audio::wire::StreamSinkPutPacketRequest::Builder(h.arena)
            .packet(fuchsia_audio::wire::Packet::Builder(h.arena)
                        .payload({
                            .buffer_id = 0,
                            .offset = k * kBytesPerPacket,
                            .size = kBytesPerPacket,
                        })
                        .timestamp(Timestamp::WithSpecified(h.arena, packet_timestamps[k]))
                        .Build())
            .Build());
    ASSERT_TRUE(result.ok()) << result;
  }

  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, std::nullopt);
  EXPECT_EQ(capture_count, 3);
}

TEST(AudioCapturerServerTest, SyncCapture) {
  TestHarness h({
      .usage = CaptureUsage::FOREGROUND,
      .format = kFormat,
  });

  zx::vmo vmo, vmo_dup;
  ASSERT_EQ(zx::vmo::create(1024, 0, &vmo), ZX_OK);
  ASSERT_EQ(vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo_dup), ZX_OK);
  ASSERT_EQ((*h.capturer_client)->AddPayloadBuffer(0, std::move(vmo_dup)).status(), ZX_OK);

  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, std::nullopt);
  EXPECT_TRUE(h.capturer_server->IsFullyCreated());
  EXPECT_TRUE(h.capturer_fully_created);

  // Check that a ConsumerNode was created.
  auto& calls = h.graph_server->calls();
  ASSERT_EQ(calls.size(), 2u);
  fidl::ClientEnd<fuchsia_audio::StreamSink> stream_sink_client_end;

  {
    SCOPED_TRACE("calls[0] is CreateConsumer");
    auto call = std::get_if<GraphCreateConsumerRequest>(&calls[0]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->data_sink());
    ASSERT_TRUE(call->data_sink()->stream_sink());
    ASSERT_TRUE(call->data_sink()->stream_sink()->client_end());
    ASSERT_TRUE(call->data_sink()->stream_sink()->client_end()->is_valid());
    stream_sink_client_end = std::move(*call->data_sink()->stream_sink()->client_end());

    ASSERT_TRUE(call->data_sink()->stream_sink()->payload_buffer());
    EXPECT_TRUE(call->data_sink()->stream_sink()->payload_buffer()->is_valid());
    WriteSequentialFrames(*call->data_sink()->stream_sink()->payload_buffer());
  }

  auto mapped_vmo_result = MemoryMappedBuffer::CreateWithFullSize(vmo, true);
  ASSERT_TRUE(mapped_vmo_result.is_ok()) << mapped_vmo_result.error();
  auto mapped_vmo = mapped_vmo_result.take_value();

  auto stream_sink_client =
      fidl::WireClient(std::move(stream_sink_client_end), h.loop.dispatcher());

  // Make packets large enough so there are just 3 packets per payload buffer, so we can test
  // wrap-around with the fourth packet.
  static const auto kFramesPerPayload =
      static_cast<int64_t>(mapped_vmo->size()) / kFormat.bytes_per_frame();
  static const auto kFramesPerPacket = kFramesPerPayload / 3;
  static const auto kBytesPerPacket =
      static_cast<uint32_t>(kFramesPerPacket * kFormat.bytes_per_frame());
  static const std::vector<int64_t> packet_timestamps{
      kFormat.duration_per(Fixed(0 * kFramesPerPacket)).get(),
      kFormat.duration_per(Fixed(1 * kFramesPerPacket)).get(),
      kFormat.duration_per(Fixed(2 * kFramesPerPacket)).get(),
      kFormat.duration_per(Fixed(3 * kFramesPerPacket)).get(),
  };

  // Start an async capture.
  ASSERT_EQ(
      (*h.capturer_client)->StartAsyncCapture(static_cast<uint32_t>(kFramesPerPacket)).status(),
      ZX_OK);
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, std::nullopt);

  // Must have called Graph.Start.
  ASSERT_EQ(calls.size(), 3u);
  EXPECT_TRUE(std::holds_alternative<GraphStartRequest>(calls[2]));

  // Simulate four packets from the ConsumerNode.
  for (uint32_t k = 0; k < 4; k++) {
    SCOPED_TRACE("queue source packet " + std::to_string(k));
    auto result = stream_sink_client->PutPacket(
        fuchsia_audio::wire::StreamSinkPutPacketRequest::Builder(h.arena)
            .packet(fuchsia_audio::wire::Packet::Builder(h.arena)
                        .payload({
                            .buffer_id = 0,
                            .offset = (k % 3) * kBytesPerPacket,
                            .size = kBytesPerPacket,
                        })
                        .timestamp(Timestamp::WithSpecified(h.arena, packet_timestamps[k]))
                        .Build())
            .Build());
    ASSERT_TRUE(result.ok()) << result;
  }

  // We should receive three packets.
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, std::nullopt);
  ASSERT_EQ(h.captured_packets->size(), 3u);

  for (uint32_t k = 0; k < 3; k++) {
    SCOPED_TRACE("verifying captured packet " + std::to_string(k));
    auto& packet = (*h.captured_packets)[k];
    EXPECT_EQ(packet.pts, packet_timestamps[k]);
    EXPECT_EQ(packet.payload_buffer_id, 0u);
    EXPECT_EQ(packet.payload_offset, k * kBytesPerPacket);
    EXPECT_EQ(packet.payload_size, kBytesPerPacket);

    // The packet data should be an increasing sequence starting from payload_offset.
    const int32_t first_value = static_cast<int32_t>(packet.payload_offset) /
                                static_cast<int32_t>(kFormat.bytes_per_sample());

    int32_t* data = static_cast<int32_t*>(mapped_vmo->offset(packet.payload_offset));
    for (auto i = 0; i < kFramesPerPacket * kFormat.channels(); i++) {
      // use ASSERT_EQ instead of EXPECT_EQ to reduce error spam if there's a bug: if one sample is
      // wrong, then ~all samples are probably wrong.
      ASSERT_EQ(data[i], first_value + i) << "at data[" << i << "]";
    }
  }

  // Releasing one packet should allow us to receive the fourth packet.
  ASSERT_EQ((*h.capturer_client)->ReleasePacket((*h.captured_packets)[0]).status(), ZX_OK);
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, std::nullopt);
  ASSERT_EQ(h.captured_packets->size(), 4u);

  {
    SCOPED_TRACE("verifying captured packet 3");
    auto& packet = (*h.captured_packets)[3];
    EXPECT_EQ(packet.pts, packet_timestamps[3]);
    // These should match captured_packets[0].
    EXPECT_EQ(packet.payload_buffer_id, 0u);
    EXPECT_EQ(packet.payload_offset, 0 * kBytesPerPacket);
    EXPECT_EQ(packet.payload_size, kBytesPerPacket);
  }

  // Stop the capture.
  ASSERT_EQ((*h.capturer_client)->StopAsyncCaptureNoReply().status(), ZX_OK);
  h.loop.RunUntilIdle();
  EXPECT_EQ(h.capturer_close_status, std::nullopt);
  ASSERT_EQ(h.captured_packets->size(), 4u);

  // Must have called Graph.Stop.
  ASSERT_EQ(calls.size(), 4u);
  EXPECT_TRUE(std::holds_alternative<GraphStopRequest>(calls[3]));
}

}  // namespace
}  // namespace media_audio
