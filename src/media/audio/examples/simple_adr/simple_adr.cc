// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/examples/simple_adr/simple_adr.h"

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/markers.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fidl/cpp/unified_messaging.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/fzl/vmar-manager.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>
#include <math.h>

#include <cmath>
#include <cstdlib>
#include <ctime>
#include <iomanip>
#include <iostream>
#include <ostream>
#include <string>
#include <utility>

#include <src/lib/fxl/strings/string_printf.h>

namespace examples {

inline std::ostream& operator<<(
    std::ostream& out,
    const std::optional<fuchsia_audio_device::PlugDetectCapabilities>& plug_caps) {
  if (plug_caps) {
    switch (*plug_caps) {
      case fuchsia_audio_device::PlugDetectCapabilities::kHardwired:
        return (out << "kHardwired");
      case fuchsia_audio_device::PlugDetectCapabilities::kPluggable:
        return (out << "kPluggable");
      default:
        return (out << "unknown PlugDetectCapabilities enum");
    }
  }
  return (out << "NONE (non-compliant)");
}

inline std::ostream& operator<<(std::ostream& out,
                                const std::optional<fuchsia_audio_device::PlugState>& plug_state) {
  if (plug_state) {
    switch (*plug_state) {
      case fuchsia_audio_device::PlugState::kPlugged:
        return (out << "kPlugged");
      case fuchsia_audio_device::PlugState::kUnplugged:
        return (out << "kUnplugged");
      default:
        return (out << "unknown PlugState enum");
    }
  }
  return (out << "NONE (non-compliant)");
}

inline std::ostream& operator<<(std::ostream& out,
                                const std::optional<fuchsia_audio::SampleType>& sample_type) {
  if (sample_type) {
    switch (*sample_type) {
      case fuchsia_audio::SampleType::kUint8:
        return (out << "kUint8");
      case fuchsia_audio::SampleType::kInt16:
        return (out << "kInt16");
      case fuchsia_audio::SampleType::kInt32:
        return (out << "kInt32");
      case fuchsia_audio::SampleType::kFloat32:
        return (out << "kFloat32");
      case fuchsia_audio::SampleType::kFloat64:
        return (out << "kFloat64");
      default:
        return (out << "unknown SampleType enum");
    }
  }
  return (out << "NONE (non-compliant)");
}

inline std::ostream& operator<<(std::ostream& out,
                                const std::optional<fuchsia_audio::ChannelLayout>& channel_layout) {
  if (channel_layout) {
    switch (channel_layout->config().value()) {
      case fuchsia_audio::ChannelConfig::kMono:
        return (out << "kMono");
      case fuchsia_audio::ChannelConfig::kStereo:
        return (out << "kStereo");
      case fuchsia_audio::ChannelConfig::kQuad:
        return (out << "kQuad");
      case fuchsia_audio::ChannelConfig::kSurround3:
        return (out << "kSurround3");
      case fuchsia_audio::ChannelConfig::kSurround4:
        return (out << "kSurround4");
      case fuchsia_audio::ChannelConfig::kSurround51:
        return (out << "kSurround51");
      default:
        return (out << "unknown ChannelConfig enum");
    }
  }
  return (out << "NONE");
}

template <typename ProtocolT>
FidlHandler<ProtocolT>::FidlHandler(MediaApp* parent, std::string_view name)
    : parent_(parent), name_(name) {}

// fidl::AsyncEventHandler<> implementation, called when the server disconnects its channel.
template <typename ProtocolT>
void FidlHandler<ProtocolT>::on_fidl_error(fidl::UnbindInfo error) {
  // fidl::AsyncEventHandler<> implementation, called when the server disconnects its channel.
  std::cerr << name_ << ":" << __func__ << ", shutting down... " << error << '\n';
  parent_->Shutdown();
}

MediaApp::MediaApp(async::Loop& loop, fit::closure quit_callback)
    : loop_(loop), quit_callback_(std::move(quit_callback)) {
  FX_CHECK(quit_callback_);
}

void MediaApp::Run() {
  std::cout << __func__ << '\n';

  ConnectToRegistry();
  WaitForFirstAudioOutput();
}

void MediaApp::ConnectToRegistry() {
  std::cout << __func__ << '\n';

  zx::result client_end = component::Connect<fuchsia_audio_device::Registry>();
  registry_client_ = fidl::Client<fuchsia_audio_device::Registry>(
      std::move(*client_end), loop_.dispatcher(), &reg_handler_);
}

void MediaApp::WaitForFirstAudioOutput() {
  std::cout << __func__ << '\n';

  registry_client_->WatchDevicesAdded().Then([this](fidl::Result<fuchsia_audio_device::Registry::
                                                                     WatchDevicesAdded>& result) {
    std::cout << "Registry/WatchDevicesAdded callback" << '\n';
    if (result.is_error()) {
      std::cerr << "Registry/WatchDevicesAdded error: " << result.error_value().FormatDescription()
                << '\n';
      Shutdown();
      return;
    }

    // Now that we have been notified of a device, log its information and configure the first
    // ring_buffer with the first supported ring_buffer_format.
    for (const auto& device : *result->devices()) {
      if (*device.device_type() == fuchsia_audio_device::DeviceType::kOutput) {
        device_token_id_ = *device.token_id();
        std::cout << "Connecting to audio device:" << '\n';
        std::cout << "    token_id                  " << device_token_id_ << '\n';
        std::cout << "    device_type               kOutput" << '\n';
        std::cout << "    device_name               " << *device.device_name() << '\n';
        std::cout << "    manufacturer              " << device.manufacturer().value_or("NONE")
                  << '\n';
        std::cout << "    product                   " << device.product().value_or("NONE") << '\n';

        std::string uid_str;
        if (!device.unique_instance_id()) {
          uid_str = "NONE";
        } else {
          uid_str = fxl::StringPrintf(
              "0x%02X%02X%02X%02X%02X%02X%02X%02X%02X%02X%02X%02X%02X%02X%02X%02X",
              (*device.unique_instance_id())[0], (*device.unique_instance_id())[1],
              (*device.unique_instance_id())[2], (*device.unique_instance_id())[3],
              (*device.unique_instance_id())[4], (*device.unique_instance_id())[5],
              (*device.unique_instance_id())[6], (*device.unique_instance_id())[7],
              (*device.unique_instance_id())[8], (*device.unique_instance_id())[9],
              (*device.unique_instance_id())[10], (*device.unique_instance_id())[11],
              (*device.unique_instance_id())[12], (*device.unique_instance_id())[13],
              (*device.unique_instance_id())[14], (*device.unique_instance_id())[15]);
        }
        std::cout << "    unique_instance_id        " << uid_str << '\n';

        // Log ring_buffer_format_sets details
        std::cout << "    ring_buffer_format_sets[" << device.ring_buffer_format_sets()->size()
                  << "]" << '\n';
        for (auto idx = 0u; idx < device.ring_buffer_format_sets()->size(); ++idx) {
          auto element_ring_buffer_format_set = device.ring_buffer_format_sets()->at(idx);
          std::cout << "        [" << idx << "]   element_id  "
                    << *element_ring_buffer_format_set.element_id() << '\n';
          std::cout << "              format_set ["
                    << element_ring_buffer_format_set.format_sets()->size() << "]" << '\n';
          for (auto idx = 0u; idx < element_ring_buffer_format_set.format_sets()->size(); ++idx) {
            auto ring_buffer_format_set = element_ring_buffer_format_set.format_sets()->at(idx);
            std::cout << "                  [" << idx << "]  channel_sets ["
                      << ring_buffer_format_set.channel_sets()->size() << "]" << '\n';
            for (auto cs = 0u; cs < ring_buffer_format_set.channel_sets()->size(); ++cs) {
              auto channel_set = (*ring_buffer_format_set.channel_sets())[cs];
              std::cout << "                [" << cs << "]   attributes["
                        << channel_set.attributes()->size() << "]" << '\n';
              for (auto a = 0u; a < channel_set.attributes()->size(); ++a) {
                auto attribs = (*channel_set.attributes())[a];
                std::cout << "                        [" << a << "]   min_frequency  "
                          << (attribs.min_frequency().has_value()
                                  ? std::to_string(*attribs.min_frequency())
                                  : "NONE")
                          << '\n';
                std::cout << "                              max_frequency  "
                          << (attribs.max_frequency().has_value()
                                  ? std::to_string(*attribs.max_frequency())
                                  : "NONE")
                          << '\n';
              }
            }
            std::cout << "            sample_types["
                      << ring_buffer_format_set.sample_types()->size() << "]" << '\n';
            for (auto st = 0u; st < ring_buffer_format_set.sample_types()->size(); ++st) {
              std::cout << "                [" << st << "]     "
                        << (*ring_buffer_format_set.sample_types())[st] << '\n';
            }
            std::cout << "            frame_rates [" << ring_buffer_format_set.frame_rates()->size()
                      << "]" << '\n';
            for (auto fr = 0u; fr < ring_buffer_format_set.frame_rates()->size(); ++fr) {
              std::cout << "                [" << fr << "]     "
                        << (*ring_buffer_format_set.frame_rates())[fr] << '\n';
            }
          }
        }

        // Include `dai_format_sets` here too.

        std::cout << "    is_input                  "
                  << (device.is_input() ? (*device.is_input() ? "true" : "false") : "UNSPECIFIED")
                  << '\n';

        std::cout << "    gain_caps" << '\n';
        std::cout << "        min_gain_db           "
                  << (device.gain_caps()->min_gain_db()
                          ? std::to_string(*device.gain_caps()->min_gain_db()) + " dB"
                          : "NONE (non-compliant)")
                  << '\n';
        std::cout << "        max_gain_db           "
                  << (device.gain_caps()->max_gain_db()
                          ? std::to_string(*device.gain_caps()->max_gain_db()) + " dB"
                          : "NONE (non-compliant)")
                  << '\n';
        std::cout << "        gain_step_db          "
                  << (device.gain_caps()->gain_step_db()
                          ? std::to_string(*device.gain_caps()->gain_step_db()) + " dB"
                          : "NONE (non-compliant)")
                  << '\n';
        std::cout << "        can_mute              "
                  << (device.gain_caps()->can_mute()
                          ? (*device.gain_caps()->can_mute() ? "TRUE" : "FALSE")
                          : "NONE (FALSE)")
                  << '\n';
        std::cout << "        can_agc               "
                  << (device.gain_caps()->can_agc()
                          ? (*device.gain_caps()->can_agc() ? "TRUE" : "FALSE")
                          : "NONE (FALSE)")
                  << '\n';
        max_gain_db_ = *device.gain_caps()->max_gain_db();  // This example chooses
        max_gain_db_ = std::min(max_gain_db_, 0.0f);        // not to exceed 0 dB.
        min_gain_db_ = *device.gain_caps()->min_gain_db();

        std::cout << "    plug_caps                 " << device.plug_detect_caps() << '\n';

        std::string clk_domain_str;
        if (!device.clock_domain()) {
          clk_domain_str = "unspecified (CLOCK_DOMAIN_EXTERNAL)";
        } else if (*device.clock_domain() == 0xFFFFFFFF) {
          clk_domain_str = "CLOCK_DOMAIN_EXTERNAL";
        } else if (*device.clock_domain() == 0) {
          clk_domain_str = "CLOCK_DOMAIN_MONOTONIC";
        } else {
          clk_domain_str = std::to_string(*device.clock_domain()) + " (not MONOTONIC)";
        }
        std::cout << "    clock_domain              " << clk_domain_str << '\n';

        // Include `signal_processing_elements` here too.

        // Include `signal_processing_topologies` here too.

        // Now determine the format we will use, and connect more deeply to the device.
        if (!device.ring_buffer_format_sets() || device.ring_buffer_format_sets()->empty() ||
            !device.ring_buffer_format_sets()->front().format_sets() ||
            device.ring_buffer_format_sets()->front().format_sets()->empty() ||
            !device.ring_buffer_format_sets()->front().format_sets()->front().channel_sets() ||
            device.ring_buffer_format_sets()
                ->front()
                .format_sets()
                ->front()
                .channel_sets()
                ->empty() ||
            !device.ring_buffer_format_sets()
                 ->front()
                 .format_sets()
                 ->front()
                 .channel_sets()
                 ->front()
                 .attributes() ||
            device.ring_buffer_format_sets()
                ->front()
                .format_sets()
                ->front()
                .channel_sets()
                ->front()
                .attributes()
                ->empty()) {
          std::cout
              << "Cannot determine a channel-count from ring_buffer_format_sets (missing/empty)";
          Shutdown();
          return;
        }
        // For convenience, just use the first channel configuration that the device listed.
        channels_per_frame_ = device.ring_buffer_format_sets()
                                  ->front()
                                  .format_sets()
                                  ->front()
                                  .channel_sets()
                                  ->front()
                                  .attributes()
                                  ->size();

        // If we didn't get a valid overall format, then we shouldn't continue onward.
        if (!channels_per_frame_) {
          return;
        }

        ObserveDevice();
        ConnectToControlCreator();
        // Create a RingBuffer and play a tone in that format to that RingBuffer.
        ControlDevice();
      }
    }
  });
}

void MediaApp::ObserveDevice() {
  std::cout << __func__ << '\n';

  zx::channel server_end, client_end;
  zx::channel::create(0, &server_end, &client_end);
  observer_client_ = fidl::Client<fuchsia_audio_device::Observer>(
      fidl::ClientEnd<fuchsia_audio_device::Observer>(std::move(client_end)), loop_.dispatcher(),
      &obs_handler_);

  registry_client_
      ->CreateObserver({{
          .token_id = device_token_id_,
          .observer_server = fidl::ServerEnd<fuchsia_audio_device::Observer>(std::move(server_end)),
      }})
      .Then([this](fidl::Result<fuchsia_audio_device::Registry::CreateObserver>& result) {
        std::cout << "Registry/CreateObserver callback" << '\n';
        if (!result.is_ok()) {
          std::cerr << "Registry/CreateObserver error: " << result.error_value().FormatDescription()
                    << '\n';
          Shutdown();
          return;
        }

        observer_client_->WatchGainState().Then(
            [this](fidl::Result<fuchsia_audio_device::Observer::WatchGainState>& result) {
              std::cout << "Observer/WatchGainState callback" << '\n';
              if (!result.is_ok()) {
                std::cerr << "Observer/WatchGainState error: "
                          << result.error_value().FormatDescription() << '\n';
                Shutdown();
              }

              std::cout << "GainState" << '\n';
              std::cout << "    gain_db:                  "
                        << (result->state()->gain_db()
                                ? std::to_string(*result->state()->gain_db()) + " dB"
                                : "NONE (non-compliant)")
                        << '\n';
              std::cout << "    muted:                    "
                        << (result->state()->muted()
                                ? (*result->state()->muted() ? "TRUE" : "FALSE")
                                : "NONE (FALSE)")
                        << '\n';
              std::cout << "    agc_enabled:              "
                        << (result->state()->agc_enabled()
                                ? (*result->state()->agc_enabled() ? "TRUE" : "FALSE")
                                : "NONE (FALSE)")
                        << '\n';
            });
        observer_client_->WatchPlugState().Then(
            [this](fidl::Result<fuchsia_audio_device::Observer::WatchPlugState>& result) {
              std::cout << "Observer/WatchPlugState callback" << '\n';
              if (!result.is_ok()) {
                std::cerr << "Observer/WatchPlugState error: "
                          << result.error_value().FormatDescription() << '\n';
                Shutdown();
              }

              std::cout << "PlugState" << '\n';
              std::cout << "    state:                    " << result->state() << '\n';
              std::cout << "    plug_time (nsec):         "
                        << (result->plug_time() ? std::to_string(*result->plug_time())
                                                : "NONE (non-compliant)")
                        << '\n';
            });
      });
}

void MediaApp::ConnectToControlCreator() {
  std::cout << __func__ << '\n';

  zx::result client_end = component::Connect<fuchsia_audio_device::ControlCreator>();
  control_creator_client_ = fidl::Client<fuchsia_audio_device::ControlCreator>(
      std::move(*client_end), loop_.dispatcher(), &ctl_crtr_handler_);
}

void MediaApp::ControlDevice() {
  std::cout << __func__ << '\n';

  zx::channel server_end, client_end;
  zx::channel::create(0, &server_end, &client_end);
  control_client_ = fidl::Client<fuchsia_audio_device::Control>(
      fidl::ClientEnd<fuchsia_audio_device::Control>(std::move(client_end)), loop_.dispatcher(),
      &ctl_handler_);

  control_creator_client_
      ->Create({{
          .token_id = device_token_id_,
          .control_server = fidl::ServerEnd<fuchsia_audio_device::Control>(std::move(server_end)),
      }})
      .Then([this](fidl::Result<fuchsia_audio_device::ControlCreator::Create>& result) {
        std::cout << "ControlCreator/Create callback" << '\n';
        if (!result.is_ok()) {
          std::cerr << "ControlCreator/Create error: " << result.error_value().FormatDescription()
                    << '\n';
          Shutdown();
          return;
        }

        CreateRingBufferConnection();
      });
}

void MediaApp::CreateRingBufferConnection() {
  std::cout << __func__ << '\n';

  zx::channel server_end, client_end;
  zx::channel::create(0, &server_end, &client_end);
  ring_buffer_client_ = fidl::Client<fuchsia_audio_device::RingBuffer>(
      fidl::ClientEnd<fuchsia_audio_device::RingBuffer>(std::move(client_end)), loop_.dispatcher(),
      &rb_handler_);

  control_client_
      ->CreateRingBuffer({{
          .options = fuchsia_audio_device::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = kSampleFormat,
                  .channel_count = channels_per_frame_,
                  .frames_per_second = kFrameRate,
              }},
              .ring_buffer_min_bytes = 4096,
          }},
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(server_end)),
      }})
      .Then([this](fidl::Result<fuchsia_audio_device::Control::CreateRingBuffer>& result) {
        std::cout << "Control/CreateRingBuffer callback" << '\n';
        if (!result.is_ok()) {
          std::cerr << "Control/CreateRingBuffer error: "
                    << result.error_value().FormatDescription() << '\n';
          Shutdown();
          return;
        }
        FX_CHECK(result->properties() && result->ring_buffer());
        FX_CHECK(result->properties()->valid_bits_per_sample() &&
                 result->properties()->turn_on_delay());
        FX_CHECK(result->ring_buffer()->buffer() && result->ring_buffer()->format() &&
                 result->ring_buffer()->producer_bytes() &&
                 result->ring_buffer()->consumer_bytes() &&
                 result->ring_buffer()->reference_clock());

        std::cout << "Control/CreateRingBuffer is_ok" << '\n';

        ring_buffer_ = std::move(*result->ring_buffer());
        const auto fmt = ring_buffer_.format();

        std::cout << "properties:" << '\n';
        std::cout << "    valid_bits_per_sample:    "
                  << static_cast<uint32_t>(*result->properties()->valid_bits_per_sample()) << '\n';
        std::cout << "    turn_on_delay (nsec):     " << *result->properties()->turn_on_delay()
                  << '\n';
        std::cout << "ring_buffer:" << '\n';
        std::cout << "    buffer:" << '\n';
        std::cout << "        vmo (handle):         0x" << std::hex
                  << ring_buffer_.buffer()->vmo().get() << std::dec << '\n';
        std::cout << "        size:                 " << ring_buffer_.buffer()->size() << '\n';
        std::cout << "    format:" << '\n';
        std::cout << "        sample_type:          " << fmt->sample_type() << '\n';
        std::cout << "        channel_count:        " << *fmt->channel_count() << '\n';
        std::cout << "        frames_per_second:    " << *fmt->frames_per_second() << '\n';
        std::cout << "        channel_layout:       " << fmt->channel_layout() << '\n';
        std::cout << "    producer_bytes:           " << *ring_buffer_.producer_bytes() << '\n';
        std::cout << "    consumer_bytes:           " << *ring_buffer_.consumer_bytes() << '\n';
        std::cout << "    reference_clock (handle): 0x" << std::hex
                  << ring_buffer_.reference_clock()->get() << std::dec << '\n';
        std::string clk_domain_str;
        if (!ring_buffer_.reference_clock_domain()) {
          clk_domain_str = "unspecified (CLOCK_DOMAIN_EXTERNAL)";
        } else if (*ring_buffer_.reference_clock_domain() == 0xFFFFFFFF) {
          clk_domain_str = "CLOCK_DOMAIN_EXTERNAL";
        } else if (*ring_buffer_.reference_clock_domain() == 0) {
          clk_domain_str = "CLOCK_DOMAIN_MONOTONIC";
        } else {
          clk_domain_str =
              std::to_string(*ring_buffer_.reference_clock_domain()) + " (not MONOTONIC)";
        }
        std::cout << "    reference_clock_domain:   " << clk_domain_str << '\n';

        if (!MapRingBufferVmo()) {
          Shutdown();
          return;
        }
        WriteAudioToVmo();
        StartRingBuffer();
      });
}

