// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "codec_adapter_cvsd_encoder.h"

#include <lib/media/codec_impl/codec_port.h>
#include <lib/media/codec_impl/log.h>

#include <safemath/safe_math.h>

namespace {
// Each audio sample is signed 16 bits (2 byte).
constexpr uint32_t kInputBytesPerSample = 2;
// Input chunk size matches the number of bytes required to produce 1
// output byte.
// 2 byte signed input -> 1 bit output.
// 2 byte signed * 8 inputs -> 8 bits (1 byte) output.
constexpr uint32_t kInputChunkSize = kInputBytesPerSample * 8;

// The implementations are based off of the CVSD codec algorithm defined in
// Bluetooth Core Spec v5.3, Vol 2, Part B Sec 9.2.
uint8_t SingleEncode(CodecParams* params, const int16_t& input_sample) {
  const int16_t x = input_sample;

  // Determine output value (EQ 15).
  const uint8_t bit = (x >= params->accumulator) ? 0 : 1;

  UpdateCVSDParams(params, bit);

  return bit;
}
}  // namespace

CodecAdapterCvsdEncoder::CodecAdapterCvsdEncoder(std::mutex& lock,
                                                 CodecAdapterEvents* codec_adapter_events)
    : CodecAdapterSW(lock, codec_adapter_events) {}

fuchsia::sysmem::BufferCollectionConstraints
CodecAdapterCvsdEncoder::CoreCodecGetBufferCollectionConstraints(
    CodecPort port, const fuchsia::media::StreamBufferConstraints& stream_buffer_constraints,
    const fuchsia::media::StreamBufferPartialSettings& partial_settings) {
  std::lock_guard<std::mutex> lock(lock_);

  fuchsia::sysmem::BufferCollectionConstraints result;

  // The CodecImpl won't hand us the sysmem token, so we shouldn't expect to
  // have the token here.
  ZX_DEBUG_ASSERT(!partial_settings.has_sysmem_token());

  if (port == kOutputPort) {
    result.min_buffer_count_for_camping = kOutputMinBufferCountForCamping;
  } else {
    result.min_buffer_count_for_camping = kInputMinBufferCountForCamping;
  }
  ZX_DEBUG_ASSERT(result.min_buffer_count_for_dedicated_slack == 0);
  ZX_DEBUG_ASSERT(result.min_buffer_count_for_shared_slack == 0);

  // Actual minimum buffer size required for input is 16 and for output is
  // 1 byte since we're doing 16:1 compression for encoding. However, sysmem
  // will basically allocate at least a 4KiB page per buffer even if the client
  // is also ok with less, so we default to system page size for now.
  if (port == kInputPort) {
    // TODO(dayeonglee): consider requiring a larger buffer, based on what
    // seems like a minimum reasonable time duration per buffer, to keep the
    // buffers per second <= ~120.
    result.buffer_memory_constraints.min_size_bytes = zx_system_get_page_size();
    result.buffer_memory_constraints.max_size_bytes = kInputPerPacketBufferBytesMax;
  } else {
    result.buffer_memory_constraints.min_size_bytes = zx_system_get_page_size();
    // Set to some arbitrary value.
    result.buffer_memory_constraints.max_size_bytes = 0xFFFFFFFF;
  }

  result.has_buffer_memory_constraints = true;

  // These are all false because SW encode.
  result.buffer_memory_constraints.physically_contiguous_required = false;
  result.buffer_memory_constraints.secure_required = false;

  ZX_DEBUG_ASSERT(result.image_format_constraints_count == 0);

  // We don't have to fill out usage - CodecImpl takes care of that.
  ZX_DEBUG_ASSERT(!result.usage.cpu);
  ZX_DEBUG_ASSERT(!result.usage.display);
  ZX_DEBUG_ASSERT(!result.usage.vulkan);
  ZX_DEBUG_ASSERT(!result.usage.video);

  return result;
}

void CodecAdapterCvsdEncoder::CoreCodecSetBufferCollectionInfo(
    CodecPort port, const fuchsia::sysmem::BufferCollectionInfo_2& buffer_collection_info) {
  // Nothing to do here.
}

void CodecAdapterCvsdEncoder::CleanUpAfterStream() { codec_params_ = std::nullopt; }

void CodecAdapterCvsdEncoder::CoreCodecStopStream() {
  PostSerial(input_processing_loop_.dispatcher(), [this] {
    if (output_item_ && output_item_->buffer) {
      // If we have an output buffer pending but not sent, return it to the pool. CodecAdapterSW
      // expects all buffers returned after stream is stopped.
      auto base = output_item_->buffer->base();
      output_buffer_pool_.FreeBuffer(base);
      output_item_->buffer = nullptr;
    }
  });

  CodecAdapterSW::CoreCodecStopStream();
}

