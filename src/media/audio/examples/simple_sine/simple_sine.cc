// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/examples/simple_sine/simple_sine.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <math.h>

#include <iostream>
#include <utility>

namespace {
// Set the AudioRenderer stream type to: 48 kHz, mono, 32-bit float.
constexpr uint32_t kFrameRate = 48000;

// This example feeds the system 1 second of audio, in 10-millisecond payloads.
constexpr uint32_t kNumPayloadsPerBuffer = 100;
constexpr uint32_t kNumPacketsToSend = kNumPayloadsPerBuffer;
constexpr uint32_t kFramesPerPayload = kFrameRate / kNumPayloadsPerBuffer;

// Play a 439 Hz sine wave at 1/8 of full-scale volume.
constexpr double kFrequency = 439.0;
constexpr double kAmplitude = 0.125;
}  // namespace

namespace examples {

MediaApp::MediaApp(fit::closure quit_callback) : quit_callback_(std::move(quit_callback)) {
  FX_CHECK(quit_callback_);
}

// Prepare for playback, submit initial data and start the presentation timeline.
void MediaApp::Run(sys::ComponentContext* app_context) {
  AcquireAudioRenderer(app_context);
  AcceptAudioRendererDefaultClock();
  SetStreamType();

  if (CreateMemoryMapping() != ZX_OK) {
    Shutdown();
    return;
  }

  WriteAudioIntoBuffer();
  for (uint32_t payload_num = 0; payload_num < kNumPayloadsPerBuffer; ++payload_num) {
    SendPacket(CreatePacket(payload_num));
  }

  // AudioRenderer defaults to unity gain, unmuted; we need not change our loudness.
  // (Although not shown here, we would do so via a GainControl, obtained from the AudioRenderer.)

  // By not explicitly setting timestamp values (neither reference clock nor PTS), we indicate that
  // we want to start playback, with default timing. This means we start at a reference_time of "as
  // soon as safely possible", when we present audio corresponding to a media_time (PTS) of zero.
  audio_renderer_->PlayNoReply(fuchsia::media::NO_TIMESTAMP, fuchsia::media::NO_TIMESTAMP);
}

// Use StartupContext to acquire AudioPtr, which we only need in order to get an AudioRendererPtr.
// Set an error handler, in case of channel closure.
void MediaApp::AcquireAudioRenderer(sys::ComponentContext* app_context) {
  fuchsia::media::AudioPtr audio = app_context->svc()->Connect<fuchsia::media::Audio>();

  audio->CreateAudioRenderer(audio_renderer_.NewRequest());

  audio_renderer_.set_error_handler([this](zx_status_t status) {
    std::cerr << "fuchsia::media::AudioRenderer connection lost: " << status << std::endl;
    Shutdown();
  });
}

// Tell AudioRenderer we want to use its 'optimal' reference clock, not our own.
void MediaApp::AcceptAudioRendererDefaultClock() {
  audio_renderer_->SetReferenceClock(zx::clock(ZX_HANDLE_INVALID));
}

// Set the AudioRenderer's audio stream_type: mono 48kHz 32-bit float.
void MediaApp::SetStreamType() {
  fuchsia::media::AudioStreamType stream_type;

  stream_type.sample_format = fuchsia::media::AudioSampleFormat::FLOAT;
  stream_type.channels = 1;
  stream_type.frames_per_second = kFrameRate;

  audio_renderer_->SetPcmStreamType(stream_type);
}

// Create a Virtual Memory Object, and map enough memory for audio buffers. This will be our cross-
// process shared buffer, so send a duplicate handle (with reduced-rights) to AudioRenderer.
zx_status_t MediaApp::CreateMemoryMapping() {
  zx::vmo payload_vmo;

  payload_size_ = kFramesPerPayload * sizeof(float);
  total_mapping_size_ = payload_size_ * kNumPayloadsPerBuffer;

  zx_status_t status =
      payload_buffer_.CreateAndMap(total_mapping_size_, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr,
                                   &payload_vmo, ZX_RIGHT_READ | ZX_RIGHT_MAP | ZX_RIGHT_TRANSFER);

  if (status != ZX_OK) {
    std::cerr << "VmoMapper:::CreateAndMap failed: " << status << std::endl;
    return status;
  }

  audio_renderer_->AddPayloadBuffer(0, std::move(payload_vmo));

  return ZX_OK;
}

// Write a sine wave into our buffer; we will submit packets that point to it.
void MediaApp::WriteAudioIntoBuffer() {
  auto float_buffer = reinterpret_cast<float*>(payload_buffer_.start());

  for (uint32_t frame = 0; frame < kFramesPerPayload * kNumPayloadsPerBuffer; ++frame) {
    float_buffer[frame] = static_cast<float>(
        kAmplitude * sin(2.0 * M_PI * (kFrequency / static_cast<double>(kFrameRate)) *
                         static_cast<double>(frame)));
  }
}

// Create the packet that corresponds to the given number in the sequence.
// We divide our cross-proc buffer into different zones, called payloads. Each packet sent to
// AudioRenderer corresponds to a payload. By specifying NO_TIMESTAMP for each packet's presentation
// timestamp, we rely on AudioRenderer to treat the sequence of payloads as a continuous unbroken
// stream of audio (even if the payloads are not contiguous in memory). A client simply must present
// packets early enough. For this example, we actually submit all packets before playback starts.
fuchsia::media::StreamPacket MediaApp::CreatePacket(uint32_t packet_num) const {
  fuchsia::media::StreamPacket packet;

  // By default upon packet construction, .pts is fuchsia::media::NO_TIMESTAMP; leave this as-is.
  // By default upon construction, .payload_buffer_id is 0; leave this (we only map one buffer).
  packet.payload_offset = (packet_num * payload_size_) % total_mapping_size_;
  packet.payload_size = payload_size_;
  return packet;
}

// Submit a packet, incrementing our count of packets sent. When it returns:
// a. if there are more packets to send, create and send the next packet;
// b. if all expected packets have completed, begin closing down the system.
void MediaApp::SendPacket(fuchsia::media::StreamPacket packet) {
  ++num_packets_sent_;
  audio_renderer_->SendPacket(packet, [this]() { OnSendPacketComplete(); });
}

void MediaApp::OnSendPacketComplete() {
  ++num_packets_completed_;
  FX_CHECK(num_packets_completed_ <= kNumPacketsToSend);

  if (num_packets_sent_ < kNumPacketsToSend) {
    SendPacket(CreatePacket(num_packets_sent_));
  } else if (num_packets_completed_ >= kNumPacketsToSend) {
    Shutdown();
  }
}

// Unmap memory, quit message loop (FIDL interfaces auto-delete upon ~MediaApp).
void MediaApp::Shutdown() {
  payload_buffer_.Unmap();
  quit_callback_();
}

}  // namespace examples

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  auto startup_context = sys::ComponentContext::CreateAndServeOutgoingDirectory();

  examples::MediaApp media_app(
      [&loop]() { async::PostTask(loop.dispatcher(), [&loop]() { loop.Quit(); }); });

  media_app.Run(startup_context.get());

  loop.Run();  // Now wait for the message loop to return...

  return 0;
}