namespace {

fbl::RefPtr<fzl::VmarManager>* CreateVmarManager() {
  constexpr size_t kSize = 16ull * 1024 * 1024 * 1024;
  constexpr zx_vm_option_t kFlags =
      ZX_VM_COMPACT | ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE | ZX_VM_ALIGN_1GB;

  auto ptr = new fbl::RefPtr<fzl::VmarManager>;
  *ptr = fzl::VmarManager::Create(kSize, nullptr, kFlags);
  return ptr;
}
const fbl::RefPtr<fzl::VmarManager>* const vmar_manager = CreateVmarManager();

}  // namespace

// Validate and map the VMO.
bool MediaApp::MapRingBufferVmo() {
  std::cout << __func__ << '\n';

  auto buffer = std::move(*ring_buffer_.buffer());
  ring_buffer_size_ = buffer.size();

  zx_info_vmo_t info;
  if (auto status = buffer.vmo().get_info(ZX_INFO_VMO, &info, sizeof(info), nullptr, nullptr);
      status != ZX_OK) {
    std::cout << "WARNING: vmo.get_info failed: " << std::to_string(status) << '\n';
    return false;
  }
  if ((info.flags & ZX_INFO_VMO_RESIZABLE) != 0) {
    std::cout << "WARNING: vmo is resizable, which is not permittted" << '\n';
    return false;
  }

  // The VMO must allow mapping with appropriate permissions.
  zx_rights_t expected_rights = ZX_RIGHT_READ | ZX_RIGHT_MAP | ZX_RIGHT_WRITE;
  if ((info.handle_rights & expected_rights) != expected_rights) {
    std::cout << "WARNING: invalid rights = 0x" << std::hex << std::setw(4) << std::setfill('0')
              << info.handle_rights << " (required rights = 0x" << std::setw(4) << std::setfill('0')
              << expected_rights << ")" << std::dec << '\n';
    return false;
  }
  std::cout << "Mapping a mem.Buffer/size of " << ring_buffer_size_ << '\n';

  // Map.
  zx_vm_option_t flags = ZX_VM_PERM_READ | ZX_VM_PERM_WRITE;
  if (auto status =
          ring_buffer_mapper_.Map(buffer.vmo(), 0, ring_buffer_size_, flags, *vmar_manager);
      status != ZX_OK) {
    std::cout << "WARNING: VmpMapper.Map failed with status=" << std::to_string(status) << '\n';
    return false;
  }

  void* vmo_start = ring_buffer_mapper_.start();
  void* vmo_end = static_cast<char*>(vmo_start) + ring_buffer_size_;
  rb_start_ = static_cast<int16_t*>(vmo_start);
  std::cout << "Mapped a VMO of size: 0x" << std::hex << ring_buffer_size_ << ", to [0x"
            << vmo_start << ", 0x" << vmo_end << ")" << std::dec << '\n';
  return true;
}