std::pair<fuchsia::media::FormatDetails, size_t> CodecAdapterCvsdEncoder::OutputFormatDetails() {
  ZX_DEBUG_ASSERT(codec_params_);
  fuchsia::media::AudioCompressedFormatCvsd cvsd;
  fuchsia::media::AudioCompressedFormat compressed_format;
  compressed_format.set_cvsd(std::move(cvsd));

  fuchsia::media::AudioFormat audio_format;
  audio_format.set_compressed(std::move(compressed_format));

  fuchsia::media::FormatDetails format_details;
  format_details.set_mime_type(kCvsdMimeType);
  format_details.mutable_domain()->set_audio(std::move(audio_format));

  // The bytes needed to store each output packet. Since we're doing 16-1
  // compression for mono audio, where 2 byte input = 1 bit output. bytes needed
  // to store each output packet is 1.
  return {std::move(format_details), 1};
}

void CodecAdapterCvsdEncoder::ProcessInputLoop() {
  std::optional<CodecInputItem> maybe_input_item;
  while ((maybe_input_item = input_queue_.WaitForElement())) {
    CodecInputItem item = std::move(maybe_input_item.value());

    if (!item.is_valid()) {
      return;
    }

    // Item is format details.
    if (item.is_format_details()) {
      if (ProcessFormatDetails(item.format_details()) == kShouldTerminate) {
        // A failure was reported through `events_` or the stream was stopped.
        return;
      }
      // CodecImpl guarantees that QueueInputFormatDetails() will happen before
      // any input packets (in CodecImpl::HandlePendingInputFormatDetails()),
      // regardless of whether the input format is ultimately provided during
      // StreamProcessor create or via a format item queued from the client.
      // So we can be sure that this call happens before any input packets.
      events_->onCoreCodecMidStreamOutputConstraintsChange(
          /*output_re_config_required=*/true);
    } else if (item.is_end_of_stream()) {
      if (ProcessEndOfStream(&item) == kShouldTerminate) {
        // A failure was reported through `events_` or the stream was stopped.
        return;
      }
      events_->onCoreCodecOutputEndOfStream(false);
    } else {
      // Input is packet.
      ZX_DEBUG_ASSERT(item.is_packet());
      auto status = ProcessInputPacket(item.packet());
      events_->onCoreCodecInputPacketDone(item.packet());
      if (status == kShouldTerminate) {
        // A failure was reported through `events_` or the stream was stopped.
        return;
      }
    }
  }
}

InputLoopStatus CodecAdapterCvsdEncoder::ProcessFormatDetails(
    const fuchsia::media::FormatDetails& format_details) {
  if (!format_details.has_domain() || !format_details.domain().is_audio() ||
      !format_details.domain().audio().is_uncompressed() ||
      !format_details.domain().audio().uncompressed().is_pcm()) {
    events_->onCoreCodecFailCodec(
        "CVSD Encoder received input that was not uncompressed pcm audio.");
    return kShouldTerminate;
  }
  if (!format_details.has_encoder_settings() || !format_details.encoder_settings().is_cvsd()) {
    events_->onCoreCodecFailCodec("CVSD Encoder did not receive encoder settings.");
    return kShouldTerminate;
  }
  auto& input_format = format_details.domain().audio().uncompressed().pcm();
  if (input_format.pcm_mode != fuchsia::media::AudioPcmMode::LINEAR ||
      input_format.bits_per_sample != 16 || input_format.channel_map.size() != 1) {
    events_->onCoreCodecFailCodec(
        "CVSD Encoder only encodes mono audio with signed 16 bit linear samples.");
    return kShouldTerminate;
  }

  InitCodecParams(codec_params_);
  InitChunkInputStream(format_details);
  return kOk;
}

