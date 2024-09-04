// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_BUFFERS_IO_BUFFERS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_BUFFERS_IO_BUFFERS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_buffer.h>

#include <functional>

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <ktl/move.h>
#include <ktl/span.h>

namespace unit_testing {
bool test_vec_input_buffer();
bool test_vec_output_buffer();
}  // namespace unit_testing

namespace starnix {

using namespace starnix_uapi;

/// The callback for `OutputBuffer::write_each`. The callback is passed the buffers to write to in
/// order, and must return for each, how many bytes has been written.
using OutputBufferCallback = std::function<fit::result<Errno, size_t>(ktl::span<uint8_t>&)>;

using PeekBufferSegmentsCallback = std::function<void(const UserBuffer&)>;

/// A buffer.
///
/// Provides the common implementations for input and output buffers.
class Buffer {
 public:
  /// Returns the number of segments, if the buffer supports I/O directly
  /// to/from individual segments.
  virtual fit::result<Errno, size_t> segments_count() const = 0;

  /// Calls the callback with each segment backing this buffer.
  ///
  /// Each segment is safe to read from (if this is an `InputBuffer`) or write
  /// to (if this is an `OutputBuffer`) without causing undefined behaviour.
  virtual fit::result<Errno> peek_each_segment(PeekBufferSegmentsCallback callback) = 0;

  /// Returns all the segments backing this `Buffer`.
  ///
  /// Note that we use `IovecsRef<'_>` so that while `IovecsRef` is held,
  /// no other methods may be called on this `Buffer` since `IovecsRef`
  /// holds onto the mutable reference for this `Buffer`.
  ///
  /// Each segment is safe to read from (if this is an `InputBuffer`) or write
  /// to (if this is an `OutputBuffer`) without causing undefined behaviour.
  // virtual Result < IovecsRef <'_, syncio::zxio::iovec>, Errno> peek_all_segments_as_iovecs() = 0;

 public:
  // C++
  virtual ~Buffer() = default;
};

class InputBuffer;

/// The OutputBuffer allows for writing bytes to a buffer.
/// A single OutputBuffer will only write up to MAX_RW_COUNT bytes which is the maximum size of a
/// single operation.
class OutputBuffer : public Buffer {
 public:
  /// Calls `callback` for each segment to write data for. `callback` must returns the number of
  /// bytes actually written. When it returns less than the size of the input buffer, the write
  /// is stopped.
  ///
  /// Returns the total number of bytes written.
  virtual fit::result<Errno, size_t> write_each(OutputBufferCallback callback) = 0;

  /// Returns the number of bytes available to be written into the buffer.
  virtual size_t available() const = 0;

  /// Returns the number of bytes already written into the buffer.
  virtual size_t bytes_written() const = 0;

  /// Fills this buffer with zeros.
  virtual fit::result<Errno, size_t> zero() = 0;

  /// Advance the output buffer by `length` bytes.
  ///
  /// # Safety
  ///
  /// The caller must guarantee that the length bytes are initialized.
  virtual fit::result<Errno> advance(size_t length) = 0;

  /// Write the content of `buffer` into this buffer. If this buffer is too small, the write will
  /// be partial.
  ///
  /// Returns the number of bytes written in this buffer.
  virtual fit::result<Errno, size_t> write(const ktl::span<uint8_t>& _buffer);

  /// Write the content of `buffer` into this buffer. It is an error to pass a buffer larger than
  /// the number of bytes available in this buffer. In that case, the content of the buffer after
  /// the operation is unspecified.
  ///
  /// In case of success, always returns `buffer.len()`.
  virtual fit::result<Errno, size_t> write_all(const ktl::span<uint8_t>& buffer);

  /// Write the content of the given `InputBuffer` into this buffer. The number of bytes written
  /// will be the smallest between the number of bytes available in this buffer and in the
  /// `InputBuffer`.
  ///
  /// Returns the number of bytes read and written.
  virtual fit::result<Errno, size_t> write_buffer(InputBuffer& input);

 public:
  // C++
  virtual ~OutputBuffer() = default;
};

/// The callback for `InputBuffer::peek_each` and `InputBuffer::read_each`. The callback is passed
/// the buffers to write to in order, and must return for each, how many bytes has been read.

using InputBufferCallback = std::function<fit::result<Errno, size_t>(const ktl::span<uint8_t>&)>;

/// The InputBuffer allows for reading bytes from a buffer.
/// A single InputBuffer will only read up to MAX_RW_COUNT bytes which is the maximum size of a
/// single operation.
class InputBuffer : public Buffer {
 public:
  /// Calls `callback` for each segment to peek data from. `callback` must returns the number of
  /// bytes actually peeked. When it returns less than the size of the output buffer, the read
  /// is stopped.
  ///
  /// Returns the total number of bytes peeked.
  virtual fit::result<Errno, size_t> peek_each(InputBufferCallback callback) = 0;

