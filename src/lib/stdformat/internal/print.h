// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_STDFORMAT_INTERNAL_PRINT_H_
#define SRC_LIB_STDFORMAT_INTERNAL_PRINT_H_

#include <cstdio>
#include <format>
#include <iterator>
#include <span>

namespace cpp23::internal {

class stream_output_iterator {
 public:
  using iterator_category = std::output_iterator_tag;
  using value_type = char;
  using difference_type = std::ptrdiff_t;
  using pointer = char*;
  using reference = char&;

  explicit stream_output_iterator(std::FILE* stream, std::span<char> buffer)
      : stream_(stream), buffer_(buffer) {}

  stream_output_iterator(stream_output_iterator&& other) noexcept
      : stream_(other.stream_), buffer_(other.buffer_), index_(other.index_) {}
  stream_output_iterator& operator=(stream_output_iterator&& other) noexcept {
    stream_ = other.stream_;
    buffer_ = other.buffer_;
    index_ = other.index_;
    return *this;
  }
  stream_output_iterator(const stream_output_iterator& other) noexcept = default;
  stream_output_iterator& operator=(const stream_output_iterator& other) noexcept = delete;

  stream_output_iterator& operator=(const char& ch) {
    buffer_[index_] = ch;
    return *this;
  }

  reference operator*() { return buffer_[index_]; }

  stream_output_iterator& operator++() {
    index_++;
    if (index_ == buffer_.size()) {
      fwrite(buffer_.data(), 1, buffer_.size(), stream_);
      index_ = 0;
    }
    return *this;
  }

  stream_output_iterator operator++(int) {
    auto tmp = *this;
    ++*this;
    return tmp;
  }

  size_t index() const { return index_; }

 private:
  std::FILE* stream_;
  std::span<char> buffer_;
  size_t index_ = 0;
};

// This function is neither vprint_unicode nor vprint_unicode_buffered exactly, and thus not being
// made public. The former relies on stdio for any buffering. The latter guarantees exactly one
// stdio write per call. This one splits the difference by doing as few stdio writes as possible
// with the chosen buffer size while not requiring dynamic allocation.
inline void vprint(std::FILE* stream, std::string_view fmt, std::format_args args) {
  constexpr size_t kBufferSize = 256;
  std::array<char, kBufferSize> buffer = {};
  size_t index = std::vformat_to(stream_output_iterator(stream, buffer), fmt, args).index();
  if (index > 0) {
    fwrite(buffer.data(), 1, index, stream);
  }
}

}  // namespace cpp23::internal

#endif  // SRC_LIB_STDFORMAT_INTERNAL_PRINT_H_
