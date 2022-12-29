// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "codec_adapter_cvsd_decoder.h"

#include <safemath/safe_math.h>

namespace {

// Each bit of CVSD compressed audio is equal 2 bytes (signed 16 bits) of uncompressed output audio.
constexpr uint32_t kOutputBytesPerCompressedBit = 2;
// We should be able to decode at least 1 byte (8 bits) of CVSD compressed audio.
// That is equal to 2 bytes * 8 = 16 bytes of resulting uncompressed output audio.
constexpr uint32_t kOutputPerPacketBufferBytesMin = kOutputBytesPerCompressedBit * 8;

constexpr char kPcmMimeType[] = "audio/pcm";
constexpr uint8_t kPcmBitsPerSample = 16;

// The implementations are based off of the CVSD codec algorithm defined in
// Bluetooth Core Spec v5.3, Vol 2, Part B Sec 9.2.
int16_t SingleDecode(CodecParams* params, const uint8_t& compressed_bit) {
  UpdateCVSDParams(params, compressed_bit);

  int16_t x = Round(params->accumulator);
  return x;
}
}  // namespace

CodecAdapterCvsdDecoder::CodecAdapterCvsdDecoder(std::mutex& lock,
                                                 CodecAdapterEvents* codec_adapter_events)
    : CodecAdapterSW(lock, codec_adapter_events) {}

