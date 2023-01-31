// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fuchsia/media/playback/cpp/fidl.h>
#include <fuchsia/sys/cpp/fidl.h>
#include <fuchsia/ui/views/cpp/fidl.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/time.h>

#include <array>
#include <memory>
#include <optional>
#include <queue>
#include <string_view>
#include <type_traits>

#include <gtest/gtest.h>
#include <src/lib/testing/loop_fixture/real_loop_fixture.h>

#include "lib/async-loop/cpp/loop.h"
#include "lib/media/cpp/timeline_function.h"
#include "lib/media/cpp/type_converters.h"
#include "lib/ui/scenic/cpp/view_token_pair.h"
#include "src/lib/fsl/io/fd.h"
#include "src/media/playback/mediaplayer/test/command_queue.h"
#include "src/media/playback/mediaplayer/test/fakes/fake_audio.h"
#include "src/media/playback/mediaplayer/test/fakes/fake_scenic.h"
#include "src/media/playback/mediaplayer/test/fakes/fake_sysmem.h"
#include "src/media/playback/mediaplayer/test/fakes/fake_wav_reader.h"
#include "src/media/playback/mediaplayer/test/sink_feeder.h"
#include "zircon/status.h"

namespace media_player {
namespace test {
using namespace component_testing;

static constexpr uint16_t kSamplesPerFrame = 2;      // Stereo
static constexpr uint32_t kFramesPerSecond = 48000;  // 48kHz
static constexpr size_t kSinkFeedSize = 65536;
static constexpr uint32_t kSinkFeedMaxPacketSize = 4096;
static constexpr uint32_t kSinkFeedMaxPacketCount = 10;
constexpr char kBearFilePath[] = "/pkg/data/media_test_data/bear.mp4";
constexpr char kOpusFilePath[] = "/pkg/data/media_test_data/sfx-opus-441.webm";

// Base class for mediaplayer tests.
class MediaPlayerTests : public gtest::RealLoopFixture {
 protected:
  MediaPlayerTests()
      : fake_reader_(dispatcher()),
        fake_audio_owned_(std::make_unique<FakeAudio>(dispatcher())),
        fake_audio_(fake_audio_owned_.get()),
        fake_scenic_owned_(std::make_unique<FakeScenic>(dispatcher())),
        fake_scenic_(fake_scenic_owned_.get()),
        fake_sysmem_owned_(std::make_unique<FakeSysmem>(dispatcher())),
        fake_sysmem_(fake_sysmem_owned_.get()) {}

  void SetUp() override {
    auto realm_builder = component_testing::RealmBuilder::Create();

    realm_builder.AddChild("mediaplayer", "#meta/mediaplayer.cm");
    realm_builder.AddLocalChild("audio", [this]() {
      EXPECT_TRUE(!!fake_audio_owned_);
      return std::move(fake_audio_owned_);
    });
    realm_builder.AddLocalChild("sysmem", [this]() {
      EXPECT_TRUE(!!fake_sysmem_owned_);
      return std::move(fake_sysmem_owned_);
    });
    realm_builder.AddLocalChild("scenic", [this]() {
      EXPECT_TRUE(!!fake_scenic_owned_);
      return std::move(fake_scenic_owned_);
    });
    fake_scenic_->SetSysmemAllocator(fake_sysmem_);

    // Route fuchsia.media.playback.Player up to the parent
    realm_builder.AddRoute(component_testing::Route{
        .capabilities = {component_testing::Protocol{fuchsia::media::playback::Player::Name_}},
        .source = component_testing::ChildRef{"mediaplayer"},
        .targets = {component_testing::ParentRef{}}});

    // // Route fuchsia.media.Audio from audio child to mediaplayer child
    realm_builder.AddRoute(Route{.capabilities = {Protocol{fuchsia::media::Audio::Name_}},
                                 .source = ChildRef{"audio"},
                                 .targets = {ChildRef{"mediaplayer"}}});

    realm_builder.AddRoute(Route{.capabilities = {Protocol{"fuchsia.sysmem.Allocator"}},
                                 .source = ChildRef{"sysmem"},
                                 .targets = {ChildRef{"mediaplayer"}}});

    realm_builder.AddRoute(Route{.capabilities = {Protocol{"fuchsia.ui.scenic.Scenic"}},
                                 .source = ChildRef{"scenic"},
                                 .targets = {ChildRef{"mediaplayer"}}});

    realm_builder.AddRoute(Route{.capabilities =
                                     {
                                         Protocol{"fuchsia.logger.LogSink"},
                                         Protocol{"fuchsia.tracing.provider.Registry"},
                                         Protocol{"fuchsia.scheduler.ProfileProvider"},
                                         Protocol{"fuchsia.mediacodec.CodecFactory"},
                                     },
                                 .source = ParentRef(),
                                 .targets = {ChildRef{"mediaplayer"}}});

    realm_ = realm_builder.Build(dispatcher());
    teardown_callback_ = realm_->TeardownCallback();

    zx_status_t const status = realm_->component().Connect(player_.NewRequest());

    FX_CHECK(status == ZX_OK);

    commands_.Init(player_.get());

    player_.set_error_handler([this](zx_status_t status) {
      FX_LOGS(ERROR) << "Player connection closed, status " << zx_status_get_string(status) << ".";
      player_connection_closed_ = true;
      QuitLoop();
    });

    player_.events().OnStatusChanged = [this](fuchsia::media::playback::PlayerStatus status) {
      commands_.NotifyStatusChanged(status);
    };
  }

  void TearDown() override {
    EXPECT_FALSE(player_connection_closed_);
    player_ = nullptr;
    realm_.reset();
    // Wait for realm to teardown before fakes, so we don't end up with crashes that lead to flakes.
    RunLoopUntil(std::move(teardown_callback_));
  }

  zx::vmo CreateVmo(size_t size) {
    zx::vmo result;
    zx_status_t status = zx::vmo::create(size, 0, &result);
    FX_CHECK(status == ZX_OK);
    return result;
  }

  // Queues commands to wait for end of stream and to call |QuitLoop|.
  void QuitOnEndOfStream() {
    commands_.WaitForEndOfStream();
    commands_.Invoke([this]() { QuitLoop(); });
  }

  // Executes queued commands
  void Execute() {
    commands_.Execute();
    RunLoop();
  }

  // Creates a view.
  void CreateView() {
    auto [view_token, view_holder_token] = scenic::ViewTokenPair::New();
    player_->CreateView(std::move(view_token));
    view_holder_token_ = std::move(view_holder_token);
  }

  std::list<std::unique_ptr<FakeSysmem::Expectations>> BlackImageSysmemExpectations();
  std::list<std::unique_ptr<FakeSysmem::Expectations>> BearVideoImageSysmemExpectations();
  std::list<std::unique_ptr<FakeSysmem::Expectations>> BearSysmemExpectations();

  FakeWavReader fake_reader_;
  std::unique_ptr<FakeAudio> fake_audio_owned_;
  FakeAudio* fake_audio_;
  std::unique_ptr<FakeScenic> fake_scenic_owned_;
  FakeScenic* fake_scenic_;
  std::unique_ptr<FakeSysmem> fake_sysmem_owned_;
  FakeSysmem* fake_sysmem_;
  std::optional<component_testing::RealmRoot> realm_;

  fuchsia::media::playback::PlayerPtr player_;
  bool player_connection_closed_ = false;
  fit::function<bool()> teardown_callback_;