void CodecAdapterCvsdEncoder::InitChunkInputStream(
    const fuchsia::media::FormatDetails& format_details) {
  auto& input_format = format_details.domain().audio().uncompressed().pcm();
  if (input_format.frames_per_second != kExpectedSamplingFreq) {
    FX_LOGS(WARNING) << "Expected sampling frequency " << kExpectedSamplingFreq << " got "
                     << input_format.frames_per_second;
  }

  // Bytes per second is sampling frequency * number_of_channels * bytes_per_sample.
  const size_t bytes_per_second = 1ull * input_format.frames_per_second * kInputBytesPerSample;

  chunk_input_stream_.emplace(
      kInputChunkSize,
      format_details.has_timebase()
          ? TimestampExtrapolator(format_details.timebase(), bytes_per_second)
          : TimestampExtrapolator(),
      [this](const ChunkInputStream::InputBlock input_block) {
        // Get the output buffer for encoding the current input block.
        if (!output_item_ || output_item_->packet == nullptr) {
          std::optional<CodecPacket*> maybe_output_packet = free_output_packets_.WaitForElement();
          if (!maybe_output_packet) {
            // We should close the stream since we couldn't fetch output buffer.
            return ChunkInputStream::kTerminate;
          }
          CodecPacket* packet = *maybe_output_packet;
          ZX_DEBUG_ASSERT(packet);

          const CodecBuffer* buffer = output_buffer_pool_.AllocateBuffer();
          ZX_DEBUG_ASSERT(buffer);
          auto checked_buffer_length = safemath::MakeCheckedNum(buffer->size()).Cast<uint32_t>();
          ZX_DEBUG_ASSERT(checked_buffer_length.IsValid());
          ZX_DEBUG_ASSERT(checked_buffer_length.ValueOrDie() > 0);
          SetOutputItem(output_item_, packet, buffer);
        }

        ZX_DEBUG_ASSERT(output_item_);
        ZX_DEBUG_ASSERT(output_item_->packet);
        ZX_DEBUG_ASSERT(output_item_->buffer);
        ZX_DEBUG_ASSERT(output_item_->buffer->size() > 0);

        // Only encode if the input block has some real data.
        if (input_block.non_padding_len != 0) {
          // Update timestamp if timestamp hasn't been updated for
          // the current output packet yet.
          if (output_item_->data_length == 0 && input_block.timestamp_ish) {
            output_item_->timestamp.emplace(*input_block.timestamp_ish);
          }

          // TODO(dayeonglee): change so that the encode would encode more than
          // one byte at a time$.
          // Encode contents of input buffer to current output buffer.
          Encode(&(*codec_params_), const_cast<uint8_t*>(input_block.data),
                 output_item_->buffer->base() + output_item_->data_length);
          // For now, Encode only encodes a single byte.
          output_item_->data_length++;
        }

        // Output current buffer if it cannot encode another input block or if
        // current input block indicates end of stream and the output contains
        // some data.
        // TODO(fxb/116824): consider outputting current packet if the input
        // queue is empty and the the chunk input stream does not have at least
        // one more chunk after the current chunk.
        if (output_item_->data_length == output_item_->buffer->size() ||
            (input_block.is_end_of_stream && output_item_->data_length > 0)) {
          SendAndResetOutputPacket();
        }

        return ChunkInputStream::kContinue;
      });
}

InputLoopStatus CodecAdapterCvsdEncoder::ProcessEndOfStream(CodecInputItem* item) {
  ZX_DEBUG_ASSERT(item->is_end_of_stream());
  return ProcessCodecPacket(nullptr);
}

InputLoopStatus CodecAdapterCvsdEncoder::ProcessInputPacket(CodecPacket* packet) {
  return ProcessCodecPacket(packet);
}

InputLoopStatus CodecAdapterCvsdEncoder::ProcessCodecPacket(CodecPacket* packet) {
  ZX_DEBUG_ASSERT(codec_params_);
  ZX_DEBUG_ASSERT(chunk_input_stream_);
  ChunkInputStream::Status status;
  if (packet == nullptr) {
    status = chunk_input_stream_->Flush();
  } else {
    status = chunk_input_stream_->ProcessInputPacket(packet);
  }

  switch (status) {
    case ChunkInputStream::kExtrapolationFailedWithoutTimebase:
      events_->onCoreCodecFailCodec("Timebase was not set for extrapolation.");
      return kShouldTerminate;
    case ChunkInputStream::kUserTerminated:
      return kShouldTerminate;
    default:
      return kOk;
  };
}

// Encode input buffer of 16 byte size to produce one byte of output data.
void CodecAdapterCvsdEncoder::Encode(CodecParams* params, const uint8_t* input_data,
                                     uint8_t* output) {
  uint8_t output_byte = 0x00;
  for (uint32_t idx = 0; idx < kInputChunkSize; idx += 2) {
    auto audio_sample = reinterpret_cast<const int16_t*>(&input_data[idx]);
    output_byte = static_cast<uint8_t>(output_byte << 1);
    output_byte |= SingleEncode(params, *audio_sample);
  }
  *output = output_byte;
}

void CodecAdapterCvsdEncoder::SendAndResetOutputPacket() {
  ZX_DEBUG_ASSERT(output_item_);
  ZX_DEBUG_ASSERT(output_item_->packet);
  ZX_DEBUG_ASSERT(output_item_->data_length > 0);

  auto packet = output_item_->packet;
  packet->SetBuffer(output_item_->buffer);
  packet->SetStartOffset(0);
  packet->SetValidLengthBytes(output_item_->data_length);
  if (output_item_->timestamp) {
    packet->SetTimstampIsh(*output_item_->timestamp);
  } else {
    packet->ClearTimestampIsh();
  }

  {
    TRACE_INSTANT("codec_runner", "Media:PacketSent", TRACE_SCOPE_THREAD);

    fit::closure free_buffer = [this, base = packet->buffer()->base()] {
      output_buffer_pool_.FreeBuffer(base);
    };
    std::lock_guard<std::mutex> lock(lock_);
    in_use_by_client_[packet] = fit::defer(std::move(free_buffer));
  }

  events_->onCoreCodecOutputPacket(packet,
                                   /*error_detected_before=*/false,
                                   /*error_detected_during=*/false);

  // Reset output item.
  output_item_ = std::nullopt;
}
