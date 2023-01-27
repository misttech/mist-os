// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <lib/fidl/cpp/enum.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>

#include <string>

#include <gtest/gtest.h>

#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/control_server.h"

namespace media_audio {
namespace {

using Control = fuchsia_audio_device::Control;

class ControlServerWarningTest : public AudioDeviceRegistryServerTestBase,
                                 public fidl::AsyncEventHandler<fuchsia_audio_device::Control>,
                                 public fidl::AsyncEventHandler<fuchsia_audio_device::RingBuffer> {
 protected:
  std::unique_ptr<FakeAudioDriver> CreateAndEnableDriverWithDefaults() {
    EXPECT_EQ(dispatcher(), test_loop().dispatcher());
    zx::channel server_end, client_end;
    EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
    auto fake_driver = std::make_unique<FakeAudioDriver>(std::move(server_end),
                                                         std::move(client_end), dispatcher());

    adr_service_->AddDevice(Device::Create(
        adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
        fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }

  std::optional<TokenId> WaitForAddedDeviceTokenId(
      fidl::Client<fuchsia_audio_device::Registry>& registry_client) {
    std::optional<TokenId> added_device_id;
    registry_client->WatchDevicesAdded().Then(
        [&added_device_id](
            fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
          ASSERT_TRUE(result.is_ok());
          ASSERT_TRUE(result->devices());
          ASSERT_EQ(result->devices()->size(), 1u);
          ASSERT_TRUE(result->devices()->at(0).token_id());
          added_device_id = *result->devices()->at(0).token_id();
        });
    RunLoopUntilIdle();
    return added_device_id;
  }

  // Obtain a control via ControlCreator/Create (not the synthetic CreateTestControlServer method).
  fidl::Client<fuchsia_audio_device::Control> ConnectToControl(
      fidl::Client<fuchsia_audio_device::ControlCreator>& control_creator_client,
      TokenId token_id) {
    auto [control_client_end, control_server_end] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Control>();
    auto control_client = fidl::Client<fuchsia_audio_device::Control>(
        fidl::ClientEnd<fuchsia_audio_device::Control>(std::move(control_client_end)), dispatcher(),
        control_fidl_handler_.get());
    bool received_callback = false;
    control_creator_client
        ->Create({{
            .token_id = token_id,
            .control_server =
                fidl::ServerEnd<fuchsia_audio_device::Control>(std::move(control_server_end)),
        }})
        .Then([&received_callback](
                  fidl::Result<fuchsia_audio_device::ControlCreator::Create>& result) {
          ASSERT_TRUE(result.is_ok());
          received_callback = true;
        });
    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
    EXPECT_TRUE(control_client.is_valid());
    return control_client;
  }

  void TestSetGainBadState(const std::optional<fuchsia_audio_device::GainState>& bad_state,
                           fuchsia_audio_device::ControlSetGainError expected_error);
  void TestCreateRingBufferBadOptions(
      const std::optional<fuchsia_audio_device::RingBufferOptions>& bad_options,
      fuchsia_audio_device::ControlCreateRingBufferError expected_error);
};

// Device error causes ControlNotify->DeviceHasError
// Driver drops StreamConfig causes ControlNotify->DeviceIsRemoved
// Client closes RingBuffer does NOT cause ControlNotify->DeviceIsRemoved or DeviceHasError
// Driver drops RingBuffer does NOT cause ControlNotify->DeviceIsRemoved or DeviceHasError
void ControlServerWarningTest::TestSetGainBadState(
    const std::optional<fuchsia_audio_device::GainState>& bad_state,
    fuchsia_audio_device::ControlSetGainError expected_error) {
  EXPECT_EQ(dispatcher(), test_loop().dispatcher());
  zx::channel server_end, client_end;
  EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
  auto fake_driver =
      std::make_unique<FakeAudioDriver>(std::move(server_end), std::move(client_end), dispatcher());
  fake_driver->set_can_mute(false);
  fake_driver->set_can_agc(false);
  fake_driver->AllocateRingBuffer(8192);
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();

  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(ControlServer::count(), 1u);
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
      fidl::ClientEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_client_end)),
      dispatcher(), ring_buffer_fidl_handler_.get());
  bool received_callback = false;