  fuchsia::ui::views::ViewHolderToken view_holder_token_;
  bool sink_connection_closed_ = false;
  SinkFeeder sink_feeder_;
  CommandQueue commands_;
};

std::list<std::unique_ptr<FakeSysmem::Expectations>>
MediaPlayerTests::BlackImageSysmemExpectations() {
  std::list<std::unique_ptr<FakeSysmem::Expectations>> result;
  result.push_back(
      std::make_unique<FakeSysmem::Expectations>(FakeSysmem::Expectations{
          .constraints_ =
              {fuchsia::sysmem::BufferCollectionConstraints{
                   .usage =
                       {
                           .cpu =
                               fuchsia::sysmem::cpuUsageRead | fuchsia::sysmem::cpuUsageReadOften |
                               fuchsia::sysmem::cpuUsageWrite | fuchsia::sysmem::cpuUsageWriteOften,
                       },
                   .min_buffer_count = 1,
                   .has_buffer_memory_constraints = true,
                   .buffer_memory_constraints =
                       {
                           .min_size_bytes = 16,
                           .ram_domain_supported =
                               true,
                       },
                   .image_format_constraints_count = 1,
                   .image_format_constraints =
                       {
                           fuchsia::sysmem::ImageFormatConstraints{
                               .pixel_format = {.type = fuchsia::sysmem::PixelFormatType::R8G8B8A8},
                               .color_spaces_count = 1,
                               .color_space =
                                   {
                                       fuchsia::sysmem::ColorSpace{
                                           .type = fuchsia::sysmem::ColorSpaceType::SRGB},
                                   },
                               .required_min_coded_width = 2,
                               .required_max_coded_width = 2,
                               .required_min_coded_height = 2,
                               .required_max_coded_height = 2,
                           },
                       },
               },
               fuchsia::sysmem::BufferCollectionConstraints{
                   .usage =
                       {
                           .cpu =
                               fuchsia::sysmem::cpuUsageRead | fuchsia::sysmem::cpuUsageReadOften,
                       },
                   .has_buffer_memory_constraints = true,
                   .buffer_memory_constraints =
                       {
                           .ram_domain_supported = true,
                       },
               }},
          .collection_info_ =
              {
                  .buffer_count = 2,
                  .settings = {.buffer_settings =
                                   {
                                       .size_bytes = 128,
                                       .coherency_domain = fuchsia::sysmem::CoherencyDomain::RAM,
                                       .heap = fuchsia::sysmem::HeapType::SYSTEM_RAM,
                                   },
                               .has_image_format_constraints = true,
                               .image_format_constraints =
                                   {
                                       .pixel_format =
                                           {.type = fuchsia::sysmem::PixelFormatType::R8G8B8A8},
                                       .color_spaces_count = 1,
                                       .color_space =
                                           {
                                               fuchsia::sysmem::ColorSpace{
                                                   .type = fuchsia::sysmem::ColorSpaceType::SRGB}},
                                       .min_coded_width = 0,
                                       .max_coded_width = 16384,
                                       .min_coded_height = 0,
                                       .max_coded_height = 16384,
                                       .min_bytes_per_row = 4,
                                       .max_bytes_per_row = 4294967295,
                                       .bytes_per_row_divisor = 64,
                                       .start_offset_divisor = 4,
                                       .required_min_coded_width = 2,
                                       .required_max_coded_width = 2,
                                       .required_min_coded_height = 2,
                                       .required_max_coded_height = 2,
                                       .required_min_bytes_per_row = 4294967295,
                                       .required_max_bytes_per_row = 0,
                                   }},
                  .buffers =
                      {
                          fuchsia::sysmem::VmoBuffer{.vmo = CreateVmo(4096)},
                          fuchsia::sysmem::VmoBuffer{.vmo = CreateVmo(4096)},
                      },
              }}));

  return result;
}

std::list<std::unique_ptr<FakeSysmem::Expectations>>
MediaPlayerTests::BearVideoImageSysmemExpectations() {
  std::list<std::unique_ptr<FakeSysmem::Expectations>> result;

  // Video buffers
  result.push_back(std::make_unique<FakeSysmem::Expectations>(FakeSysmem::Expectations{
      .constraints_ =
          {fuchsia::sysmem::BufferCollectionConstraints{
               .usage =
                   {
                       .cpu = fuchsia::sysmem::cpuUsageRead | fuchsia::sysmem::cpuUsageReadOften,
                   },
               .has_buffer_memory_constraints = true,
               .buffer_memory_constraints =
                   {
                       .ram_domain_supported = true,
                   },
           },
           fuchsia::sysmem::BufferCollectionConstraints{
               .usage =
                   {
                       .cpu = fuchsia::sysmem::cpuUsageWrite |
                              fuchsia::sysmem::cpuUsageWriteOften,
                   },
               .min_buffer_count_for_camping = 6,
               .has_buffer_memory_constraints = true,
               .buffer_memory_constraints =
                   {
                       .min_size_bytes = 1416960,
                       .ram_domain_supported = true,
                   },
               .image_format_constraints_count = 1,
               .image_format_constraints =
                   {
                       fuchsia::sysmem::ImageFormatConstraints{
                           .pixel_format = {.type = fuchsia::sysmem::PixelFormatType::I420},
                           .color_spaces_count = 1,
                           .color_space =
                               {
                                   fuchsia::sysmem::ColorSpace{
                                       .type = fuchsia::sysmem::ColorSpaceType::REC709},
                               },
                           .required_min_coded_width = 1280,
                           .required_max_coded_width = 1280,
                           .required_min_coded_height = 738,
                           .required_max_coded_height = 738,
                       },
                   },
           }},
      .collection_info_ =
          {
              .buffer_count = 8,
              .settings = {.buffer_settings =
                               {
                                   .size_bytes = 1416960,
                                   .coherency_domain = fuchsia::sysmem::CoherencyDomain::RAM,
                                   .heap = fuchsia::sysmem::HeapType::SYSTEM_RAM,
                               },
                           .has_image_format_constraints = true,
                           .image_format_constraints =
                               {
                                   .pixel_format = {.type = fuchsia::sysmem::PixelFormatType::I420},
                                   .color_spaces_count = 1,
                                   .color_space =
                                       {
                                           fuchsia::sysmem::ColorSpace{
                                               .type = fuchsia::sysmem::ColorSpaceType::REC709}},
                                   .min_coded_width = 0,
                                   .max_coded_width = 16384,
                                   .min_coded_height = 0,
                                   .max_coded_height = 16384,
                                   .min_bytes_per_row = 4,
                                   .max_bytes_per_row = 4294967295,
                                   .coded_width_divisor = 2,
                                   .coded_height_divisor = 2,
                                   .bytes_per_row_divisor = 16,
                                   .start_offset_divisor = 2,
                                   .required_min_coded_width = 1280,
                                   .required_max_coded_width = 1280,
                                   .required_min_coded_height = 738,
                                   .required_max_coded_height = 738,
                                   .required_min_bytes_per_row = 4294967295,
                                   .required_max_bytes_per_row = 0,
                               }},
              .buffers =
                  {
                      fuchsia::sysmem::VmoBuffer{.vmo = CreateVmo(1417216)},
                      fuchsia::sysmem::VmoBuffer{.vmo = CreateVmo(1417216)},
                      fuchsia::sysmem::VmoBuffer{.vmo = CreateVmo(1417216)},
                      fuchsia::sysmem::VmoBuffer{.vmo = CreateVmo(1417216)},
                      fuchsia::sysmem::VmoBuffer{.vmo = CreateVmo(1417216)},
                      fuchsia::sysmem::VmoBuffer{.vmo = CreateVmo(1417216)},
                      fuchsia::sysmem::VmoBuffer{.vmo = CreateVmo(1417216)},
                      fuchsia::sysmem::VmoBuffer{.vmo = CreateVmo(1417216)},
                  },
          }}));

  return result;
}

std::list<std::unique_ptr<FakeSysmem::Expectations>> MediaPlayerTests::BearSysmemExpectations() {
  auto result = BlackImageSysmemExpectations();
  result.splice(result.end(), BearVideoImageSysmemExpectations());
  return result;
}

// Play a synthetic WAV file from beginning to end.
TEST_F(MediaPlayerTests, PlayWav) {
  fake_audio_->renderer().ExpectPackets({{0, 4096, 0x20c39d1e31991800},
                                         {1024, 4096, 0xeaf137125d313800},
                                         {2048, 4096, 0x6162095671991800},
                                         {3072, 4096, 0x36e551c7dd41f800},
                                         {4096, 4096, 0x23dcbf6fb1991800},
                                         {5120, 4096, 0xee0a5963dd313800},
                                         {6144, 4096, 0x647b2ba7f1991800},
                                         {7168, 4096, 0x39fe74195d41f800},
                                         {8192, 4096, 0xb3de76b931991800},
                                         {9216, 4096, 0x7e0c10ad5d313800},
                                         {10240, 4096, 0xf47ce2f171991800},
                                         {11264, 4096, 0xca002b62dd41f800},
                                         {12288, 4096, 0xb6f7990ab1991800},
                                         {13312, 4096, 0x812532fedd313800},
                                         {14336, 4096, 0xf7960542f1991800},
                                         {15360, 4052, 0x7308a9824acbd5ea}});

  fuchsia::media::playback::SeekingReaderPtr fake_reader_ptr;
  fidl::InterfaceRequest<fuchsia::media::playback::SeekingReader> reader_request =
      fake_reader_ptr.NewRequest();
  fake_reader_.Bind(std::move(reader_request));

  fuchsia::media::playback::SourcePtr source;
  player_->CreateReaderSource(std::move(fake_reader_ptr), source.NewRequest());
  player_->SetSource(std::move(source));

  commands_.Play();
  QuitOnEndOfStream();

  Execute();
  EXPECT_TRUE(fake_audio_->renderer().expected());
}

// Play a synthetic WAV file from beginning to end, delaying the retirement of
// the last packet to simulate delayed end-of-stream recognition.
// TODO(fxbug.dev/35616): Flaking.
TEST_F(MediaPlayerTests, PlayWavDelayEos) {
  fake_audio_->renderer().ExpectPackets({{0, 4096, 0x20c39d1e31991800},
                                         {1024, 4096, 0xeaf137125d313800},
                                         {2048, 4096, 0x6162095671991800},
                                         {3072, 4096, 0x36e551c7dd41f800},
                                         {4096, 4096, 0x23dcbf6fb1991800},
                                         {5120, 4096, 0xee0a5963dd313800},
                                         {6144, 4096, 0x647b2ba7f1991800},
                                         {7168, 4096, 0x39fe74195d41f800},
                                         {8192, 4096, 0xb3de76b931991800},
                                         {9216, 4096, 0x7e0c10ad5d313800},
                                         {10240, 4096, 0xf47ce2f171991800},
                                         {11264, 4096, 0xca002b62dd41f800},
                                         {12288, 4096, 0xb6f7990ab1991800},
                                         {13312, 4096, 0x812532fedd313800},
                                         {14336, 4096, 0xf7960542f1991800},
                                         {15360, 4052, 0x7308a9824acbd5ea}});

  fuchsia::media::playback::SeekingReaderPtr fake_reader_ptr;
  fidl::InterfaceRequest<fuchsia::media::playback::SeekingReader> reader_request =
      fake_reader_ptr.NewRequest();
  fake_reader_.Bind(std::move(reader_request));

  fuchsia::media::playback::SourcePtr source;
  player_->CreateReaderSource(std::move(fake_reader_ptr), source.NewRequest());
  player_->SetSource(std::move(source));

  fake_audio_->renderer().DelayPacketRetirement(15360);

  commands_.Play();
  QuitOnEndOfStream();

  Execute();
  EXPECT_TRUE(fake_audio_->renderer().expected());
}

// Play a synthetic WAV file from beginning to end, retaining packets. This
// tests the ability of the player to handle the case in which the audio
// renderer is holding on to packets for too long.
TEST_F(MediaPlayerTests, PlayWavRetainPackets) {
  fake_audio_->renderer().SetRetainPackets(true);
  fake_audio_->renderer().ExpectPackets({
      {0, 4096, 0x20c39d1e31991800},     {1024, 4096, 0xeaf137125d313800},
      {2048, 4096, 0x6162095671991800},  {3072, 4096, 0x36e551c7dd41f800},
      {4096, 4096, 0x23dcbf6fb1991800},  {5120, 4096, 0xee0a5963dd313800},
      {6144, 4096, 0x647b2ba7f1991800},  {7168, 4096, 0x39fe74195d41f800},
      {8192, 4096, 0xb3de76b931991800},  {9216, 4096, 0x7e0c10ad5d313800},
      {10240, 4096, 0xf47ce2f171991800}, {11264, 4096, 0xca002b62dd41f800},
      {12288, 4096, 0xb6f7990ab1991800}, {13312, 4096, 0x812532fedd313800},
      {14336, 4096, 0xf7960542f1991800}, {15360, 4096, 0x5cdf188f881c7800},
      {16384, 4096, 0x20c39d1e31991800}, {17408, 4096, 0xeaf137125d313800},
      {18432, 4096, 0x6162095671991800}, {19456, 4096, 0x36e551c7dd41f800},
      {20480, 4096, 0x23dcbf6fb1991800}, {21504, 4096, 0xee0a5963dd313800},
      {22528, 4096, 0x647b2ba7f1991800}, {23552, 4096, 0x39fe74195d41f800},
      {24576, 4096, 0xb3de76b931991800}, {25600, 4096, 0x7e0c10ad5d313800},
      {26624, 4096, 0xf47ce2f171991800}, {27648, 4096, 0xca002b62dd41f800},
      {28672, 4096, 0xb6f7990ab1991800}, {29696, 4096, 0x812532fedd313800},
      {30720, 4096, 0xf7960542f1991800}, {31744, 4096, 0x5cdf188f881c7800},
      {32768, 4096, 0x20c39d1e31991800}, {33792, 4096, 0xeaf137125d313800},
      {34816, 4096, 0x6162095671991800}, {35840, 4096, 0x36e551c7dd41f800},
      {36864, 4096, 0x23dcbf6fb1991800}, {37888, 4096, 0xee0a5963dd313800},
      {38912, 4096, 0x647b2ba7f1991800}, {39936, 4096, 0x39fe74195d41f800},
      {40960, 4096, 0xb3de76b931991800}, {41984, 4096, 0x7e0c10ad5d313800},
      {43008, 4096, 0xf47ce2f171991800}, {44032, 4096, 0xca002b62dd41f800},
      {45056, 4096, 0xb6f7990ab1991800}, {46080, 4096, 0x812532fedd313800},
      {47104, 4096, 0xf7960542f1991800}, {48128, 4096, 0x5cdf188f881c7800},
      {49152, 4096, 0x20c39d1e31991800}, {50176, 4096, 0xeaf137125d313800},
      {51200, 4096, 0x6162095671991800}, {52224, 4096, 0x36e551c7dd41f800},
      {53248, 4096, 0x23dcbf6fb1991800}, {54272, 4096, 0xee0a5963dd313800},
      {55296, 4096, 0x647b2ba7f1991800}, {56320, 4096, 0x39fe74195d41f800},
      {57344, 4096, 0xb3de76b931991800}, {58368, 4096, 0x7e0c10ad5d313800},
      {59392, 4096, 0xf47ce2f171991800}, {60416, 4096, 0xca002b62dd41f800},
      {61440, 4096, 0xb6f7990ab1991800}, {62464, 4096, 0x812532fedd313800},
      {63488, 2004, 0xfbff1847deca6dea},
  });

  fuchsia::media::playback::SeekingReaderPtr fake_reader_ptr;
  fidl::InterfaceRequest<fuchsia::media::playback::SeekingReader> reader_request =
      fake_reader_ptr.NewRequest();
  fake_reader_.Bind(std::move(reader_request));

  // Need more than 1s of data.
  fake_reader_.SetSize(256000);

  fuchsia::media::playback::SourcePtr source;
  player_->CreateReaderSource(std::move(fake_reader_ptr), source.NewRequest());
  player_->SetSource(std::move(source));

  commands_.Play();

  // Wait a bit.
  commands_.Sleep(zx::sec(2));

  commands_.Invoke([this]() {
    // We should not be at end-of-stream in spite of waiting long enough, because the pipeline
    // has been stalled by the renderer retaining packets.
    EXPECT_FALSE(commands_.at_end_of_stream());

    // Retire packets.
    fake_audio_->renderer().SetRetainPackets(false);
  });

  QuitOnEndOfStream();

  Execute();
  EXPECT_TRUE(fake_audio_->renderer().expected());
}

// Play a short synthetic WAV file from beginning to end, retaining packets. This
// tests the ability of the player to handle the case in which the audio
// renderer not retiring packets, but all of the audio content fits into the
// payload VMO.
TEST_F(MediaPlayerTests, PlayShortWavRetainPackets) {
  fake_audio_->renderer().SetRetainPackets(true);
  fake_audio_->renderer().ExpectPackets({{0, 4096, 0x20c39d1e31991800},
                                         {1024, 4096, 0xeaf137125d313800},
                                         {2048, 4096, 0x6162095671991800},
                                         {3072, 4096, 0x36e551c7dd41f800},
                                         {4096, 4096, 0x23dcbf6fb1991800},
                                         {5120, 4096, 0xee0a5963dd313800},
                                         {6144, 4096, 0x647b2ba7f1991800},
                                         {7168, 4096, 0x39fe74195d41f800},
                                         {8192, 4096, 0xb3de76b931991800},
                                         {9216, 4096, 0x7e0c10ad5d313800},
                                         {10240, 4096, 0xf47ce2f171991800},
                                         {11264, 4096, 0xca002b62dd41f800},
                                         {12288, 4096, 0xb6f7990ab1991800},
                                         {13312, 4096, 0x812532fedd313800},
                                         {14336, 4096, 0xf7960542f1991800},
                                         {15360, 4052, 0x7308a9824acbd5ea}});

  fuchsia::media::playback::SeekingReaderPtr fake_reader_ptr;
  fidl::InterfaceRequest<fuchsia::media::playback::SeekingReader> reader_request =
      fake_reader_ptr.NewRequest();
  fake_reader_.Bind(std::move(reader_request));

  fuchsia::media::playback::SourcePtr source;
  player_->CreateReaderSource(std::move(fake_reader_ptr), source.NewRequest());
  player_->SetSource(std::move(source));

  commands_.Play();

  // Wait a bit.
  commands_.Sleep(zx::sec(2));

  commands_.Invoke([this]() {
    // We should not be at end-of-stream in spite of waiting long enough, because the pipeline
    // has been stalled by the renderer retaining packets.
    EXPECT_FALSE(commands_.at_end_of_stream());

    // Retire packets.
    fake_audio_->renderer().SetRetainPackets(false);
  });

  QuitOnEndOfStream();

  Execute();
  EXPECT_TRUE(fake_audio_->renderer().expected());
}

// Opens an SBC elementary stream using |ElementarySource|.
TEST_F(MediaPlayerTests, ElementarySourceWithSBC) {
  fuchsia::media::playback::ElementarySourcePtr elementary_source;
  player_->CreateElementarySource(1, false, false, nullptr, elementary_source.NewRequest());

  fuchsia::media::AudioStreamType audio_stream_type;
  audio_stream_type.sample_format = fuchsia::media::AudioSampleFormat::SIGNED_16;
  audio_stream_type.channels = kSamplesPerFrame;
  audio_stream_type.frames_per_second = kFramesPerSecond;
  fuchsia::media::StreamType stream_type;
  stream_type.medium_specific.set_audio(std::move(audio_stream_type));
  stream_type.encoding = fuchsia::media::AUDIO_ENCODING_SBC;

  fuchsia::media::SimpleStreamSinkPtr sink;
  elementary_source->AddStream(std::move(stream_type), kFramesPerSecond, 1, sink.NewRequest());
  sink.set_error_handler([this](zx_status_t status) {
    FX_LOGS(ERROR) << "SimpleStreamSink connection closed.";
    sink_connection_closed_ = true;
    QuitLoop();
  });

  // Here we're upcasting from a
  // |fidl::InterfaceHandle<fuchsia::media::playback::ElementarySource>| to a
  // |fidl::InterfaceHandle<fuchsia::media::playback::Source>| the only way we
  // currently can. The compiler has no way of knowing whether this is
  // legit.
  // TODO(fxbug.dev/7664): Do this safely once fxbug.dev/7664 is fixed.
  player_->SetSource(fidl::InterfaceHandle<fuchsia::media::playback::Source>(
      elementary_source.Unbind().TakeChannel()));

  commands_.WaitForAudioConnected();
  commands_.Invoke([this]() { QuitLoop(); });

  Execute();
  EXPECT_FALSE(sink_connection_closed_);
}

// Opens an AAC elementary stream using |ElementarySource|.
TEST_F(MediaPlayerTests, ElementarySourceWithAAC) {
  fuchsia::media::playback::ElementarySourcePtr elementary_source;
  player_->CreateElementarySource(1, false, false, nullptr, elementary_source.NewRequest());

  fuchsia::media::AudioStreamType audio_stream_type;
  audio_stream_type.sample_format = fuchsia::media::AudioSampleFormat::SIGNED_16;
  audio_stream_type.channels = kSamplesPerFrame;
  audio_stream_type.frames_per_second = kFramesPerSecond;
  fuchsia::media::StreamType stream_type;
  stream_type.medium_specific.set_audio(std::move(audio_stream_type));
  stream_type.encoding = fuchsia::media::AUDIO_ENCODING_AAC;

  fuchsia::media::SimpleStreamSinkPtr sink;
  elementary_source->AddStream(std::move(stream_type), kFramesPerSecond, 1, sink.NewRequest());
  sink.set_error_handler([this](zx_status_t status) {
    FX_LOGS(ERROR) << "SimpleStreamSink connection closed.";
    sink_connection_closed_ = true;
    QuitLoop();
  });

  // Here we're upcasting from a
  // |fidl::InterfaceHandle<fuchsia::media::playback::ElementarySource>| to a
  // |fidl::InterfaceHandle<fuchsia::media::playback::Source>| the only way we
  // currently can. The compiler has no way of knowing whether this is
  // legit.
  // TODO(fxbug.dev/7664): Do this safely once fxbug.dev/7664 is fixed.
  player_->SetSource(fidl::InterfaceHandle<fuchsia::media::playback::Source>(
      elementary_source.Unbind().TakeChannel()));

  commands_.WaitForAudioConnected();
  commands_.Invoke([this]() { QuitLoop(); });

  Execute();
  EXPECT_FALSE(sink_connection_closed_);
}

// Opens an AACLATM elementary stream using |ElementarySource|.
TEST_F(MediaPlayerTests, ElementarySourceWithAACLATM) {
  fuchsia::media::playback::ElementarySourcePtr elementary_source;
  player_->CreateElementarySource(1, false, false, nullptr, elementary_source.NewRequest());

  fuchsia::media::AudioStreamType audio_stream_type;
  audio_stream_type.sample_format = fuchsia::media::AudioSampleFormat::SIGNED_16;
  audio_stream_type.channels = kSamplesPerFrame;
  audio_stream_type.frames_per_second = kFramesPerSecond;
  fuchsia::media::StreamType stream_type;
  stream_type.medium_specific.set_audio(std::move(audio_stream_type));
  stream_type.encoding = fuchsia::media::AUDIO_ENCODING_AACLATM;

  fuchsia::media::SimpleStreamSinkPtr sink;
  elementary_source->AddStream(std::move(stream_type), kFramesPerSecond, 1, sink.NewRequest());
  sink.set_error_handler([this](zx_status_t status) {
    FX_LOGS(ERROR) << "SimpleStreamSink connection closed.";
    sink_connection_closed_ = true;
    QuitLoop();
  });

  // Here we're upcasting from a
  // |fidl::InterfaceHandle<fuchsia::media::playback::ElementarySource>| to a
  // |fidl::InterfaceHandle<fuchsia::media::playback::Source>| the only way we
  // currently can. The compiler has no way of knowing whether this is
  // legit.
  // TODO(fxbug.dev/7664): Do this safely once fxbug.dev/7664 is fixed.
  player_->SetSource(fidl::InterfaceHandle<fuchsia::media::playback::Source>(
      elementary_source.Unbind().TakeChannel()));

  commands_.WaitForAudioConnected();
  commands_.Invoke([this]() { QuitLoop(); });

  Execute();
  EXPECT_FALSE(sink_connection_closed_);
}

// Tries to open a bogus elementary stream using |ElementarySource|.
TEST_F(MediaPlayerTests, ElementarySourceWithBogus) {
  fuchsia::media::playback::ElementarySourcePtr elementary_source;
  player_->CreateElementarySource(1, false, false, nullptr, elementary_source.NewRequest());

  fuchsia::media::AudioStreamType audio_stream_type;
  audio_stream_type.sample_format = fuchsia::media::AudioSampleFormat::SIGNED_16;
  audio_stream_type.channels = kSamplesPerFrame;
  audio_stream_type.frames_per_second = kFramesPerSecond;
  fuchsia::media::StreamType stream_type;
  stream_type.medium_specific.set_audio(std::move(audio_stream_type));
  stream_type.encoding = "bogus encoding";

  fuchsia::media::SimpleStreamSinkPtr sink;
  elementary_source->AddStream(std::move(stream_type), kFramesPerSecond, 1, sink.NewRequest());
  sink.set_error_handler([this](zx_status_t status) {
    FX_LOGS(ERROR) << "SimpleStreamSink connection closed.";
    sink_connection_closed_ = true;
    QuitLoop();
  });

  // Here we're upcasting from a
  // |fidl::InterfaceHandle<fuchsia::media::playback::ElementarySource>| to a
  // |fidl::InterfaceHandle<fuchsia::media::playback::Source>| the only way we
  // currently can. The compiler has no way of knowing whether this is
  // legit.
  // TODO(fxbug.dev/7664): Do this safely once is fixed.
  player_->SetSource(fidl::InterfaceHandle<fuchsia::media::playback::Source>(
      elementary_source.Unbind().TakeChannel()));

  commands_.WaitForProblem();
  commands_.Invoke([this]() { QuitLoop(); });

  Execute();
  EXPECT_FALSE(sink_connection_closed_);
}

// Play a real A/V file from beginning to end.
TEST_F(MediaPlayerTests, PlayBear) {
  fake_sysmem_->SetExpectations(BearSysmemExpectations());

  // ARM64 hashes
  fake_audio_->renderer().ExpectPackets(
      {{1024, 8192, 0xe378bf675ba71490},   {2048, 8192, 0x5f7fc82beb8b4b74},
       {3072, 8192, 0xb25935423814b8a6},   {4096, 8192, 0xc9fb1d58b0bde3f6},
       {5120, 8192, 0xb896f085158b4586},   {6144, 8192, 0x0fd4218f2faef458},
       {7168, 8192, 0x4f8116da4dc6b28a},   {8192, 8192, 0x32091a93269776fc},
       {9216, 8192, 0x4f7e9d36ec1b8b4e},   {10240, 8192, 0xad8ed32d90242ae0},
       {11264, 8192, 0x49f1a268c9cff980},  {12288, 8192, 0x64742cd58ac6c22e},
       {13312, 8192, 0x72079f9f21705496},  {14336, 8192, 0xc323f949fe02e528},
       {15360, 8192, 0xd50113f31abc80bc},  {16384, 8192, 0xa6474b0944bef530},
       {17408, 8192, 0x586ff0bb43d38cf4},  {18432, 8192, 0xb3ab96f86039eef6},
       {19456, 8192, 0x19d0d34a083ee8ec},  {20480, 8192, 0x4e534dbfdfc34710},
       {21504, 8192, 0x8f33738d2347df2a},  {22528, 8192, 0x524c8b4b9429f554},
       {23552, 8192, 0x8215f88c173ccfac},  {24576, 8192, 0x8ecd0819bc5eb5ee},
       {25600, 8192, 0xd452486f6ec7e774},  {26624, 8192, 0x2e5bc5f378b5823c},
       {27648, 8192, 0x34031a02410688fa},  {28672, 8192, 0x350321100c085212},
       {29696, 8192, 0x4fdbcdfeebde00ca},  {30720, 8192, 0x0d054da954b2c35a},
       {31744, 8192, 0x0443303df505c864},  {32768, 8192, 0x3e7706767fc93696},
       {33792, 8192, 0x5f555df0ee0e68ce},  {34816, 8192, 0x40b62bd3de931d94},
       {35840, 8192, 0xd73efaddd2c31ca0},  {36864, 8192, 0x366cfd403b27444a},
       {37888, 8192, 0x6e6e339acbfe94b2},  {38912, 8192, 0x4e4ae0ff82f02fe4},
       {39936, 8192, 0x999d4a3175bb5188},  {40960, 8192, 0x6e61424992d9a268},
       {41984, 8192, 0xc0d6a024162fd1c8},  {43008, 8192, 0x78f3afad01a3e276},
       {44032, 8192, 0x930283e1fb4202d4},  {45056, 8192, 0x2bdd851dcffa4080},
       {46080, 8192, 0xa17900e74b5189ee},  {47104, 8192, 0x89b1e172a13d431c},
       {48128, 8192, 0xcaddea50e4234222},  {49152, 8192, 0xeb263f8ce068c084},
       {50176, 8192, 0xf5648b51c1497ab4},  {51200, 8192, 0xb2c9efb9e61bae6c},
       {52224, 8192, 0x251fd6e581d36824},  {53248, 8192, 0x8f9e5cdc7b9db9d8},
       {54272, 8192, 0x245c4ec91ceec142},  {55296, 8192, 0x943b4f098eb11498},
       {56320, 8192, 0xcd078ed886da307c},  {57344, 8192, 0x4a47778091fb6748},
       {58368, 8192, 0xe10aeb8c4e48c9ba},  {59392, 8192, 0x82feae2dc18a1ca6},
       {60416, 8192, 0x063b518491b66d2c},  {61440, 8192, 0x8893b7d1435d1cd8},
       {62464, 8192, 0xd743f5b4a1cd25ae},  {63488, 8192, 0x0631dacbe0260396},
       {64512, 8192, 0x9871be037107d926},  {65536, 8192, 0xc2fd7f8431296d26},
       {66560, 8192, 0x040faf3e488989b4},  {67584, 8192, 0x4e3899c184dafb9e},
       {68608, 8192, 0x1e172f91690b5a48},  {69632, 8192, 0x74940c17184fb6ea},
       {70656, 8192, 0xea3eb83823ef84a2},  {71680, 8192, 0x8229d6eec16beb9c},
       {72704, 8192, 0xfd40e1dc0acc7c70},  {73728, 8192, 0xa24440f46272b872},
       {74752, 8192, 0xecda392d1658266c},  {75776, 8192, 0xcceb933a91533e82},
       {76800, 8192, 0x83bdf67e40281fde},  {77824, 8192, 0x0f32f74dcf779620},
       {78848, 8192, 0x628facdf5f56d366},  {79872, 8192, 0xd0a4b2b4b633ddc4},
       {80896, 8192, 0xf03f415b49f3f08e},  {81920, 8192, 0x6962cb29ff70dc4a},
       {82944, 8192, 0x4b63b039ce5e6292},  {83968, 8192, 0x98c2eba2dc607ed4},
       {84992, 8192, 0x04680dbb9e52f2de},  {86016, 8192, 0x996730f73524f45a},
       {87040, 8192, 0x2d6e4a03c3f5f36e},  {88064, 8192, 0xf395b48ca422c94a},
       {89088, 8192, 0xc25e28e0b28c0758},  {90112, 8192, 0x0b352780cd68bd96},
       {91136, 8192, 0x9a7ac0d8565922ba},  {92160, 8192, 0x58d325b0b2a7dfc2},
       {93184, 8192, 0x687222156517fede},  {94208, 8192, 0xe0987d30b9229542},
       {95232, 8192, 0xf239835603346e7c},  {96256, 8192, 0xa5bc27db8a5a2e6e},
       {97280, 8192, 0x3240c439f17f7e8c},  {98304, 8192, 0x90c367db9515fd2c},
       {99328, 8192, 0x8076b2ea67ecb4ba},  {100352, 8192, 0x96dd82019e8c75e6},
       {101376, 8192, 0x8a98db93059d690e}, {102400, 8192, 0xc4d6a95d22e80e8c},
       {103424, 8192, 0xce1ec7e9f4813ea0}, {104448, 8192, 0x6bedaa1372b9dab2},
       {105472, 8192, 0xb90e1c646784b7f8}, {106496, 8192, 0x3b6cc09b62bf5e2c},
       {107520, 8192, 0x93049f9eb1ca2040}, {108544, 8192, 0x8aa9d79d3737a300},
       {109568, 8192, 0x0c59abb66b651876}, {110592, 8192, 0xb9903a57092ea688},
       {111616, 8192, 0x56a4a184093d63be}, {112640, 8192, 0x9011ad111b2a3596},
       {113664, 8192, 0xa35992a25b4697e8}, {114688, 8192, 0xf2e6c6600b9192e4},
       {115712, 8192, 0x99139b1eafaac746}, {116736, 8192, 0x0000000000000000},
       {117760, 8192, 0x0000000000000000}, {118784, 8192, 0x0000000000000000},
       {119808, 8192, 0x0000000000000000}, {120832, 8192, 0x0000000000000000}});

  // X64 hashes
  fake_audio_->renderer().ExpectPackets(
      {{1024, 8192, 0xe07048ea42002dc8},   {2048, 8192, 0x56ccb8a6089d573c},
       {3072, 8192, 0x4d1bebb95f7baa6a},   {4096, 8192, 0x8fb71764268f4c7a},
       {5120, 8192, 0x7b33af6ed09ce576},   {6144, 8192, 0x48ed1201b9eefa48},
       {7168, 8192, 0xf7da361cdc44cdfc},   {8192, 8192, 0xff276202afd0d990},
       {9216, 8192, 0x0f2a06a0e713d4de},   {10240, 8192, 0x8d6c214b1c28d0f4},
       {11264, 8192, 0xbbebad03edf0f218},  {12288, 8192, 0xc793371fc6c6b274},
       {13312, 8192, 0x666d565875a0a90a},  {14336, 8192, 0xe873a46b1df9a8ca},
       {15360, 8192, 0xf2a738773576e102},  {16384, 8192, 0x73d97cf32df29b6a},
       {17408, 8192, 0xb8371b8364a3f04a},  {18432, 8192, 0x04758eafbb08dd02},
       {19456, 8192, 0x46e121c7a2949ac0},  {20480, 8192, 0x8e842c69c6a30734},
       {21504, 8192, 0x37f2368ede9ff60a},  {22528, 8192, 0xd922dc101949b5fe},
       {23552, 8192, 0x91924bd83306a0de},  {24576, 8192, 0xfd413297262e5864},
       {25600, 8192, 0x6f0c36406ddada0c},  {26624, 8192, 0xa9b5bb928a964f86},
       {27648, 8192, 0x010bd68d40ac585a},  {28672, 8192, 0x771778993e4cf7e6},
       {29696, 8192, 0xea0cbf478731ab48},  {30720, 8192, 0xa847cc095eca0a52},
       {31744, 8192, 0x4dad81e06b73b5fa},  {32768, 8192, 0x79193e3d6cd3b0d0},
       {33792, 8192, 0xace570c8718bed06},  {34816, 8192, 0x8328c2729e0230ee},
       {35840, 8192, 0x86bf3663fba59b06},  {36864, 8192, 0xa5ee21fe270a754e},
       {37888, 8192, 0x9217963c010a5f6c},  {38912, 8192, 0xf6407a7f99a22c32},
       {39936, 8192, 0xc732eba49f1a2458},  {40960, 8192, 0xcc70dc5bc2bcdefe},
       {41984, 8192, 0x919d2b9244f10b7e},  {43008, 8192, 0x88287ed2dd9ed982},
       {44032, 8192, 0xfcf762913a344dac},  {45056, 8192, 0x6eb78d25c098a8ba},
       {46080, 8192, 0xa4acac73456f2ff4},  {47104, 8192, 0x01163dd1e8def692},
       {48128, 8192, 0x8da60c2fb78bb1ce},  {49152, 8192, 0x57613f21dc6048b8},
       {50176, 8192, 0x7fb9731ad640c646},  {51200, 8192, 0xa3215a4f58b465ca},
       {52224, 8192, 0x4b0c4b346cdc5278},  {53248, 8192, 0xc556925fef0d300a},
       {54272, 8192, 0x8b470fab15c0c680},  {55296, 8192, 0x35be5e27cbfb9dfe},
       {56320, 8192, 0x19705bbb3096003e},  {57344, 8192, 0xa1451c1a7b60c922},
       {58368, 8192, 0x34319151c1f2f84c},  {59392, 8192, 0x4e7e0c52f7f88b74},
       {60416, 8192, 0x52250e7f427de308},  {61440, 8192, 0xc9ce53d293ede012},
       {62464, 8192, 0x98890e49141641c4},  {63488, 8192, 0x6511024a3daa2a88},
       {64512, 8192, 0xad865e490f55b4b0},  {65536, 8192, 0x061e09fd34dad5d8},
       {66560, 8192, 0xdcb56d3f7b922edc},  {67584, 8192, 0x14fde740dfef633a},
       {68608, 8192, 0xa9d874b5c49d6854},  {69632, 8192, 0x90f532855cb593c4},
       {70656, 8192, 0xff341aae71e8cffe},  {71680, 8192, 0xd516442941cc7c6e},
       {72704, 8192, 0x97711ff386fe3cfa},  {73728, 8192, 0x1795c91fd87297e8},
       {74752, 8192, 0x8dbd5969ae4c79a4},  {75776, 8192, 0x5913acc40119b706},
       {76800, 8192, 0xb25d01d4a4f66804},  {77824, 8192, 0xc8b2c623249734b8},
       {78848, 8192, 0x1296a2693d5c8b66},  {79872, 8192, 0xb5605c5877374dc8},
       {80896, 8192, 0x0d4a70097114e2de},  {81920, 8192, 0xba9d5b2368c16b1c},
       {82944, 8192, 0x3885818f4c03f1e8},  {83968, 8192, 0x6f991203b29e99d8},
       {84992, 8192, 0xcdaa5a21f84d9ddc},  {86016, 8192, 0x2c9090bddb1f302a},
       {87040, 8192, 0x00046a36299086c2},  {88064, 8192, 0xd878df9a7d04f554},
       {89088, 8192, 0x43c288db8ea0c1d0},  {90112, 8192, 0x5d831fc01b30762e},
       {91136, 8192, 0xac2a7b67273a81fc},  {92160, 8192, 0xe21966c79303a938},
       {93184, 8192, 0x59d837f4bcebfd02},  {94208, 8192, 0x814e88b91b1229a4},
       {95232, 8192, 0x9f1a78beeb4fa414},  {96256, 8192, 0x7f8ff018d9cc9720},
       {97280, 8192, 0xcb4b129681b91b2a},  {98304, 8192, 0xbb3d48aa3f62d486},
       {99328, 8192, 0xaa9d642ab0856c4e},  {100352, 8192, 0x8b179bc7c2323d7a},
       {101376, 8192, 0x06c73fa3037af4a8}, {102400, 8192, 0xcb8b533f4a2640d2},
       {103424, 8192, 0x177825150ba16718}, {104448, 8192, 0x62749ca362394b14},
       {105472, 8192, 0x57247528ac244288}, {106496, 8192, 0x74a451316a5e9f4c},
       {107520, 8192, 0xfa863a3072b86888}, {108544, 8192, 0xf7d71018bc978038},
       {109568, 8192, 0xdabc1a437f54a878}, {110592, 8192, 0x90e9f43b9b00ce94},
       {111616, 8192, 0x05695a101ce2691c}, {112640, 8192, 0xd57dd116aec078fc},
       {113664, 8192, 0x067388568736b478}, {114688, 8192, 0x69f5ec70dbc7478e},
       {115712, 8192, 0xbd9c0421959f25a6}, {116736, 8192, 0x0000000000000000},
       {117760, 8192, 0x0000000000000000}, {118784, 8192, 0x0000000000000000},
       {119808, 8192, 0x0000000000000000}, {120832, 8192, 0x0000000000000000}});

  fake_scenic_->session().SetExpectations(
      1,
      {
          .pixel_format = {.type = fuchsia::sysmem::PixelFormatType::R8G8B8A8},
          .coded_width = 2,
          .coded_height = 2,
          .bytes_per_row = 8,
          .display_width = 2,
          .display_height = 2,
          .color_space = {.type = fuchsia::sysmem::ColorSpaceType::SRGB},
          .has_pixel_aspect_ratio = true,
      },
      {
          .pixel_format = {.type = fuchsia::sysmem::PixelFormatType::I420},
          .coded_width = 1280,
          .coded_height = 738,
          .bytes_per_row = 1280,
          .display_width = 1280,
          .display_height = 720,
          .color_space = {.type = fuchsia::sysmem::ColorSpaceType::REC709},
          .has_pixel_aspect_ratio = true,
      },
      {
          {0, 944640, 0xe22305b43e20ba47},          {146479375, 944640, 0x66ae7cd1ab593c8e},
          {179846042, 944640, 0x8893faaea28f39bc},  {213212708, 944640, 0x88508b0a34efffad},
          {246579375, 944640, 0x3a63c81772b70383},  {279946042, 944640, 0x3780c4550621ebe0},
          {313312708, 944640, 0x4f921c4320a6417f},  {346679375, 944640, 0x4e9a21647e4929be},
          {380046042, 944640, 0xe7e665c795955c15},  {413412708, 944640, 0x3c3aedc1d6683aa4},
          {446779375, 944640, 0xfe9e286a635fb73d},  {480146042, 944640, 0x47e6f4f1abff1b7e},
          {513512708, 944640, 0x84f562dcd46197a5},  {546879375, 944640, 0xf38b34e69d27cbc9},
          {580246042, 944640, 0xee2998da3599b399},  {613612708, 944640, 0x524da51958ef48d3},
          {646979375, 944640, 0x062586602fe0a479},  {680346042, 944640, 0xc32d430e92ae479c},
          {713712708, 944640, 0x3dff5398e416dc2b},  {747079375, 944640, 0xd3c76266c63bd4c3},
          {780446042, 944640, 0xc3241587b5491999},  {813812708, 944640, 0xfd3abe1fbe877da2},
          {847179375, 944640, 0x1a3bd139a0f8460b},  {880546042, 944640, 0x11f585d7e68bda67},
          {913912708, 944640, 0xecd344c5043e29ae},  {947279375, 944640, 0x7ae6b259c3b7f093},
          {980646042, 944640, 0x5d49bfa6c196c9d1},  {1014012708, 944640, 0xe83a44b02cac86f6},
          {1047379375, 944640, 0xffad44c6d3f60005}, {1080746042, 944640, 0x85d1372b40b214c4},
          {1114112708, 944640, 0x9b6f88950ead9041}, {1147479375, 944640, 0x1396882cb6f522a1},
          {1180846042, 944640, 0x07815d4ef90b1507}, {1214212708, 944640, 0x424879e928edc717},
          {1247579375, 944640, 0xd623f27e3773245f}, {1280946042, 944640, 0x47581df2a2e350ff},
          {1314312708, 944640, 0xb836a1cbbae59a31}, {1347679375, 944640, 0xe6d7ce3f416411ea},
          {1381046042, 944640, 0x1c5dba765b2b85f3}, {1414412708, 944640, 0x85987a43defb3ead},
          {1447779375, 944640, 0xe66b3d70ca2358db}, {1481146042, 944640, 0x2b7e765a1f2245de},
          {1514512708, 944640, 0x9e79fedce712de01}, {1547879375, 944640, 0x7ad7078f8731e4f0},
          {1581246042, 944640, 0x91ac3c20c4d4e497}, {1614612708, 944640, 0xdb7c8209e5b3a2f4},
          {1647979375, 944640, 0xd47a9314da3ddec9}, {1681346042, 944640, 0x00c1c1f8e8570386},
          {1714712708, 944640, 0x1b603a5644b00e7f}, {1748079375, 944640, 0x15c18419b83f5a54},
          {1781446042, 944640, 0x0038ff1808d201c7}, {1814812708, 944640, 0xe7b2592675d2002a},
          {1848179375, 944640, 0x55ef9a4ba7570494}, {1881546042, 944640, 0x14b6c92ae0fde6a9},
          {1914912708, 944640, 0x3f05f2378c5d06c2}, {1948279375, 944640, 0x04f246ec6c3f0ab9},
          {1981646042, 944640, 0x829529ce2d0a95cd}, {2015012708, 944640, 0xc0eee6a564624169},
          {2048379375, 944640, 0xdd31903bdc9c909f}, {2081746042, 944640, 0x989727e8fcd13cca},
          {2115112708, 944640, 0x9e6b6fe9d1b02649}, {2148479375, 944640, 0x01cfc5a96079d823},
          {2181846042, 944640, 0x90ee949821bfed16}, {2215212708, 944640, 0xf6e66a48b2c977cc},
          {2248579375, 944640, 0xb5a1d79f1401e1a6}, {2281946042, 944640, 0x89e8ca8aa0b24bef},
          {2315312708, 944640, 0xd7e384493250e13b}, {2348679375, 944640, 0x7c042bbc365297eb},
          {2382046042, 944640, 0xfaf92184251ecbf4}, {2415412708, 944640, 0x0edcf5f479f9ec39},
          {2448779375, 944640, 0x59c165487d90fbb3}, {2482146042, 944640, 0xd4fbf15095e6b728},
          {2515512708, 944640, 0x6a05e676671df8e1}, {2548879375, 944640, 0x44d653ed72393e1c},
          {2582246042, 944640, 0x912f720f4c904527}, {2615612708, 944640, 0xe4ca7bc6919d1e70},
          {2648979375, 944640, 0x6cde61420e173a62}, {2682346042, 944640, 0xfe0d7d86d0b57044},
          {2715712708, 944640, 0x2d96bc09b6303a4b}, {2749079375, 944640, 0x2cdaab788c93a466},
          {2782446042, 944640, 0x979b90a096e76dbb}, {2815812708, 944640, 0x851ccb01ea035f4e},
      });

  CreateView();
  commands_.SetFile(kBearFilePath);
  commands_.Play();
  QuitOnEndOfStream();

  Execute();
  EXPECT_TRUE(fake_audio_->create_audio_renderer_called());
  EXPECT_TRUE(fake_audio_->renderer().expected());
  EXPECT_TRUE(fake_scenic_->session().expected());
  EXPECT_TRUE(fake_sysmem_->expected());
}

// Play a real A/V file from beginning to end with no audio.
TEST_F(MediaPlayerTests, PlayBearSilent) {
  fake_sysmem_->SetExpectations(BearSysmemExpectations());

  fake_scenic_->session().SetExpectations(
      1,
      {
          .pixel_format = {.type = fuchsia::sysmem::PixelFormatType::R8G8B8A8},
          .coded_width = 2,
          .coded_height = 2,
          .bytes_per_row = 8,
          .display_width = 2,
          .display_height = 2,
          .color_space = {.type = fuchsia::sysmem::ColorSpaceType::SRGB},
          .has_pixel_aspect_ratio = true,
      },
      {
          .pixel_format = {.type = fuchsia::sysmem::PixelFormatType::I420},
          .coded_width = 1280,
          .coded_height = 738,
          .bytes_per_row = 1280,
          .display_width = 1280,
          .display_height = 720,
          .color_space = {.type = fuchsia::sysmem::ColorSpaceType::REC709},
          .has_pixel_aspect_ratio = true,
      },
      {
          {0, 944640, 0xe22305b43e20ba47},          {146479375, 944640, 0x66ae7cd1ab593c8e},
          {179846042, 944640, 0x8893faaea28f39bc},  {213212708, 944640, 0x88508b0a34efffad},
          {246579375, 944640, 0x3a63c81772b70383},  {279946042, 944640, 0x3780c4550621ebe0},
          {313312708, 944640, 0x4f921c4320a6417f},  {346679375, 944640, 0x4e9a21647e4929be},
          {380046042, 944640, 0xe7e665c795955c15},  {413412708, 944640, 0x3c3aedc1d6683aa4},
          {446779375, 944640, 0xfe9e286a635fb73d},  {480146042, 944640, 0x47e6f4f1abff1b7e},
          {513512708, 944640, 0x84f562dcd46197a5},  {546879375, 944640, 0xf38b34e69d27cbc9},
          {580246042, 944640, 0xee2998da3599b399},  {613612708, 944640, 0x524da51958ef48d3},
          {646979375, 944640, 0x062586602fe0a479},  {680346042, 944640, 0xc32d430e92ae479c},
          {713712708, 944640, 0x3dff5398e416dc2b},  {747079375, 944640, 0xd3c76266c63bd4c3},
          {780446042, 944640, 0xc3241587b5491999},  {813812708, 944640, 0xfd3abe1fbe877da2},
          {847179375, 944640, 0x1a3bd139a0f8460b},  {880546042, 944640, 0x11f585d7e68bda67},
          {913912708, 944640, 0xecd344c5043e29ae},  {947279375, 944640, 0x7ae6b259c3b7f093},
          {980646042, 944640, 0x5d49bfa6c196c9d1},  {1014012708, 944640, 0xe83a44b02cac86f6},
          {1047379375, 944640, 0xffad44c6d3f60005}, {1080746042, 944640, 0x85d1372b40b214c4},
          {1114112708, 944640, 0x9b6f88950ead9041}, {1147479375, 944640, 0x1396882cb6f522a1},
          {1180846042, 944640, 0x07815d4ef90b1507}, {1214212708, 944640, 0x424879e928edc717},
          {1247579375, 944640, 0xd623f27e3773245f}, {1280946042, 944640, 0x47581df2a2e350ff},
          {1314312708, 944640, 0xb836a1cbbae59a31}, {1347679375, 944640, 0xe6d7ce3f416411ea},
          {1381046042, 944640, 0x1c5dba765b2b85f3}, {1414412708, 944640, 0x85987a43defb3ead},
          {1447779375, 944640, 0xe66b3d70ca2358db}, {1481146042, 944640, 0x2b7e765a1f2245de},
          {1514512708, 944640, 0x9e79fedce712de01}, {1547879375, 944640, 0x7ad7078f8731e4f0},
          {1581246042, 944640, 0x91ac3c20c4d4e497}, {1614612708, 944640, 0xdb7c8209e5b3a2f4},
          {1647979375, 944640, 0xd47a9314da3ddec9}, {1681346042, 944640, 0x00c1c1f8e8570386},
          {1714712708, 944640, 0x1b603a5644b00e7f}, {1748079375, 944640, 0x15c18419b83f5a54},
          {1781446042, 944640, 0x0038ff1808d201c7}, {1814812708, 944640, 0xe7b2592675d2002a},
          {1848179375, 944640, 0x55ef9a4ba7570494}, {1881546042, 944640, 0x14b6c92ae0fde6a9},
          {1914912708, 944640, 0x3f05f2378c5d06c2}, {1948279375, 944640, 0x04f246ec6c3f0ab9},
          {1981646042, 944640, 0x829529ce2d0a95cd}, {2015012708, 944640, 0xc0eee6a564624169},
          {2048379375, 944640, 0xdd31903bdc9c909f}, {2081746042, 944640, 0x989727e8fcd13cca},
          {2115112708, 944640, 0x9e6b6fe9d1b02649}, {2148479375, 944640, 0x01cfc5a96079d823},
          {2181846042, 944640, 0x90ee949821bfed16}, {2215212708, 944640, 0xf6e66a48b2c977cc},
          {2248579375, 944640, 0xb5a1d79f1401e1a6}, {2281946042, 944640, 0x89e8ca8aa0b24bef},
          {2315312708, 944640, 0xd7e384493250e13b}, {2348679375, 944640, 0x7c042bbc365297eb},
          {2382046042, 944640, 0xfaf92184251ecbf4}, {2415412708, 944640, 0x0edcf5f479f9ec39},
          {2448779375, 944640, 0x59c165487d90fbb3}, {2482146042, 944640, 0xd4fbf15095e6b728},
          {2515512708, 944640, 0x6a05e676671df8e1}, {2548879375, 944640, 0x44d653ed72393e1c},
          {2582246042, 944640, 0x912f720f4c904527}, {2615612708, 944640, 0xe4ca7bc6919d1e70},
          {2648979375, 944640, 0x6cde61420e173a62}, {2682346042, 944640, 0xfe0d7d86d0b57044},
          {2715712708, 944640, 0x2d96bc09b6303a4b}, {2749079375, 944640, 0x2cdaab788c93a466},
          {2782446042, 944640, 0x979b90a096e76dbb}, {2815812708, 944640, 0x851ccb01ea035f4e},
      });

  CreateView();
  commands_.SetFile(kBearFilePath, true);  // true -> silent
  commands_.Play();
  QuitOnEndOfStream();

  Execute();
  EXPECT_FALSE(fake_audio_->create_audio_renderer_called());
  EXPECT_TRUE(fake_scenic_->session().expected());
  EXPECT_TRUE(fake_sysmem_->expected());
}

// Play an opus file from beginning to end.
TEST_F(MediaPlayerTests, PlayOpus) {
  // The decoder works a bit differently on x64 vs arm64, hence the two lists here.
  fake_audio_->renderer().ExpectPackets({{-336, 1296, 0x47ff30edd64831d6},
                                         {312, 1920, 0xcc4016bbb348e52b},
                                         {1272, 1920, 0xe54a89514c636028},
                                         {2232, 1920, 0x8ef31ce86009d7da},
                                         {3192, 1920, 0x36490fe70ca3bb81},
                                         {4152, 1920, 0x4a8bdd8e9c2f42bb},
                                         {5112, 1920, 0xbc8cea1839f0299e},
                                         {6072, 1920, 0x868a68451d7ab814},
                                         {7032, 1920, 0x84ac9b11a685a9a9},
                                         {7992, 1920, 0xe4359c110afe8adb},
                                         {8952, 1920, 0x2092c7fbf2ff0f0c},
                                         {9912, 1920, 0x8002d77665736d63},
                                         {10872, 1920, 0x541b415fbdc7b268},
                                         {11832, 1920, 0xe81ef757a5953573},
                                         {12792, 1920, 0xbc70aba0ed44f7dc}});
  fake_audio_->renderer().ExpectPackets({{-336, 1296, 0xbf1f56243e245a2c},
                                         {312, 1920, 0x670e69ee3076c4b2},
                                         {1272, 1920, 0xe0667e312e65207d},
                                         {2232, 1920, 0x291ffa6baf5dd2b1},
                                         {3192, 1920, 0x1b408d840e27bcc1},
                                         {4152, 1920, 0xdbf5034a75bc761b},
                                         {5112, 1920, 0x46fa968eb705415b},
                                         {6072, 1920, 0x9f47ee9cbb3c814c},
                                         {7032, 1920, 0x7256d4c58d7afe56},
                                         {7992, 1920, 0xb2a7bc50ce80c898},
                                         {8952, 1920, 0xb314415fd9c3a694},
                                         {9912, 1920, 0x34d9ce067ffacc37},
                                         {10872, 1920, 0x661cc8ec834fb30a},
                                         {11832, 1920, 0x05fd64442f53c5cc},
                                         {12792, 1920, 0x3e2a98426c8680d0}});

  commands_.SetFile(kOpusFilePath);
  commands_.Play();
  QuitOnEndOfStream();

  Execute();
  EXPECT_TRUE(fake_audio_->renderer().expected());
}

// Play a real A/V file from beginning to end, retaining audio packets. This
// tests the ability of the player to handle the case in which the audio
// renderer is holding on to packets for too long.
TEST_F(MediaPlayerTests, PlayBearRetainAudioPackets) {
  fake_sysmem_->SetExpectations(BearSysmemExpectations());

  CreateView();
  fake_audio_->renderer().SetRetainPackets(true);

  commands_.SetFile(kBearFilePath);
  commands_.Play();

  // Wait a bit.
  commands_.Sleep(zx::sec(2));

  commands_.Invoke([this]() {
    // We should not be at end-of-stream in spite of waiting long enough, because the pipeline
    // has been stalled by the renderer retaining packets.
    EXPECT_FALSE(commands_.at_end_of_stream());

    // Retire packets.
    fake_audio_->renderer().SetRetainPackets(false);
  });

  QuitOnEndOfStream();

  Execute();
  EXPECT_TRUE(fake_audio_->renderer().expected());
  EXPECT_TRUE(fake_scenic_->session().expected());
  EXPECT_TRUE(fake_sysmem_->expected());
}

// Regression test for fxbug.dev/28417.
TEST_F(MediaPlayerTests, RegressionTestUS544) {
  fake_sysmem_->SetExpectations(BearSysmemExpectations());

  CreateView();
  commands_.SetFile(kBearFilePath);

  // Play for two seconds and pause.
  commands_.Play();
  commands_.WaitForPosition(zx::sec(2));
  commands_.Pause();

  // Wait a bit.
  commands_.Sleep(zx::sec(2));

  // Seek to the beginning and resume playing.
  commands_.Seek(zx::sec(0));
  commands_.Play();

  QuitOnEndOfStream();

  Execute();
  EXPECT_TRUE(fake_audio_->renderer().expected());
  EXPECT_TRUE(fake_scenic_->session().expected());
  EXPECT_TRUE(fake_sysmem_->expected());
}

// Regression test for QA-539.
// Verifies that the player can play two files in a row.
TEST_F(MediaPlayerTests, RegressionTestQA539) {
  fake_sysmem_->SetExpectations(BearSysmemExpectations());

  CreateView();
  commands_.SetFile(kBearFilePath);

  // Play the file to the end.
  commands_.Play();
  commands_.WaitForEndOfStream();

  // Reload the file.
  commands_.SetFile(kBearFilePath);

  commands_.Play();
  QuitOnEndOfStream();

  Execute();
  EXPECT_TRUE(fake_audio_->renderer().expected());
  EXPECT_TRUE(fake_scenic_->session().expected());
  EXPECT_TRUE(fake_sysmem_->expected());
}

// Play an LPCM elementary stream using |ElementarySource|. We delay calling SetSource to ensure
// that the SimpleStreamSink defers taking any action until it's properly connected.
TEST_F(MediaPlayerTests, ElementarySourceDeferred) {
  fake_audio_->renderer().ExpectPackets({{0, 4096, 0xd2fbd957e3bf0000},
                                         {1024, 4096, 0xda25db3fa3bf0000},
                                         {2048, 4096, 0xe227e0f6e3bf0000},
                                         {3072, 4096, 0xe951e2dea3bf0000},
                                         {4096, 4096, 0x37ebf7d3e3bf0000},
                                         {5120, 4096, 0x3f15f9bba3bf0000},
                                         {6144, 4096, 0x4717ff72e3bf0000},
                                         {7168, 4096, 0x4e42015aa3bf0000},
                                         {8192, 4096, 0xeabc5347e3bf0000},
                                         {9216, 4096, 0xf1e6552fa3bf0000},
                                         {10240, 4096, 0xf9e85ae6e3bf0000},
                                         {11264, 4096, 0x01125ccea3bf0000},
                                         {12288, 4096, 0x4fac71c3e3bf0000},
                                         {13312, 4096, 0x56d673aba3bf0000},
                                         {14336, 4096, 0x5ed87962e3bf0000},
                                         {15360, 4096, 0x66027b4aa3bf0000}});

  fuchsia::media::playback::ElementarySourcePtr elementary_source;
  player_->CreateElementarySource(0, false, false, nullptr, elementary_source.NewRequest());

  fuchsia::media::AudioStreamType audio_stream_type;
  audio_stream_type.sample_format = fuchsia::media::AudioSampleFormat::SIGNED_16;
  audio_stream_type.channels = kSamplesPerFrame;
  audio_stream_type.frames_per_second = kFramesPerSecond;
  fuchsia::media::StreamType stream_type;
  stream_type.medium_specific.set_audio(std::move(audio_stream_type));
  stream_type.encoding = fuchsia::media::AUDIO_ENCODING_LPCM;

  fuchsia::media::SimpleStreamSinkPtr sink;
  elementary_source->AddStream(std::move(stream_type), kFramesPerSecond, 1, sink.NewRequest());
  sink.set_error_handler([this](zx_status_t status) {
    FX_LOGS(ERROR) << "SimpleStreamSink connection closed.";
    sink_connection_closed_ = true;
    QuitLoop();
  });

  sink_feeder_.Init(std::move(sink), kSinkFeedSize, kSamplesPerFrame * sizeof(int16_t),
                    kSinkFeedMaxPacketSize, kSinkFeedMaxPacketCount);

  // Here we're upcasting from a
  // |fidl::InterfaceHandle<fuchsia::media::playback::ElementarySource>| to a
  // |fidl::InterfaceHandle<fuchsia::media::playback::Source>| the only way we
  // currently can. The compiler has no way of knowing whether this is
  // legit.
  // TODO(dalesat): Do this safely once fxbug.dev/7664 is fixed.
  player_->SetSource(fidl::InterfaceHandle<fuchsia::media::playback::Source>(
      elementary_source.Unbind().TakeChannel()));

  commands_.Play();
  QuitOnEndOfStream();

  Execute();
  EXPECT_TRUE(fake_audio_->renderer().expected());
  EXPECT_FALSE(sink_connection_closed_);
}

// Play a real A/V file from beginning to end at rate 2.0.
TEST_F(MediaPlayerTests, PlayBear2) {
  fake_sysmem_->SetExpectations(BearSysmemExpectations());

  // Only a few packets will be seen by audio renderer, and none of them would be actually rendered
  // since Play won't be called due to 2.0 being an unsupported rate.

  // ARM64 hashes
  fake_audio_->renderer().ExpectPackets({{1024, 8192, 0xe378bf675ba71490},
                                         {2048, 8192, 0x5f7fc82beb8b4b74},
                                         {3072, 8192, 0xb25935423814b8a6},
                                         {4096, 8192, 0xc9fb1d58b0bde3f6},
                                         {5120, 8192, 0xb896f085158b4586}});

  // X64 hashes
  fake_audio_->renderer().ExpectPackets({{1024, 8192, 0xe07048ea42002dc8},
                                         {2048, 8192, 0x56ccb8a6089d573c},
                                         {3072, 8192, 0x4d1bebb95f7baa6a},
                                         {4096, 8192, 0x8fb71764268f4c7a},
                                         {5120, 8192, 0x7b33af6ed09ce576}});

  fake_scenic_->session().SetExpectations(
      1,
      {
          .pixel_format = {.type = fuchsia::sysmem::PixelFormatType::R8G8B8A8},
          .coded_width = 2,
          .coded_height = 2,
          .bytes_per_row = 8,
          .display_width = 2,
          .display_height = 2,
          .color_space = {.type = fuchsia::sysmem::ColorSpaceType::SRGB},
          .has_pixel_aspect_ratio = true,
      },
      {
          .pixel_format = {.type = fuchsia::sysmem::PixelFormatType::I420},
          .coded_width = 1280,
          .coded_height = 738,
          .bytes_per_row = 1280,
          .display_width = 1280,
          .display_height = 720,
          .color_space = {.type = fuchsia::sysmem::ColorSpaceType::REC709},
          .has_pixel_aspect_ratio = true,
      },
      {
          {0, 944640, 0xe22305b43e20ba47},          {112751833, 944640, 0x66ae7cd1ab593c8e},
          {129435167, 944640, 0x8893faaea28f39bc},  {146118500, 944640, 0x88508b0a34efffad},
          {162801833, 944640, 0x3a63c81772b70383},  {179485167, 944640, 0x3780c4550621ebe0},
          {196168500, 944640, 0x4f921c4320a6417f},  {212851833, 944640, 0x4e9a21647e4929be},
          {229535167, 944640, 0xe7e665c795955c15},  {246218500, 944640, 0x3c3aedc1d6683aa4},
          {262901833, 944640, 0xfe9e286a635fb73d},  {279585167, 944640, 0x47e6f4f1abff1b7e},
          {296268500, 944640, 0x84f562dcd46197a5},  {312951833, 944640, 0xf38b34e69d27cbc9},
          {329635167, 944640, 0xee2998da3599b399},  {346318500, 944640, 0x524da51958ef48d3},
          {363001833, 944640, 0x062586602fe0a479},  {379685167, 944640, 0xc32d430e92ae479c},
          {396368500, 944640, 0x3dff5398e416dc2b},  {413051833, 944640, 0xd3c76266c63bd4c3},
          {429735167, 944640, 0xc3241587b5491999},  {446418500, 944640, 0xfd3abe1fbe877da2},
          {463101833, 944640, 0x1a3bd139a0f8460b},  {479785167, 944640, 0x11f585d7e68bda67},
          {496468500, 944640, 0xecd344c5043e29ae},  {513151833, 944640, 0x7ae6b259c3b7f093},
          {529835167, 944640, 0x5d49bfa6c196c9d1},  {546518500, 944640, 0xe83a44b02cac86f6},
          {563201833, 944640, 0xffad44c6d3f60005},  {579885167, 944640, 0x85d1372b40b214c4},
          {596568500, 944640, 0x9b6f88950ead9041},  {613251833, 944640, 0x1396882cb6f522a1},
          {629935167, 944640, 0x07815d4ef90b1507},  {646618500, 944640, 0x424879e928edc717},
          {663301833, 944640, 0xd623f27e3773245f},  {679985167, 944640, 0x47581df2a2e350ff},
          {696668500, 944640, 0xb836a1cbbae59a31},  {713351833, 944640, 0xe6d7ce3f416411ea},
          {730035167, 944640, 0x1c5dba765b2b85f3},  {746718500, 944640, 0x85987a43defb3ead},
          {763401833, 944640, 0xe66b3d70ca2358db},  {780085167, 944640, 0x2b7e765a1f2245de},
          {796768500, 944640, 0x9e79fedce712de01},  {813451833, 944640, 0x7ad7078f8731e4f0},
          {830135167, 944640, 0x91ac3c20c4d4e497},  {846818500, 944640, 0xdb7c8209e5b3a2f4},
          {863501833, 944640, 0xd47a9314da3ddec9},  {880185167, 944640, 0x00c1c1f8e8570386},
          {896868500, 944640, 0x1b603a5644b00e7f},  {913551833, 944640, 0x15c18419b83f5a54},
          {930235167, 944640, 0x0038ff1808d201c7},  {946918500, 944640, 0xe7b2592675d2002a},
          {963601833, 944640, 0x55ef9a4ba7570494},  {980285167, 944640, 0x14b6c92ae0fde6a9},
          {996968500, 944640, 0x3f05f2378c5d06c2},  {1013651833, 944640, 0x04f246ec6c3f0ab9},
          {1030335167, 944640, 0x829529ce2d0a95cd}, {1047018500, 944640, 0xc0eee6a564624169},
          {1063701833, 944640, 0xdd31903bdc9c909f}, {1080385167, 944640, 0x989727e8fcd13cca},
          {1097068500, 944640, 0x9e6b6fe9d1b02649}, {1113751833, 944640, 0x01cfc5a96079d823},
          {1130435167, 944640, 0x90ee949821bfed16}, {1147118500, 944640, 0xf6e66a48b2c977cc},
          {1163801833, 944640, 0xb5a1d79f1401e1a6}, {1180485167, 944640, 0x89e8ca8aa0b24bef},
          {1197168500, 944640, 0xd7e384493250e13b}, {1213851833, 944640, 0x7c042bbc365297eb},
          {1230535167, 944640, 0xfaf92184251ecbf4}, {1247218500, 944640, 0x0edcf5f479f9ec39},
          {1263901833, 944640, 0x59c165487d90fbb3}, {1280585167, 944640, 0xd4fbf15095e6b728},
          {1297268500, 944640, 0x6a05e676671df8e1}, {1313951833, 944640, 0x44d653ed72393e1c},
          {1330635167, 944640, 0x912f720f4c904527}, {1347318500, 944640, 0xe4ca7bc6919d1e70},
          {1364001833, 944640, 0x6cde61420e173a62}, {1380685167, 944640, 0xfe0d7d86d0b57044},
          {1397368500, 944640, 0x2d96bc09b6303a4b}, {1414051833, 944640, 0x2cdaab788c93a466},
          {1430735167, 944640, 0x979b90a096e76dbb}, {1447418500, 944640, 0x851ccb01ea035f4e},
      });

  CreateView();
  commands_.SetFile(kBearFilePath);
  commands_.SetPlaybackRate(2.0);
  commands_.Play();
  QuitOnEndOfStream();

  Execute();
  EXPECT_TRUE(fake_audio_->renderer().expected());
  EXPECT_TRUE(fake_scenic_->session().expected());
  EXPECT_TRUE(fake_sysmem_->expected());
}

}  // namespace test
}  // namespace media_player