fuchsia::sysmem::BufferCollectionConstraints
CodecAdapterCvsdDecoder::CoreCodecGetBufferCollectionConstraints(
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

  // Actual minimum buffer size required for input is 1 and for output is
  // 16 bytes since we're doing 1:16 decompression for decoding. However, sysmem
  // will basically allocate at least a 4KiB page per buffer even if the client
  // is also ok with less, so we default to system page size for now.
  if (port == kInputPort) {
    result.buffer_memory_constraints.min_size_bytes = zx_system_get_page_size();
    result.buffer_memory_constraints.max_size_bytes = kInputPerPacketBufferBytesMax;
  } else {
    // TODO(dayeonglee): consider requiring a larger buffer, based on what
    // seems like a minimum reasonable time duration per buffer, to keep the
    // buffers per second <= ~120.
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

void CodecAdapterCvsdDecoder::CoreCodecSetBufferCollectionInfo(
    CodecPort port, const fuchsia::sysmem::BufferCollectionInfo_2& buffer_collection_info) {
  // Nothing to do here.
}

std::pair<fuchsia::media::FormatDetails, size_t> CodecAdapterCvsdDecoder::OutputFormatDetails() {
  ZX_DEBUG_ASSERT(codec_params_);
  fuchsia::media::PcmFormat out;
  out.pcm_mode = fuchsia::media::AudioPcmMode::LINEAR;
  out.bits_per_sample = kPcmBitsPerSample;
  out.frames_per_second = kExpectedSamplingFreq;
  // For CVSD, we assume MONO channel.
  out.channel_map = {fuchsia::media::AudioChannelId::LF};

  fuchsia::media::AudioUncompressedFormat uncompressed;
  uncompressed.set_pcm(out);

  fuchsia::media::AudioFormat audio_format;
  audio_format.set_uncompressed(std::move(uncompressed));

  fuchsia::media::FormatDetails format_details;
  format_details.set_mime_type(kPcmMimeType);
  format_details.mutable_domain()->set_audio(std::move(audio_format));

  // The bytes needed to store each output packet. Since we're doing 1-16
  // decompression into PCM audio, where 1 byte of CVSD audio = 8 frames of
  // PCM audio data (i.e. 16 bytes) to each output frame, we need 16.
  return {std::move(format_details), kOutputPerPacketBufferBytesMin};
}

void CodecAdapterCvsdDecoder::CleanUpAfterStream() { codec_params_ = std::nullopt; }

void CodecAdapterCvsdDecoder::CoreCodecStopStream() {
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

void CodecAdapterCvsdDecoder::ProcessInputLoop() {
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

InputLoopStatus CodecAdapterCvsdDecoder::ProcessFormatDetails(
    const fuchsia::media::FormatDetails& format_details) {
  if (!format_details.has_mime_type() || format_details.mime_type() != kCvsdMimeType) {
    events_->onCoreCodecFailCodec(
        "CVSD Decoder received input that was not compressed cvsd audio.");
    return kShouldTerminate;
  }
  InitCodecParams(codec_params_);
  return kOk;
}

InputLoopStatus CodecAdapterCvsdDecoder::ProcessEndOfStream(CodecInputItem* item) {
  // We output whatever data is in the current output packet.
  if (output_item_ && output_item_->data_length > 0) {
    SendAndResetOutputPacket();
  }
  return kOk;
}

InputLoopStatus CodecAdapterCvsdDecoder::ProcessInputPacket(CodecPacket* packet) {
  ZX_DEBUG_ASSERT(codec_params_);

  const uint32_t input_packet_length = packet->valid_length_bytes();
  // NOTE: Potentially we could do this only if the input queue is empty, similar to how we output
  // immediately after processing a non-empty input packet only if the input queue is empty. But
  // zero-length input packets are expected to be rare anyway, so just outputting any buffered data
  // here is fine.
  if (input_packet_length == 0) {
    if (output_item_ && output_item_->data_length > 0) {
      SendAndResetOutputPacket();
    }
    // Return true since no error was reported through `events_`.
    return kOk;
  }
  uint32_t input_bytes_processed = 0;
  while (true) {
    if (!output_item_ || output_item_->packet == nullptr) {
      std::optional<CodecPacket*> maybe_output_packet = free_output_packets_.WaitForElement();
      if (!maybe_output_packet) {
        events_->onCoreCodecFailCodec("No free output packet available");
        return kShouldTerminate;
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
    ZX_DEBUG_ASSERT(output_item_->buffer->size() >= kPcmBitsPerSample);

    uint8_t* input_data = packet->buffer()->base() + packet->start_offset() + input_bytes_processed;
    input_bytes_processed += Decode(&(*codec_params_), *input_data, &(*output_item_));

    // Update current output's timestamp to the first processed packet timestamp.
    // Note that the current output buffer may be used for multiple codec input
    // item packets.
    if (input_bytes_processed > 0 && !output_item_->timestamp) {
      output_item_->timestamp.emplace(packet->timestamp_ish());
    }

    // Output buffer should be => 16 bytes in size so we can decode at least 1
    // byte of compressed CVSD audio. If the current output buffer cannot
    // process any more byte of compressed CVSD audio, we should output it.
    if (output_item_->buffer->size() - output_item_->data_length < kPcmBitsPerSample) {
      SendAndResetOutputPacket();
    }

    if (input_bytes_processed == input_packet_length) {
      break;
    }
  }

  // Output buffer if it contains some data when the queue is empty to avoid
  // long output delay.
  if (output_item_ && output_item_->data_length > 0 && !input_queue_.Signaled()) {
    SendAndResetOutputPacket();
  }
  return kOk;
}

// Encode input buffer of 1 byte to produce 16 bytes of output data.
// Also updates the `output_buffer` length when the decoding is successful and
// returns the number of input bytes that was processed (0 if not processed, 1 otherwise).
//
// TODO(dayeonglee): update so that instead of decoding a single byte, it
// decodes as much input data as possible.
uint32_t CodecAdapterCvsdDecoder::Decode(CodecParams* params, const uint8_t& input,
                                         OutputItem* output_item) {
  ZX_ASSERT(output_item->buffer->size() - output_item->data_length >= kPcmBitsPerSample);

  // Since decode does 1 bit to 16 bits decompression, we cast to int16_t
  // pointer.
  int16_t* output =
      reinterpret_cast<int16_t*>(output_item->buffer->base() + output_item->data_length);
  uint8_t mask = 0b10000000;
  for (uint32_t idx = 0; idx < 8; idx++) {
    uint8_t encoded_bit = input & mask ? 1 : 0;
    *output = SingleDecode(params, encoded_bit);
    mask >>= 1;
    output++;
  }
  output_item->data_length += kPcmBitsPerSample;
  return 1;
}

void CodecAdapterCvsdDecoder::SendAndResetOutputPacket() {
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