  control_client
      ->SetGain({{
          .target_state = bad_state,
      }})
      .Then([&received_callback, expected_error](fidl::Result<Control::SetGain>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(), expected_error)
            << result.error_value().FormatDescription();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

TEST_F(ControlServerWarningTest, SetGainMissingState) {
  TestSetGainBadState(std::nullopt, fuchsia_audio_device::ControlSetGainError::kInvalidGainState);
}

TEST_F(ControlServerWarningTest, SetGainEmptyState) {
  TestSetGainBadState(fuchsia_audio_device::GainState(),
                      fuchsia_audio_device::ControlSetGainError::kInvalidGainDb);
}

TEST_F(ControlServerWarningTest, SetGainMissingGainDb) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .muted = false,
                          .agc_enabled = false,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kInvalidGainDb);
}

TEST_F(ControlServerWarningTest, SetGainTooHigh) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .gain_db = +200.0f,
                          .muted = false,
                          .agc_enabled = false,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kGainOutOfRange);
}

TEST_F(ControlServerWarningTest, SetGainTooLow) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .gain_db = -200.0f,
                          .muted = false,
                          .agc_enabled = false,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kGainOutOfRange);
}

TEST_F(ControlServerWarningTest, BadMute) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .gain_db = 0.0f,
                          .muted = true,
                          .agc_enabled = false,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kMuteUnavailable);
}

TEST_F(ControlServerWarningTest, BadAgc) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .gain_db = 0.0f,
                          .muted = false,
                          .agc_enabled = true,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kAgcUnavailable);
}

void ControlServerWarningTest::TestCreateRingBufferBadOptions(
    const std::optional<fuchsia_audio_device::RingBufferOptions>& bad_options,
    fuchsia_audio_device::ControlCreateRingBufferError expected_error) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(ControlServer::count(), 1u);
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
      fidl::ClientEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_client_end)),
      dispatcher(), ring_buffer_fidl_handler_.get());
  bool received_callback = false;

  control_client
      ->CreateRingBuffer({{
          .options = bad_options,
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback, expected_error](fidl::Result<Control::CreateRingBuffer>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(), expected_error)
            << result.error_value().FormatDescription();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

TEST_F(ControlServerWarningTest, CreateRingBufferMissingOptions) {
  TestCreateRingBufferBadOptions(
      std::nullopt, fuchsia_audio_device::ControlCreateRingBufferError::kInvalidOptions);
}

TEST_F(ControlServerWarningTest, CreateRingBufferEmptyOptions) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions(),
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerWarningTest, CreateRingBufferMissingFormat) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = std::nullopt,
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerWarningTest, CreateRingBufferEmptyFormat) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format(),
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerWarningTest, CreateRingBufferMissingSampleType) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .channel_count = 2,
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerWarningTest, CreateRingBufferBadSampleType) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kFloat64,
              .channel_count = 2,
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch);
}

TEST_F(ControlServerWarningTest, CreateRingBufferMissingChannelCount) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerWarningTest, CreateRingBufferBadChannelCount) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 7,
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch);
}

TEST_F(ControlServerWarningTest, CreateRingBufferMissingFramesPerSecond) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 2,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerWarningTest, CreateRingBufferBadFramesPerSecond) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 2,
              .frames_per_second = 97531,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch);
}

TEST_F(ControlServerWarningTest, CreateRingBufferMissingRingBufferMinBytes) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 2,
              .frames_per_second = 48000,
          }},
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidMinBytes);
}