// Write a .125-amplitude sinusoid that can loop around the ring buffer.
void MediaApp::WriteAudioToVmo() {
  // Floor this to integer, to get a perfectly continuous signal across ring-buffer wraparound.
  auto cycles_per_buffer =
      static_cast<uint16_t>(static_cast<float>(ring_buffer_size_) / kApproxFramesPerCycle /
                            static_cast<float>(kBytesPerSample * channels_per_frame_));

  std::cout << "Writing " << cycles_per_buffer << " cycles in this " << ring_buffer_size_
            << "-byte buffer: a "
            << (static_cast<double>(channels_per_frame_ * kFrameRate * cycles_per_buffer *
                                    kBytesPerSample) /
                static_cast<double>(ring_buffer_size_))
            << "-hz tone (estimated " << kApproxToneFrequency << "-hz)" << '\n';
  for (size_t idx = 0; idx < ring_buffer_size_ / (kBytesPerSample * channels_per_frame_); ++idx) {
    auto val = static_cast<int16_t>(
        32768.0f * kToneAmplitude *
        std::sin(static_cast<double>(idx * 2 * (kBytesPerSample * channels_per_frame_) *
                                     cycles_per_buffer) *
                 M_PI / static_cast<double>(ring_buffer_size_)));
    for (size_t chan = 0; chan < channels_per_frame_; ++chan) {
      rb_start_[idx * channels_per_frame_ + chan] = val;
    }
  }
}