  /// Returns the number of bytes available to be read from the buffer.
  virtual size_t available() const = 0;

  /// Returns the number of bytes already read from the buffer.
  virtual size_t bytes_read() const = 0;

  /// Clear the remaining content in the buffer. Returns the number of bytes swallowed. After this
  /// method returns, `available()` will returns 0. This does not touch the data in the buffer.
  virtual size_t drain() = 0;

  /// Consumes `length` bytes of data from this buffer.
  virtual fit::result<Errno> advance(size_t length) = 0;

  /// Calls `callback` for each segment to read data from. `callback` must returns the number of
  /// bytes actually read. When it returns less than the size of the output buffer, the read
  /// is stopped.
  ///
  /// Returns the total number of bytes read.
  virtual fit::result<Errno, size_t> read_each(InputBufferCallback callback) {
    auto peak_each_result = peek_each(callback);
    if (peak_each_result.is_error())
      return peak_each_result.take_error();

    auto length = peak_each_result.value();
    auto advance_result = advance(length);
    if (advance_result.is_error())
      return advance_result.take_error();

    return fit::ok(length);
  }

  virtual fit::result<Errno, fbl::Vector<uint8_t>> read_all() {
    auto peek_all_result = peek_all();
    if (peek_all_result.is_error())
      return peek_all_result.take_error();

    auto drain_result = drain();
    auto result = ktl::move(peek_all_result.value());
    ASSERT(result.size() == drain_result);
    return fit::ok(ktl::move(result));
  }

  /// Peek all the remaining content in this buffer and returns it as a `fbl::Vector`.
  virtual fit::result<Errno, fbl::Vector<uint8_t>> peek_all();

  /// Peeks the content of this buffer into `buffer`.
  /// If `buffer` is too small, the read will be partial.
  /// If `buffer` is too large, the remaining bytes will be left untouched.
  ///
  /// Returns the number of bytes read from this buffer.
  virtual fit::result<Errno, size_t> peek(ktl::span<uint8_t>& buffer) {
    auto index = 0;
    return peek_each([&](const ktl::span<uint8_t>& data) -> fit::result<Errno, size_t> {
      auto size = ktl::min(buffer.size() - index, data.size());
      auto dest = buffer.subspan(index, index + size);
      auto src = data.subspan(0, size);
      memcpy(dest.data(), src.data(), src.size());
      index += size;
      return fit::ok(size);
    });
  }

  /// Write the content of this buffer into `buffer`.
  /// If `buffer` is too small, the read will be partial.
  /// If `buffer` is too large, the remaining bytes will be left untouched.
  ///
  /// Returns the number of bytes read from this buffer.
  virtual fit::result<Errno, size_t> read(ktl::span<uint8_t>& buffer) {
    auto peek_result = peek(buffer);
    if (peek_result.is_error())
      return peek_result.take_error();

    auto length = peek_result.value();
    auto advance_result = advance(length);
    if (advance_result.is_error())
      return advance_result.take_error();

    return fit::ok(length);
  }

  /// Read the exact number of bytes required to fill buf.
  ///
  /// If `buffer` is larger than the number of available bytes, an error will be returned.
  ///
  /// In case of success, always returns `buffer.len()`.
  virtual fit::result<Errno, size_t> read_exact(ktl::span<uint8_t>& buffer) {
    auto result = read(buffer);
    if (result.is_error())
      return result.take_error();
    auto size = result.value();
    if (size != buffer.size()) {
      return fit::error(errno(EINVAL));
    } else {
      return fit::ok(size);
    }
  }

 public:
  // C++
  virtual ~InputBuffer() = default;
};

class InputBufferExt : public InputBuffer {};

/// An OutputBuffer that write data to an internal buffer.
class VecOutputBuffer : public OutputBuffer {
 private:
  fbl::Vector<uint8_t> buffer_;

  /// Used to keep track of the requested capacity. `Vec::with_capacity` may
  /// allocate more than the requested capacity so we can't rely on
  /// `Vec::capacity` to return the expected capacity.
  size_t capacity_;

 public:
  /// impl VecOutputBuffer
  static VecOutputBuffer New(size_t capacity_) { return VecOutputBuffer(capacity_); }

  const uint8_t* data() const { return buffer_.data(); }

  void reset() { buffer_.reset(); }

 private:
  /// impl From<VecOutputBuffer>
  // static VecOutputBuffer from(VecOutputBuffer data) { return VecOutputBuffer(data); }

  /// impl Buffer
  fit::result<Errno, size_t> segments_count() const final { return fit::ok(1ul); }

  fit::result<Errno> peek_each_segment(PeekBufferSegmentsCallback callback) final {
    return fit::error(errno(ENOTSUP));
  }

