// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_CODEC_CODECS_SW_LC3_CODEC_ADAPTER_LC3_H_
#define SRC_MEDIA_CODEC_CODECS_SW_LC3_CODEC_ADAPTER_LC3_H_

#include <functional>

// LC3 Specification v1.0 section 2.2 Encoder Interfaces.
static constexpr uint16_t kMinExternalByteCount = 20;
static constexpr uint16_t kMaxExternalByteCount = 400;

static constexpr char kLc3MimeType[] = "audio/lc3";

// This is an arbitrary cap for now.
static constexpr uint32_t kInputPerPacketBufferBytesMax = 4 * 1024 * 1024;

template <typename T>
class Lc3CodecContainer {
 public:
  explicit Lc3CodecContainer(std::function<T(void*)> creator_func, const size_t processor_size)
      : mem_(std::make_unique<uint8_t[]>(processor_size)) {
    processor_ = creator_func(mem_.get());
  }

  T GetCodec() { return processor_; }

  // move-only, no copy
  Lc3CodecContainer(const Lc3CodecContainer&) = delete;
  Lc3CodecContainer& operator=(const Lc3CodecContainer&) = delete;
  Lc3CodecContainer(Lc3CodecContainer&&) noexcept = default;
  Lc3CodecContainer& operator=(Lc3CodecContainer&&) noexcept = default;

 private:
  // Processes encoding/decoding for one audio channel.
  // It is the same value as `mem_.data()`. When `mem_` goes out of scope, `processor_` is also
  // freed, so we don't need to free it explicitly.
  T processor_;
  std::unique_ptr<uint8_t[]> mem_;
};

#endif  // SRC_MEDIA_CODEC_CODECS_SW_LC3_CODEC_ADAPTER_LC3_H_
