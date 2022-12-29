// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_CODEC_CODECS_SW_CVSD_CODEC_ADAPTER_CVSD_DECODER_H_
#define SRC_MEDIA_CODEC_CODECS_SW_CVSD_CODEC_ADAPTER_CVSD_DECODER_H_

#include <lib/fit/defer.h>
#include <lib/media/codec_impl/codec_adapter.h>

#include "codec_adapter_cvsd.h"
#include "codec_adapter_sw.h"

class CodecAdapterCvsdDecoder : public CodecAdapterSW<fit::deferred_action<fit::closure>> {
 public:
  explicit CodecAdapterCvsdDecoder(std::mutex& lock, CodecAdapterEvents* codec_adapter_events);

  ~CodecAdapterCvsdDecoder() = default;

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
  InputLoopStatus ProcessFormatDetails(const fuchsia::media::FormatDetails& format_details);
  InputLoopStatus ProcessEndOfStream(CodecInputItem* item);
  InputLoopStatus ProcessInputPacket(CodecPacket* packet);
  void SendAndResetOutputPacket();

  // TODO(dayeonglee): consider calling with a number of bytes and looping within the
  // method, indicating how much of the input buffer is consumed.
  // Decodes a single input byte.
  static uint32_t Decode(CodecParams* params, const uint8_t& input, OutputItem* output_item);

  std::optional<CodecParams> codec_params_;

  // Current output item that we are currently encoding into.
  std::optional<OutputItem> output_item_;
};

#endif  // SRC_MEDIA_CODEC_CODECS_SW_CVSD_CODEC_ADAPTER_CVSD_DECODER_H_
