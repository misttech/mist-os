// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/test/test_base.h"

#include <fcntl.h>
#include <fuchsia/component/cpp/fidl.h>
#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <fuchsia/logger/cpp/fidl.h>
#include <fuchsia/media/cpp/fidl.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/enum.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>

#include <algorithm>
#include <cstddef>
#include <cstring>
#include <string>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/media/audio/drivers/test/audio_device_enumerator_stub.h"

namespace media::audio::drivers::test {

using component_testing::ChildRef;
using component_testing::ParentRef;
using component_testing::Protocol;
using component_testing::RealmBuilder;
using component_testing::RealmRoot;
using component_testing::Route;

// Device discovery is done once at binary open; a fresh FIDL channel is used for each test.
void TestBase::SetUp() {
  media::audio::test::TestFixture::SetUp();

  auto& entry = device_entry();
  if (entry.isA2DP()) {
    ConnectToBluetoothDevice();
  } else {
    switch (entry.driver_type) {
      case DriverType::StreamConfigInput:
        [[fallthrough]];
      case DriverType::StreamConfigOutput:
        ConnectToStreamConfigDevice(device_entry());
        break;
      case DriverType::Dai:
        ConnectToDaiDevice(device_entry());
        break;
      case DriverType::Codec:
        ConnectToCodecDevice(device_entry());
        break;
      case DriverType::Composite:
        ConnectToCompositeDevice(device_entry());
        break;
    }
  }
}

void TestBase::TearDown() {
  stream_config_.Unbind();
  dai_.Unbind();
  codec_.Unbind();
  composite_.Unbind();

  if (realm_.has_value()) {
    // We're about to shut down the realm; unbind to unhook the error handler.
    audio_binder_.Unbind();
    bool complete = false;
    realm_.value().Teardown(
        [&](fit::result<fuchsia::component::Error> result) { complete = true; });
    RunLoopUntil([&]() { return complete; });
  }

  // Audio drivers can have multiple StreamConfig channels open, but only one can be 'privileged':
  // the one that can in turn create a RingBuffer channel. Each test case starts from scratch,
  // opening and closing channels. If we create a StreamConfig channel before the previous one is
  // cleared, a new StreamConfig channel will not be privileged and Admin tests will fail.
  //
  // When disconnecting a StreamConfig, there's no signal to wait on before proceeding (potentially
  // immediately executing other tests); insert a 10-ms wait (needing >3.5ms was never observed).
  zx::nanosleep(zx::deadline_after(zx::msec(10)));

  TestFixture::TearDown();
}

void TestBase::ConnectToBluetoothDevice() {
  std::unique_ptr<AudioDeviceEnumeratorStub> audio_device_enumerator_impl =
      std::make_unique<AudioDeviceEnumeratorStub>();
  auto audio_device_enumerator_impl_ptr = audio_device_enumerator_impl.get();

  auto builder = RealmBuilder::Create();
  // The component binding must live as long as the Realm, so std::move the
  // unique_ptr into the component function.
  builder.AddLocalChild(
      "audio-device-enumerator",
      [audio_device_enumerator_impl = std::move(audio_device_enumerator_impl)]() mutable {
        // Note: This lambda does not create a new instance,
        // so the component can only be started once.
        return std::move(audio_device_enumerator_impl);
      });
  builder.AddChild("audio-device-output-harness", "#meta/audio-device-output-harness.cm");
  builder.AddRoute(Route{.capabilities = {Protocol{fuchsia::media::AudioDeviceEnumerator::Name_}},
                         .source = ChildRef{"audio-device-enumerator"},
                         .targets = {ChildRef{"audio-device-output-harness"}}});
  builder.AddRoute(Route{.capabilities = {Protocol{fuchsia::logger::LogSink::Name_}},
                         .source = ParentRef{},
                         .targets = {ChildRef{"audio-device-output-harness"}}});
  builder.AddRoute(Route{
      .capabilities = {Protocol{.name = fuchsia::component::Binder::Name_, .as = "audio-binder"}},
      .source = ChildRef{"audio-device-output-harness"},
      .targets = {ParentRef{}}});
  realm_ = builder.Build();
  ASSERT_EQ(ZX_OK,
            realm_->component().Connect("audio-binder", audio_binder_.NewRequest().TakeChannel()));
  audio_binder_.set_error_handler([](zx_status_t status) {
    FAIL() << "audio-device-output-harness exited: " << zx_status_get_string(status);
  });

  // Wait for the Bluetooth harness to AddDeviceByChannel, then pass it on
  RunLoopUntil([impl = audio_device_enumerator_impl_ptr]() {
    return impl->channel_available() || HasFailure();
  });
  CreateStreamConfigFromChannel(audio_device_enumerator_impl_ptr->TakeChannel());
}

// Given this device_entry, use its channel to open the StreamConfig device.
void TestBase::ConnectToStreamConfigDevice(const DeviceEntry& device_entry) {
  fuchsia::hardware::audio::StreamConfigConnectorPtr device;
  ASSERT_EQ(device_entry.dir.index(), 1u);
  ASSERT_EQ(fdio_service_connect_at(std::get<1>(device_entry.dir).channel()->get(),
                                    device_entry.filename.c_str(),
                                    device.NewRequest().TakeChannel().release()),
            ZX_OK);

  device.set_error_handler([this](zx_status_t status) {
    FAIL() << status << "Err " << status << ", failed to open channel to audio " << driver_type();
  });
  fidl::InterfaceHandle<fuchsia::hardware::audio::StreamConfig> stream_config_client;
  fidl::InterfaceRequest<fuchsia::hardware::audio::StreamConfig> stream_config_server =
      stream_config_client.NewRequest();
  device->Connect(std::move(stream_config_server));

  auto channel = stream_config_client.TakeChannel();
  FX_LOGS(TRACE) << "Successfully opened devnode '" << device_entry.filename << "' for audio "
                 << driver_type();

  CreateStreamConfigFromChannel(
      fidl::InterfaceHandle<fuchsia::hardware::audio::StreamConfig>(std::move(channel)));
}

// Given this device_entry, use its channel to open the Dai device.
void TestBase::ConnectToDaiDevice(const DeviceEntry& device_entry) {
  fuchsia::hardware::audio::DaiConnectorPtr device;
  ASSERT_EQ(device_entry.dir.index(), 1u);
  ASSERT_EQ(fdio_service_connect_at(std::get<1>(device_entry.dir).channel()->get(),
                                    device_entry.filename.c_str(),
                                    device.NewRequest().TakeChannel().release()),
            ZX_OK);

  device.set_error_handler([](zx_status_t status) {
    FAIL() << status << "Err " << status << ", failed to open channel to audio DAI";
  });
  fidl::InterfaceHandle<fuchsia::hardware::audio::Dai> dai_client;
  fidl::InterfaceRequest<fuchsia::hardware::audio::Dai> dai_server = dai_client.NewRequest();
  device->Connect(std::move(dai_server));

  auto channel = dai_client.TakeChannel();
  FX_LOGS(TRACE) << "Successfully opened devnode '" << device_entry.filename << "' for audio DAI";

  CreateDaiFromChannel(fidl::InterfaceHandle<fuchsia::hardware::audio::Dai>(std::move(channel)));
}

// Given this device_entry, use its channel to open the Codec device.
void TestBase::ConnectToCodecDevice(const DeviceEntry& device_entry) {
  fuchsia::hardware::audio::CodecConnectorPtr device;
  ASSERT_EQ(device_entry.dir.index(), 1u);
  ASSERT_EQ(fdio_service_connect_at(std::get<1>(device_entry.dir).channel()->get(),
                                    device_entry.filename.c_str(),
                                    device.NewRequest().TakeChannel().release()),
            ZX_OK);

  device.set_error_handler([](zx_status_t status) {
    FAIL() << status << "Err " << status << ", failed to open channel to audio Codec";
  });
  fidl::InterfaceHandle<fuchsia::hardware::audio::Codec> codec_client;
  fidl::InterfaceRequest<fuchsia::hardware::audio::Codec> codec_server = codec_client.NewRequest();
  device->Connect(std::move(codec_server));

  auto channel = codec_client.TakeChannel();
  FX_LOGS(TRACE) << "Successfully opened devnode '" << device_entry.filename << "' for audio codec";

  CreateCodecFromChannel(
      fidl::InterfaceHandle<fuchsia::hardware::audio::Codec>(std::move(channel)));
}

void TestBase::ConnectToCompositeDevice(const DeviceEntry& device_entry) {
  fuchsia::hardware::audio::CompositeConnectorPtr device;
  ASSERT_EQ(device_entry.dir.index(), 1u);
  ASSERT_EQ(fdio_service_connect_at(std::get<1>(device_entry.dir).channel()->get(),
                                    device_entry.filename.c_str(),
                                    device.NewRequest().TakeChannel().release()),
            ZX_OK);

  device.set_error_handler([](zx_status_t status) {
    FAIL() << status << "Err " << status << ", failed to open channel to audio composite";
  });
  fidl::InterfaceHandle<fuchsia::hardware::audio::Composite> composite_client;
  fidl::InterfaceRequest<fuchsia::hardware::audio::Composite> composite_server =
      composite_client.NewRequest();
  device->Connect(std::move(composite_server));

  auto channel = composite_client.TakeChannel();
  FX_LOGS(TRACE) << "Successfully opened devnode '" << device_entry.filename
                 << "' for composite audio driver";

  CreateCompositeFromChannel(
      fidl::InterfaceHandle<fuchsia::hardware::audio::Composite>(std::move(channel)));
}

void TestBase::CreateStreamConfigFromChannel(
    fidl::InterfaceHandle<fuchsia::hardware::audio::StreamConfig> channel) {
  stream_config_ = channel.Bind();

  // If no device was enumerated, don't waste further time.
  if (!stream_config_.is_bound()) {
    FAIL() << "Failed to get stream channel for this device";
  }
  AddErrorHandler(stream_config_, "StreamConfig");
}

void TestBase::CreateDaiFromChannel(fidl::InterfaceHandle<fuchsia::hardware::audio::Dai> channel) {
  dai_ = channel.Bind();

  // If no device was enumerated, don't waste further time.
  if (!dai_.is_bound()) {
    FAIL() << "Failed to get DAI channel for this device";
  }
  AddErrorHandler(dai_, "DAI");
}

void TestBase::CreateCodecFromChannel(
    fidl::InterfaceHandle<fuchsia::hardware::audio::Codec> channel) {
  codec_ = channel.Bind();

  // If no device was enumerated, don't waste further time.
  if (!codec_.is_bound()) {
    FAIL() << "Failed to get codec channel for this device";
  }
  AddErrorHandler(codec_, "Codec");
}

void TestBase::CreateCompositeFromChannel(
    fidl::InterfaceHandle<fuchsia::hardware::audio::Composite> channel) {
  composite_ = channel.Bind();

  // If no device was enumerated, don't waste further time.
  if (!composite_.is_bound()) {
    FAIL() << "Failed to get composite channel for this device";
  }
  AddErrorHandler(composite_, "Composite");
}

// Request that the driver return the format ranges that it supports.
void TestBase::RequestFormats() {
  if (device_entry().isComposite()) {
    RequestTopologies();

    // If there is a ring buffer id, request the ring buffer formats for this endpoint.
    // "No ring buffer" is also valid; do nothing in that case.
    if (ring_buffer_id_.has_value()) {
      composite()->GetRingBufferFormats(
          ring_buffer_id_.value(),
          AddCallback(
              "GetRingBufferFormats",
              [this](fuchsia::hardware::audio::Composite_GetRingBufferFormats_Result result) {
                EXPECT_FALSE(result.is_err());
                auto& supported_formats = result.response().ring_buffer_formats;
                EXPECT_FALSE(supported_formats.empty());

                for (size_t i = 0; i < supported_formats.size(); ++i) {
                  SCOPED_TRACE(testing::Message() << "Composite supported_formats[" << i << "]");
                  ASSERT_TRUE(supported_formats[i].has_pcm_supported_formats());
                  auto& format_set = *supported_formats[i].mutable_pcm_supported_formats();
                  ring_buffer_pcm_formats_.push_back(std::move(format_set));
                }
              }));
    }
    // If there is a dai id, request the DAI formats for this endpoint.
    // "No DAI" is also valid; do nothing in that case.
    if (dai_id_.has_value()) {
      composite()->GetDaiFormats(
          dai_id_.value(),
          AddCallback("GetDaiFormats",
                      [this](fuchsia::hardware::audio::Composite_GetDaiFormats_Result result) {
                        EXPECT_FALSE(result.is_err());
                        auto& supported_formats = result.response().dai_formats;
                        EXPECT_FALSE(supported_formats.empty());

                        for (size_t i = 0; i < supported_formats.size(); ++i) {
                          SCOPED_TRACE(testing::Message() << "DAI supported_formats[" << i << "]");
                          dai_formats_.push_back(std::move(supported_formats[i]));
                        }
                      }));
    }
  } else if (device_entry().isDai()) {
    dai()->GetRingBufferFormats(
        AddCallback("GetRingBufferFormats",
                    [this](fuchsia::hardware::audio::Dai_GetRingBufferFormats_Result result) {
                      EXPECT_FALSE(result.is_err());
                      auto& supported_rb_formats = result.response().ring_buffer_formats;
                      EXPECT_FALSE(supported_rb_formats.empty());

                      for (size_t i = 0; i < supported_rb_formats.size(); ++i) {
                        SCOPED_TRACE(testing::Message() << "DAI supported_rb_formats[" << i << "]");
                        ASSERT_TRUE(supported_rb_formats[i].has_pcm_supported_formats());
                        auto& format_set = *supported_rb_formats[i].mutable_pcm_supported_formats();
                        ring_buffer_pcm_formats_.push_back(std::move(format_set));
                      }
                    }));
    dai()->GetDaiFormats(AddCallback(
        "GetDaiFormats", [this](fuchsia::hardware::audio::Dai_GetDaiFormats_Result result) {
          EXPECT_FALSE(result.is_err());
          auto& supported_dai_formats = result.response().dai_formats;
          EXPECT_FALSE(supported_dai_formats.empty());

          for (size_t i = 0; i < supported_dai_formats.size(); ++i) {
            SCOPED_TRACE(testing::Message() << "DAI supported_dai_formats[" << i << "]");
            dai_formats_.push_back(std::move(supported_dai_formats[i]));
          }
        }));
  } else if (device_entry().isCodec()) {
  } else {
    stream_config()->GetSupportedFormats(AddCallback(
        "GetSupportedFormats",
        [this](std::vector<fuchsia::hardware::audio::SupportedFormats> supported_formats) {
          EXPECT_FALSE(supported_formats.empty());

          for (size_t i = 0; i < supported_formats.size(); ++i) {
            SCOPED_TRACE(testing::Message() << "StreamConfig supported_formats[" << i << "]");
            ASSERT_TRUE(supported_formats[i].has_pcm_supported_formats());
            auto& format_set = *supported_formats[i].mutable_pcm_supported_formats();
            ring_buffer_pcm_formats_.push_back(std::move(format_set));
          }
        }));
  }
  ExpectCallbacks();
  if (!HasFailure()) {
    SetMinMaxFormats();
  }
}

void TestBase::LogFormat(const fuchsia::hardware::audio::PcmFormat& format,
                         const std::string& tag) {
  FX_LOGS(WARNING) << tag << ": rate " << format.frame_rate << ", fmt "
                   << static_cast<int>(format.sample_format) << ", " << format.bytes_per_sample * 8u
                   << "b (" << static_cast<uint16_t>(format.valid_bits_per_sample)
                   << " valid), chans " << static_cast<uint16_t>(format.number_of_channels);
}

void TestBase::SetMinMaxFormats() {
  SetMinMaxRingBufferFormats();
  SetMinMaxDaiFormats();
}

void TestBase::SetMinMaxRingBufferFormats() {
  for (size_t i = 0; i < ring_buffer_pcm_formats_.size(); ++i) {
    SCOPED_TRACE(testing::Message() << "pcm_format[" << i << "]");
    size_t min_chans = 0, max_chans = 0;
    uint8_t min_bytes_per_sample = 0, max_bytes_per_sample = 0;
    uint8_t min_valid_bits_per_sample = 0, max_valid_bits_per_sample = 0;
    uint32_t min_frame_rate = 0, max_frame_rate = 0;

    auto& format_set = ring_buffer_pcm_formats_[i];
    EXPECT_GT(format_set.sample_formats().size(), 0u);
    fuchsia::hardware::audio::SampleFormat sample_format = format_set.sample_formats()[0];

    for (size_t j = 0; j < format_set.channel_sets().size(); ++j) {
      if (j == 0 || format_set.channel_sets()[j].attributes().size() < min_chans) {
        min_chans = format_set.channel_sets()[j].attributes().size();
      }
      if (j == 0 || format_set.channel_sets()[j].attributes().size() > max_chans) {
        max_chans = format_set.channel_sets()[j].attributes().size();
      }
    }

    for (size_t j = 0; j < format_set.bytes_per_sample().size(); ++j) {
      SCOPED_TRACE(testing::Message() << "bytes_per_sample[" << j << "]");
      EXPECT_GT(format_set.bytes_per_sample()[j], 0u);

      if (j == 0 || format_set.bytes_per_sample()[j] < min_bytes_per_sample) {
        min_bytes_per_sample = format_set.bytes_per_sample()[j];
      }
      if (j == 0 || format_set.bytes_per_sample()[j] > max_bytes_per_sample) {
        max_bytes_per_sample = format_set.bytes_per_sample()[j];
      }
    }

    for (size_t j = 0; j < format_set.valid_bits_per_sample().size(); ++j) {
      SCOPED_TRACE(testing::Message() << "valid_bits_per_sample[" << j << "]");
      EXPECT_LE(format_set.valid_bits_per_sample()[j], max_bytes_per_sample * 8);
      EXPECT_GT(format_set.valid_bits_per_sample()[j], 0u);

      if (j == 0 || format_set.valid_bits_per_sample()[j] < min_valid_bits_per_sample) {
        min_valid_bits_per_sample = format_set.valid_bits_per_sample()[j];
      }
      if (j == 0 || format_set.valid_bits_per_sample()[j] > max_valid_bits_per_sample) {
        max_valid_bits_per_sample = format_set.valid_bits_per_sample()[j];
      }
    }
    EXPECT_LE(min_valid_bits_per_sample, min_bytes_per_sample * 8);
    EXPECT_LE(max_valid_bits_per_sample, max_bytes_per_sample * 8);

    for (size_t j = 0; j < format_set.frame_rates().size(); ++j) {
      if (j == 0 || format_set.frame_rates()[j] < min_frame_rate) {
        min_frame_rate = format_set.frame_rates()[j];
      }
      if (j == 0 || format_set.frame_rates()[j] > max_frame_rate) {
        max_frame_rate = format_set.frame_rates()[j];
      }
    }

    // Save, if less than min.
    auto bit_rate = min_chans * min_bytes_per_sample * min_frame_rate;
    if (i == 0 || bit_rate < static_cast<size_t>(min_ring_buffer_format_.number_of_channels) *
                                 min_ring_buffer_format_.bytes_per_sample *
                                 min_ring_buffer_format_.frame_rate) {
      min_ring_buffer_format_ = {
          .number_of_channels = static_cast<uint8_t>(min_chans),
          .sample_format = sample_format,
          .bytes_per_sample = min_bytes_per_sample,
          .valid_bits_per_sample = min_valid_bits_per_sample,
          .frame_rate = min_frame_rate,
      };
    }

    // Save, if more than max.
    bit_rate = max_chans * max_bytes_per_sample * max_frame_rate;
    if (i == 0 || bit_rate > static_cast<size_t>(max_ring_buffer_format_.number_of_channels) *
                                 max_ring_buffer_format_.bytes_per_sample *
                                 max_ring_buffer_format_.frame_rate) {
      max_ring_buffer_format_ = {
          .number_of_channels = static_cast<uint8_t>(max_chans),
          .sample_format = sample_format,
          .bytes_per_sample = max_bytes_per_sample,
          .valid_bits_per_sample = max_valid_bits_per_sample,
          .frame_rate = max_frame_rate,
      };
    }
  }
}

void TestBase::SetMinMaxDaiFormats() {
  for (size_t i = 0; i < dai_formats_.size(); ++i) {
    SCOPED_TRACE(testing::Message() << "DAI format[" << i << "]");
    uint32_t min_number_of_channels = 0, max_number_of_channels = 0;
    uint8_t min_bits_per_slot = 0, max_bits_per_slot = 0;
    uint8_t min_bits_per_sample = 0, max_bits_per_sample = 0;
    uint32_t min_frame_rate = 0, max_frame_rate = 0;

    auto& format_set = dai_formats_[i];
    // Pick the first sample and frame format.
    EXPECT_GT(format_set.sample_formats.size(), 0u);
    fuchsia::hardware::audio::DaiSampleFormat sample_format = format_set.sample_formats[0];
    fuchsia::hardware::audio::DaiFrameFormat frame_format;
    EXPECT_GT(format_set.frame_formats.size(), 0u);
    EXPECT_EQ(fuchsia::hardware::audio::Clone(format_set.frame_formats[0], &frame_format), ZX_OK);

    for (size_t j = 0; j < format_set.number_of_channels.size(); ++j) {
      SCOPED_TRACE(testing::Message() << "number_of_channels[" << j << "]");
      EXPECT_GT(format_set.number_of_channels[j], 0u);

      if (j == 0 || format_set.number_of_channels[j] < min_number_of_channels) {
        min_number_of_channels = format_set.number_of_channels[j];
      }
      if (j == 0 || format_set.number_of_channels[j] > max_number_of_channels) {
        max_number_of_channels = format_set.number_of_channels[j];
      }
    }

    for (size_t j = 0; j < format_set.bits_per_slot.size(); ++j) {
      SCOPED_TRACE(testing::Message() << "bits_per_slot[" << j << "]");
      EXPECT_GT(format_set.bits_per_slot[j], 0u);

      if (j == 0 || format_set.bits_per_slot[j] < min_bits_per_slot) {
        min_bits_per_slot = format_set.bits_per_slot[j];
      }
      if (j == 0 || format_set.bits_per_slot[j] > max_bits_per_slot) {
        max_bits_per_slot = format_set.bits_per_slot[j];
      }
    }

    for (size_t j = 0; j < format_set.bits_per_sample.size(); ++j) {
      SCOPED_TRACE(testing::Message() << "bits_per_sample[" << j << "]");
      EXPECT_LE(format_set.bits_per_sample[j], max_bits_per_slot * 8);
      EXPECT_GT(format_set.bits_per_sample[j], 0u);

      if (j == 0 || format_set.bits_per_sample[j] < min_bits_per_sample) {
        min_bits_per_sample = format_set.bits_per_sample[j];
      }
      if (j == 0 || format_set.bits_per_sample[j] > max_bits_per_sample) {
        max_bits_per_sample = format_set.bits_per_sample[j];
      }
    }
    EXPECT_LE(min_bits_per_sample, min_bits_per_slot);
    EXPECT_LE(max_bits_per_sample, max_bits_per_slot);

    for (size_t j = 0; j < format_set.frame_rates.size(); ++j) {
      if (j == 0 || format_set.frame_rates[j] < min_frame_rate) {
        min_frame_rate = format_set.frame_rates[j];
      }
      if (j == 0 || format_set.frame_rates[j] > max_frame_rate) {
        max_frame_rate = format_set.frame_rates[j];
      }
    }

    // Save, if less than min.
    auto bit_rate = min_number_of_channels * min_bits_per_slot * min_frame_rate;
    if (i == 0 || bit_rate < min_dai_format_.number_of_channels * min_dai_format_.bits_per_slot *
                                 min_dai_format_.frame_rate) {
      min_dai_format_ = fuchsia::hardware::audio::DaiFormat{
          .number_of_channels = min_number_of_channels,
          .sample_format = sample_format,
          .frame_format = std::move(frame_format),
          .frame_rate = min_frame_rate,
          .bits_per_slot = min_bits_per_slot,
          .bits_per_sample = min_bits_per_sample,
      };
    }
    // Save, if more than max.
    bit_rate = max_number_of_channels * max_bits_per_slot * max_frame_rate;
    if (i == 0 || bit_rate > max_dai_format_.number_of_channels * max_dai_format_.bits_per_slot *
                                 max_dai_format_.frame_rate) {
      max_dai_format_ = fuchsia::hardware::audio::DaiFormat{
          .number_of_channels = max_number_of_channels,
          .sample_format = sample_format,
          .frame_format = std::move(frame_format),
          .frame_rate = max_frame_rate,
          .bits_per_slot = max_bits_per_slot,
          .bits_per_sample = max_bits_per_sample,
      };
    }
  }
}

void TestBase::SignalProcessingConnect() {
  if (sp_.is_bound()) {
    return;  // Already connected.
  }
  fidl::InterfaceHandle<fuchsia::hardware::audio::signalprocessing::SignalProcessing> sp_client;
  fidl::InterfaceRequest<fuchsia::hardware::audio::signalprocessing::SignalProcessing> sp_server =
      sp_client.NewRequest();
  composite()->SignalProcessingConnect(std::move(sp_server));
  sp_ = sp_client.Bind();
}

void TestBase::RequestTopologies() {
  SignalProcessingConnect();
  zx_status_t status = ZX_OK;
  sp_->GetElements(AddCallback(
      "Composite::GetElements",
      [this,
       &status](fuchsia::hardware::audio::signalprocessing::Reader_GetElements_Result result) {
        status = result.is_err() ? result.err() : ZX_OK;
        if (status == ZX_OK) {
          elements_ = std::move(result.response().processing_elements);
          ring_buffer_id_.reset();
          dai_id_.reset();
          for (auto& element : elements_) {
            if (element.type() ==
                fuchsia::hardware::audio::signalprocessing::ElementType::ENDPOINT) {
              if (element.type_specific().endpoint().type() ==
                  fuchsia::hardware::audio::signalprocessing::EndpointType::RING_BUFFER) {
                ring_buffer_id_.emplace(element.id());  // Override any previous.
              } else if (element.type_specific().endpoint().type() ==
                         fuchsia::hardware::audio::signalprocessing::EndpointType::
                             DAI_INTERCONNECT) {
                dai_id_.emplace(element.id());  // Override any previous.
              }
            }
          }
        }
      }));
  ExpectCallbacks();

  // Either we get elements or the API method is not supported.
  ASSERT_TRUE(status == ZX_OK || status == ZX_ERR_NOT_SUPPORTED);
  // We don't check for topologies if GetElements is not supported.
  if (status == ZX_ERR_NOT_SUPPORTED) {
    return;
  }
  // If supported, GetElements must return at least one element.
  ASSERT_TRUE(!elements_.empty());

  sp_->GetTopologies(AddCallback(
      "Composite::GetTopologies",
      [this](fuchsia::hardware::audio::signalprocessing::Reader_GetTopologies_Result result) {
        ASSERT_TRUE(!result.is_err());
        topologies_ = std::move(result.response().topologies);
      }));
  ExpectCallbacks();

  // We only call GetTopologies if we have elements, so we must have at least one topology.
  ASSERT_TRUE(!topologies_.empty());
}

}  // namespace media::audio::drivers::test
