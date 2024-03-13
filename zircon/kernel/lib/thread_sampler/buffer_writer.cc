// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/thread_sampler/buffer_writer.h>

#include <cstdint>

namespace sampler {
void internal::AllocatedRecord::WriteWord(uint64_t word) {
  DEBUG_ASSERT(!buffer_.empty());
  buffer_[0] = word;
  buffer_ = buffer_.subspan(1);
}

void internal::AllocatedRecord::WriteBytes(const void* buffer, size_t num_bytes) {
  size_t num_words = fxt::WordSize::FromBytes(num_bytes).SizeInWords();
  DEBUG_ASSERT(buffer_.size() >= num_words);

  // Zero out the last word to ensure any padding bytes are 0
  buffer_[num_words - 1] = 0;
  memcpy(buffer_.data(), buffer, num_bytes);
  buffer_ = buffer_.subspan(num_words);
}

void internal::AllocatedRecord::Commit() { DEBUG_ASSERT(buffer_.empty()); }

zx::result<internal::AllocatedRecord> internal::BufferWriter::Reserve(uint64_t header) {
  fxt::WordSize record_words{fxt::RecordFields::RecordSize::Get<size_t>(header)};
  if ((ptr_ + record_words.SizeInBytes()) > end_) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  AllocatedRecord rec{reinterpret_cast<uint64_t*>(ptr_), record_words, buffer_mapping_};
  rec.WriteWord(header);
  ptr_ += record_words.SizeInBytes();
  return zx::ok(ktl::move(rec));
}
}  // namespace sampler