void MediaApp::StartRingBuffer() {
  std::cout << __func__ << '\n';

  ring_buffer_client_->Start({}).Then(
      [this](fidl::Result<fuchsia_audio_device::RingBuffer::Start>& result) {
        std::cout << "RingBuffer/Start callback" << '\n';
        if (!result.is_ok()) {
          std::cerr << "RingBuffer/Start error: " << result.error_value().FormatDescription()
                    << '\n';
          Shutdown();
          return;
        }
        std::cout << "RingBuffer/Start is_ok, playback has begun" << '\n';
        // Over 3 seconds, stair-step the device gain from -15 dB to 0 dB.
        ChangeGainByDbAfter(5.0f, zx::sec(1), 3);
      });
}

void MediaApp::ChangeGainByDbAfter(float change_db, zx::duration wait_duration,
                                   int32_t iterations) {
  auto gain_db = std::max(min_gain_db_, max_gain_db_ - change_db * static_cast<float>(iterations));
  std::cout << "Setting device gain to " << gain_db << " dB" << '\n';

  control_client_
      ->SetGain({{fuchsia_audio_device::GainState{{
          .gain_db = gain_db,
      }}}})
      .Then([this, change_db, wait_duration,
             iterations](fidl::Result<fuchsia_audio_device::Control::SetGain>&) {
        zx::nanosleep(zx::deadline_after(wait_duration));
        if (!iterations) {
          StopRingBuffer();
          return;
        }
        ChangeGainByDbAfter(change_db, wait_duration, iterations - 1);
      });
}

void MediaApp::StopRingBuffer() {
  std::cout << __func__ << '\n';

  ring_buffer_client_->Stop({}).Then(
      [this](fidl::Result<fuchsia_audio_device::RingBuffer::Stop>& result) {
        std::cout << "RingBuffer/Stop callback" << '\n';
        if (!result.is_ok()) {
          std::cerr << "RingBuffer/Stop error: " << result.error_value().FormatDescription()
                    << '\n';
        } else {
          std::cout << "RingBuffer/Stop is_ok: success!" << '\n';
        }

        Shutdown();
      });
}

// Unmap memory, quit message loop (FIDL interfaces auto-delete upon ~MediaApp).
void MediaApp::Shutdown() {
  std::cout << __func__ << '\n';

  quit_callback_();
}

}  // namespace examples

int main(int argc, const char** argv) {
  fuchsia_logging::SetTags({"simple_adr"});

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  auto startup_context = sys::ComponentContext::CreateAndServeOutgoingDirectory();

  examples::MediaApp media_app(
      loop, [&loop]() { async::PostTask(loop.dispatcher(), [&loop]() { loop.Quit(); }); });

  media_app.Run();

  loop.Run();  // Now wait for the message loop to return...

  return 0;
}