  /// impl OutputBuffer
  fit::result<Errno, size_t> write_each(OutputBufferCallback callback) final {
    auto current_len = buffer_.size();
    ktl::span spare_capacity{buffer_.data() + buffer_.size(), buffer_.capacity() - buffer_.size()};
    ktl::span buffer{spare_capacity.data(), capacity_ - current_len};

    auto callback_result = callback(buffer);
    if (callback_result.is_error())
      return callback_result.take_error();

    auto written = callback_result.value();
    if (current_len + written > capacity_) {
      return fit::error(errno(EINVAL));
    }
    // SAFETY: the vector is now initialized for an extra `written` bytes.
    buffer_.set_size(current_len + written);
    return fit::ok(written);
  }

  size_t available() const final { return capacity_ - buffer_.size(); }

  size_t bytes_written() const final { return buffer_.size(); }

  fit::result<Errno, size_t> zero() final {
    auto zeroed = capacity_ - buffer_.size();
    fbl::AllocChecker ac;
    buffer_.resize(capacity_, 0, &ac);
    if (!ac.check()) {
      return fit::error(errno(ENOMEM));
    }
    return fit::ok(zeroed);
  }

  fit::result<Errno> advance(size_t length) final {
    if (length > available()) {
      return fit::error(errno(EINVAL));
    }
    capacity_ -= length;
    auto current_len = buffer_.size();
    fbl::AllocChecker ac;
    buffer_.reserve(current_len + length, &ac);
    if (!ac.check()) {
      return fit::error(errno(ENOMEM));
    }
    return fit::ok();
  }

 public:
  // C++
  ~VecOutputBuffer() override { buffer_.reset(); }

 private:
  friend bool unit_testing::test_vec_output_buffer();

  VecOutputBuffer(size_t capacity) : buffer_(), capacity_(capacity) {
    fbl::AllocChecker ac;
    buffer_.reserve(capacity, &ac);
    ASSERT(ac.check());
  }
};

/// An InputBuffer that read data from an internal buffer.
class VecInputBuffer : public InputBuffer {
 private:
  fbl::Vector<uint8_t> buffer_;

  // Invariant: `bytes_read <= buffer.len()` at all times.
  size_t bytes_read_ = 0;

 public:
  // impl VecInputBuffer
  static VecInputBuffer New(const ktl::span<uint8_t>& data);

  // impl From<Vec<u8>>
  static VecInputBuffer from(fbl::Vector<uint8_t> buffer);

 private:
  /// impl Buffer
  fit::result<Errno, size_t> segments_count() const override { return fit::ok(1ul); }

  fit::result<Errno> peek_each_segment(PeekBufferSegmentsCallback callback) final {
    // std::vector<uint8_t> sliced_buffer(buffer_.begin() + bytes_read_, buffer_.end());
    // ktl::span buffer{sliced_buffer.data(), sliced_buffer.size()};
    return fit::error(errno(ENOTSUP));
  }

  /// impl InputBuffer
  fit::result<Errno, size_t> peek_each(InputBufferCallback callback) final {
    ktl::span buffer{buffer_.begin() + bytes_read_, buffer_.end()};

    auto callback_result = callback(buffer);
    auto read = callback_result.value();
    if (bytes_read_ + read > buffer_.size()) {
      return fit::error(errno(EINVAL));
    }
    DEBUG_ASSERT(bytes_read_ <= buffer_.size());
    return fit::ok(read);
  }

  fit::result<Errno> advance(size_t length) final {
    if (length > buffer_.size()) {
      return fit::error(errno(EINVAL));
    }
    bytes_read_ += length;
    DEBUG_ASSERT(bytes_read_ <= buffer_.size());
    return fit::ok();
  }

  size_t available() const final { return buffer_.size() - bytes_read_; }

  size_t bytes_read() const final { return bytes_read_; }

  size_t drain() final {
    auto result = available();
    bytes_read_ += result;
    return result;
  }

 public:
  /// impl VecInputBuffer

  /// Read an object from userspace memory and increment the read position.
  ///
  /// Returns an error if there is not enough available bytes compared to the size of `T`.
  template <typename T>
  fit::result<Errno, T> read_object() {
    auto size = sizeof(T);
    auto end = bytes_read_ + size;
    if (end > buffer_.size()) {
      return fit::error(errno(EINVAL));
    }
    // TODO (Herrera) implement T::read_from
    // std::vector<uint8_t> sliced_buffer(buffer_.begin() + bytes_read_, buffer_.begin() + end);
    // auto obj T::read_from()
    // bytes_read_ = end;
    // DEBUG_ASSERT(bytes_read_ <= buffer_.size());
    // return fit::ok(T());
    return fit::error(errno(ENOTSUP));
  }

  // C++
  ~VecInputBuffer() override { buffer_.reset(); }

 private:
  friend bool unit_testing::test_vec_input_buffer();

  explicit VecInputBuffer(fbl::Vector<uint8_t> buffer) : buffer_(ktl::move(buffer)) {}
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_BUFFERS_IO_BUFFERS_H_
