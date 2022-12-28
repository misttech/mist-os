// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_CODEC_CODECS_SW_CVSD_CODEC_ADAPTER_CVSD_ENCODER_H_
#define SRC_MEDIA_CODEC_CODECS_SW_CVSD_CODEC_ADAPTER_CVSD_ENCODER_H_

#include <lib/media/codec_impl/codec_adapter.h>

#include "chunk_input_stream.h"
#include "codec_adapter_sw.h"

class CodecAdapterCvsdEncoder : public CodecAdapterSW<fit::deferred_action<fit::closure>> {
 public:
  // See Bluetooth Core v5.3 section 9.2 CVSD CODEC.
  struct CodecParams {
    const uint32_t k;
    const uint32_t j;
    const uint32_t equal_bit_mask;
    const uint32_t historic_bit_mask;
    uint32_t historic_bits;  // Historic record of outputs bits.
    // Accumulator and step size are kept as double values to keep more
    // precisions for smaller values. Also makes the codec more
    // similar to other CVSD codec implementations.
    double accumulator;  // Referred to as x^ in the algorithm.
    double step_size;    // Referred to as delta in the algorithm.
  };

  struct OutputItem {
    CodecPacket* packet;
    const CodecBuffer* buffer;

    // Number of valid data bytes currently in the `buffer`.
    uint32_t data_length;
    // Timestamp of the last processed input item.
    std::optional<uint64_t> timestamp;
  };

  explicit CodecAdapterCvsdEncoder(std::mutex& lock, CodecAdapterEvents* codec_adapter_events);
  ~CodecAdapterCvsdEncoder() = default;

  fuchsia::sysmem::BufferCollectionConstraints CoreCodecGetBufferCollectionConstraints(
      CodecPort port, const fuchsia::media::StreamBufferConstraints& stream_buffer_constraints,
      const fuchsia::media::StreamBufferPartialSettings& partial_settings) override;

  void CoreCodecSetBufferCollectionInfo(
      CodecPort port,
      const fuchsia::sysmem::BufferCollectionInfo_2& buffer_collection_info) override;

  void CoreCodecStopStream() override;

 protected:
  void ProcessInputLoop() override;
  void CleanUpAfterStream() override;

  // Returns the format details of the output and the bytes needed to store each
  // output packet.
  std::pair<fuchsia::media::FormatDetails, size_t> OutputFormatDetails() override;

 private:
  static void PostSerial(async_dispatcher_t* dispatcher, fit::closure to_run);
  bool ProcessFormatDetails(const fuchsia::media::FormatDetails& format_details);
  bool ProcessEndOfStream(CodecInputItem* item);
  bool ProcessInputPacket(CodecPacket* packet);
  bool ProcessCodecPacket(CodecPacket* packet);
  void InitCodecParams();
  void SendOutputPacket();
  void SetOutputItem(CodecPacket* packet, const CodecBuffer* buffer);
  // Outputs and resets current output buffer.
  void SendAndResetOutputPacket();
  void InitChunkInputStream(const fuchsia::media::FormatDetails& format_details);
  // Encodes a single output byte.
  static void Encode(CodecParams* params, const uint8_t* input_data, uint8_t* output);

  std::optional<CodecParams> codec_params_;

  // Current input buffer where we buffer data from the input packet
  // before sending it over to be encoded as output. CVSD encoder processes
  // 2 bytes of data to produce 1 bit of output data. Hence, the buffer should
  // be initialized to 16 bytes size.
  std::optional<ChunkInputStream> chunk_input_stream_;

  // Current output item that we are currently encoding into.
  std::optional<OutputItem> output_item_;
};

#endif  // SRC_MEDIA_CODEC_CODECS_SW_CVSD_CODEC_ADAPTER_CVSD_ENCODER_H_
