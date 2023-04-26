// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_CODEC_CODECS_SW_LC3_CODEC_ADAPTER_LC3_ENCODER_H_
#define SRC_MEDIA_CODEC_CODECS_SW_LC3_CODEC_ADAPTER_LC3_ENCODER_H_

#include <lc3.h>
#include <zircon/assert.h>

#include <cstdint>
#include <memory>
#include <vector>

#include "codec_adapter_sw_impl.h"

class Lc3CodecContainer {
 public:
  explicit Lc3CodecContainer(int frame_us, int freq_hz)
      : mem_(std::make_unique<uint8_t[]>(lc3_encoder_size(frame_us, freq_hz))) {
    // Sets up and returns the pointer to the encoder struct. The pointer has the same value
    // as `mem`.
    encoder_ = lc3_setup_encoder(frame_us, freq_hz, 0, mem_.get());
  }

  lc3_encoder_t GetCodec() { return encoder_; }

  // move-only, no copy
  Lc3CodecContainer(const Lc3CodecContainer&) = delete;
  Lc3CodecContainer& operator=(const Lc3CodecContainer&) = delete;
  Lc3CodecContainer(Lc3CodecContainer&&) = default;
  Lc3CodecContainer& operator=(Lc3CodecContainer&&) = default;

 private:
  // Processes encoding for one audio channel.
  // It is the same value as `mem_.data()`. When `mem_` goes out of scope, `encoder_` is also
  // freed, so we don't need to free it explicitly.
  lc3_encoder_t encoder_;
  std::unique_ptr<uint8_t[]> mem_;
};

// See LC3 Specifications v1.0 section 2.2 Encoder Interfaces.
struct Lc3EncoderParams {
  std::vector<Lc3CodecContainer> encoders;

  const int num_channels;
  // Frame duration in microseconds.
  const int dt_us;
  // Sample rate in Hz.
  const int sr_hz;
  const int nbytes;
  // Number of bits per audio sample enc.
  const lc3_pcm_format fmt;
};

class CodecAdapterLc3Encoder : public CodecAdapterSWImpl<Lc3EncoderParams> {
 public:
  explicit CodecAdapterLc3Encoder(std::mutex& lock, CodecAdapterEvents* codec_adapter_events);
  ~CodecAdapterLc3Encoder() = default;

 protected:
  std::pair<fuchsia::media::FormatDetails, size_t> OutputFormatDetails() override;
  CodecAdapterLc3Encoder::InputLoopStatus ProcessFormatDetails(
      const fuchsia::media::FormatDetails& format_details) override;
  int ProcessInputChunkData(const uint8_t* input_data, size_t input_data_size,
                            uint8_t* output_buffer, size_t output_buffer_size) override;
  size_t InputChunkSize() override;
  size_t MinOutputBufferSize() override;
  fuchsia::sysmem::BufferCollectionConstraints BufferCollectionConstraints(
      const CodecPort port) override;
  TimestampExtrapolator CreateTimestampExtrapolator(
      const fuchsia::media::FormatDetails& format_details) override;
};

#endif  // SRC_MEDIA_CODEC_CODECS_SW_LC3_CODEC_ADAPTER_LC3_ENCODER_H_