// TODO(fxbug.dev/117826): Enable this unittest that tests the upper limit of VMO size (4Gb).
// This is not high-priority since even at the service's highest supported frame rate (192 kHz),
// channel_count (8) and sample_type (float64), a 4Gb ring-buffer would be 5.8 minutes long!
TEST_F(ControlServerWarningTest, DISABLED_CreateRingBufferHugeRingBufferMinBytes) {
  EXPECT_EQ(dispatcher(), test_loop().dispatcher());
  zx::channel server_end, client_end;
  EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
  auto fake_driver =
      std::make_unique<FakeAudioDriver>(std::move(server_end), std::move(client_end), dispatcher());

  fake_driver->clear_formats();
  std::vector<fuchsia::hardware::audio::ChannelAttributes> channel_vector;
  channel_vector.push_back(fuchsia::hardware::audio::ChannelAttributes());
  fake_driver->set_channel_sets(0, 0, std::move(channel_vector));
  fake_driver->set_sample_formats(0, {fuchsia::hardware::audio::SampleFormat::PCM_UNSIGNED});
  fake_driver->set_bytes_per_sample(0, {1});
  fake_driver->set_valid_bits_per_sample(0, {8});
  fake_driver->set_frame_rates(0, {48000});

  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  fake_driver->AllocateRingBuffer(8192);
  RunLoopUntilIdle();

  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(ControlServer::count(), 1u);
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
      fidl::ClientEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_client_end)),
      dispatcher(), ring_buffer_fidl_handler_.get());
  bool received_callback = false;

  control_client
      ->CreateRingBuffer({{
          .options = fuchsia_audio_device::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = fuchsia_audio::SampleType::kUint8,
                  .channel_count = 1,
                  .frames_per_second = 48000,
              }},
              .ring_buffer_min_bytes = -1u,
          }},
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        received_callback = true;

        if (result.is_ok()) {
          FX_LOGS(ERROR) << "RingBufferProperties";
          FX_LOGS(ERROR) << "    valid_bits_per_sample: "
                         << static_cast<int16_t>(
                                result->properties()->valid_bits_per_sample().value_or(-1));
          FX_LOGS(ERROR) << "    turn_on_delay:         "
                         << result->properties()->turn_on_delay().value_or(-1);
          FX_LOGS(ERROR) << "fuchsia.audio.RingBuffer";
          FX_LOGS(ERROR) << "    buffer";
          FX_LOGS(ERROR) << "        vmo:               0x" << std::hex
                         << result->ring_buffer()->buffer()->vmo().get() << " (handle)";
          FX_LOGS(ERROR) << "        size:              0x" << std::hex
                         << result->ring_buffer()->buffer()->size();
          FX_LOGS(ERROR) << "    format";
          FX_LOGS(ERROR) << "        sample_type:       "
                         << fidl::ToUnderlying(*result->ring_buffer()->format()->sample_type());
          FX_LOGS(ERROR) << "        channel_count:     "
                         << *result->ring_buffer()->format()->channel_count();
          FX_LOGS(ERROR) << "        frames_per_second: "
                         << *result->ring_buffer()->format()->frames_per_second();
          FX_LOGS(ERROR)
              << "        channel_layout:    "
              << (result->ring_buffer()->format()->channel_layout().has_value()
                      ? std::to_string(fidl::ToUnderlying(
                            result->ring_buffer()->format()->channel_layout()->config().value()))
                      : "NONE");
          FX_LOGS(ERROR) << "    producer_bytes:        0x" << std::hex
                         << *result->ring_buffer()->producer_bytes();
          FX_LOGS(ERROR) << "    consumer_bytes:        0x" << std::hex
                         << *result->ring_buffer()->consumer_bytes();
          FX_LOGS(ERROR) << "    reference_clock:       0x" << std::hex
                         << result->ring_buffer()->reference_clock()->get() << " (handle)";
          FX_LOGS(ERROR) << "    ref_clock_domain:      "
                         << (result->ring_buffer()->reference_clock_domain().has_value()
                                 ? std::to_string(*result->ring_buffer()->reference_clock_domain())
                                 : "NONE");
        }
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreateRingBufferError::kBadRingBufferOption)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

TEST_F(ControlServerWarningTest, CreateRingBufferMissingRingBufferServerEnd) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(ControlServer::count(), 1u);
  bool received_callback = false;

  control_client
      ->CreateRingBuffer({{
          .options = fuchsia_audio_device::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = fuchsia_audio::SampleType::kInt16,
                  .channel_count = 2,
                  .frames_per_second = 48000,
              }},
              .ring_buffer_min_bytes = 8192,
          }},
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreateRingBufferError::kInvalidRingBuffer);
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

TEST_F(ControlServerWarningTest, CreateRingBufferBadRingBufferServerEnd) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(ControlServer::count(), 1u);
  bool received_callback = false;

  control_client
      ->CreateRingBuffer({{
          .options = fuchsia_audio_device::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = fuchsia_audio::SampleType::kInt16,
                  .channel_count = 2,
                  .frames_per_second = 48000,
              }},
              .ring_buffer_min_bytes = 8192,
          }},
          .ring_buffer_server = fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS);
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 0u);
}

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
//     unhealthy before SetGain. Expect Observer/Control/RingBuffer to drop, Reg/WatchRemoved.

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
//     unhealthy before GetCurrentlyPermittedFormats. Expect Obs/Ctl/RBr to drop, Reg/WatchRemoved.

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
//     unhealthy before CreateRingBuffer. Expect Obs/Ctl to drop, Reg/WatchRemoved.

}  // namespace
}  // namespace media_audio
