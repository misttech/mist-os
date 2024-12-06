// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_BUFFERS_IO_BUFFERS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_BUFFERS_IO_BUFFERS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/memory_accessor.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_buffer.h>
#include <lib/mistos/util/error_propagation.h>
#include <zircon/assert.h>

#include <functional>
#include <ranges>

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
  virtual fit::result<Errno, size_t> write(ktl::span<uint8_t>& buffer);

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

  // C++
  ~OutputBuffer() override = default;
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
  virtual fit::result<Errno, size_t> read_each(InputBufferCallback&& callback) {
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
    auto index = 0u;
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
    auto result = read(buffer) _EP(result);
    auto size = result.value();
    if (size != buffer.size()) {
      return fit::error(errno(EINVAL));
    }
    return fit::ok(size);
  }

  // C++
  virtual ~InputBuffer() override = default;
};

class InputBufferExt : public InputBuffer {
 public:
  /// Reads up to `limit` bytes into a returned `Vec`.
  virtual fit::result<Errno, fbl::Vector<uint8_t>> read_to_vec_limited(size_t limit) {
    return read_to_vec<Errno, uint8_t>(
        limit, [&](ktl::span<uint8_t>& buf) -> fit::result<Errno, NumberOfElementsRead> {
          auto result = this->read(buf) _EP(result);
          return fit::ok(NumberOfElementsRead(result.value()));
        });
  }
};

/// An OutputBuffer that write data to user space memory through a `TaskMemoryAccessor`.
template <typename M>
class UserBuffersOutputBuffer : public OutputBuffer {
 public:
  static fit::result<Errno, UserBuffersOutputBuffer> new_inner(const M* mm, UserBuffers buffers) {
    auto available = UserBuffer::cap_buffers_to_max_rw_count(mm->maximum_valid_address(), buffers);
    if (available.is_error()) {
      return available.take_error();
    }

    //  Reverse the buffers as the element will be removed as they are handled.
    buffers.reverse();
    return fit::ok(
        ktl::move(UserBuffersOutputBuffer(mm, ktl::move(buffers), available.value(), 0ul)));
  }

  template <typename B, typename F>
  fit::result<Errno, size_t> write_each_inner(F&& callback) {
    auto bytes_written = 0;
    while (!buffers_.is_empty()) {
      auto buffer = buffers_.pop();
      if (!buffer.has_value()) {
        break;
      }

      if (buffer->is_null()) {
        continue;
      }

      fit::result<Errno, B> bytes = callback(buffer->length_) _EP(bytes);

      auto result = mm_->write_memory(buffer->address_, bytes.value()) _EP(result);
      bytes_written += result.value();
      auto bytes_len = bytes->size();
      auto adv_result = buffer->advance(bytes_len) _EP(adv_result);
      this->available_ -= bytes_len;
      this->bytes_written_ += bytes_len;
      if (!buffer->is_empty()) {
        buffers_.push_back(buffer.value());
        break;
      }
    }

    return fit::ok(bytes_written);
  }

  static fit::result<Errno, UserBuffersOutputBuffer> unified_new(const CurrentTask& mm,
                                                                 UserBuffers buffers) {
    return new_inner(&static_cast<const TaskMemoryAccessor&>(mm), ktl::move(buffers));
  }

  static fit::result<Errno, UserBuffersOutputBuffer> unified_new_at(const CurrentTask& mm,
                                                                    UserAddress address,
                                                                    size_t length) {
    util::SmallVector<UserBuffer, 1> input_iovec;
    input_iovec.push_back(UserBuffer{.address_ = address, .length_ = length});
    return unified_new(mm, ktl::move(input_iovec));
  }

  fit::result<Errno, size_t> segments_count() const final { return fit::ok(buffers_.size()); }

  fit::result<Errno> peek_each_segment(PeekBufferSegmentsCallback callback) final {
    // This `UserBuffersOutputBuffer` made sure that each segment only pointed
    // to valid user-space address ranges on creation so each `buffer` is
    // safe to write to.
    for (auto& buffer : std::ranges::reverse_view(buffers_)) {
      if (buffer.is_null()) {
        continue;
      }
      callback(buffer);
    }

    return fit::ok();
  }

  fit::result<Errno, size_t> write(ktl::span<uint8_t>& bytes) final {
    return write_each_inner<ktl::span<uint8_t>>(
        [&](size_t buflen) -> fit::result<Errno, ktl::span<uint8_t>> {
          auto bytes_len = ktl::min(bytes.size(), buflen);
          auto to_write = bytes.subspan(0, bytes_len);
          auto remaining = bytes.subspan(bytes_len);
          bytes = remaining;
          return fit::ok(to_write);
        });
  }

  fit::result<Errno, size_t> write_each(OutputBufferCallback callback) final {
    return write_each_inner<fbl::Vector<uint8_t>>(
        [&](size_t buflen) -> fit::result<Errno, fbl::Vector<uint8_t>> {
          return read_to_vec<Errno, uint8_t>(
              buflen, [&](ktl::span<uint8_t>& buf) -> fit::result<Errno, NumberOfElementsRead> {
                auto result = callback(buf) _EP(result);
                if (result.value() > buflen) {
                  return fit::error(errno(EINVAL));
                }
                return fit::ok(NumberOfElementsRead(result.value()));
              });
        });
  }

  size_t available() const final { return available_; }

  size_t bytes_written() const final { return bytes_written_; }

  fit::result<Errno, size_t> zero() final {
    auto bytes_written = 0;
    while (!buffers_.is_empty()) {
      auto buffer = buffers_.pop();
      if (!buffer.has_value()) {
        break;
      }

      if (buffer->is_null()) {
        continue;
      }

      auto count = mm_->zero(buffer->address_, buffer->length_) _EP(count);
      auto result = buffer->advance(count.value()) _EP(result);
      bytes_written += count.value();

      this->available_ -= count.value();
      this->bytes_written_ += count.value();

      if (!buffer->is_empty()) {
        buffers_.push_back(buffer.value());
        break;
      }
    }

    return fit::ok(bytes_written);
  }

  fit::result<Errno> advance(size_t length) final {
    if (length > available_) {
      return fit::error(errno(EINVAL));
    }

    while (!buffers_.is_empty()) {
      auto buffer = buffers_.pop();
      if (!buffer.has_value()) {
        break;
      }

      if (buffer->is_null()) {
        continue;
      }

      auto advance_by = ktl::min(length, buffer->length_);
      auto result = buffer->advance(advance_by) _EP(result);
      this->available_ -= advance_by;
      this->bytes_written_ += advance_by;
      if (!buffer->is_empty()) {
        buffers_.push_back(buffer.value());
        break;
      }
      length -= advance_by;
    }

    return fit::ok();
  }

 private:
  explicit UserBuffersOutputBuffer(const M* mm, UserBuffers buffers, size_t available,
                                   size_t bytes_written)
      : mm_(mm),
        buffers_(ktl::move(buffers)),
        available_(available),
        bytes_written_(bytes_written) {}

  const M* mm_;
  UserBuffers buffers_;
  size_t available_;
  size_t bytes_written_;
};

/// An InputBuffer that read data from user space memory through a `TaskMemoryAccessor`.
template <typename M>
class UserBuffersInputBuffer : public InputBuffer {
 private:
  const M* mm_;
  UserBuffers buffers_;
  size_t available_;
  size_t bytes_read_;

 public:
  // impl<'a, M: TaskMemoryAccessor> UserBuffersInputBuffer<'a, M>
  static fit::result<Errno, UserBuffersInputBuffer> new_inner(const M* mm, UserBuffers buffers) {
    auto available = UserBuffer::cap_buffers_to_max_rw_count(mm->maximum_valid_address(), buffers);
    if (available.is_error()) {
      return available.take_error();
    }

    //  Reverse the buffers as the element will be removed as they are handled.
    buffers.reverse();
    return fit::ok(
        ktl::move(UserBuffersInputBuffer(mm, ktl::move(buffers), available.value(), 0ul)));
  }

  template <typename F>
  fit::result<Errno, size_t> peek_each_inner(F&& callback) {
    size_t read = 0;
    for (auto& buffer : std::ranges::reverse_view(buffers_)) {
      if (buffer.is_null()) {
        continue;
      }

      auto result = callback(buffer, read);
      if (result.is_error()) {
        return result.take_error();
      }

      if (result > buffer.length_) {
        return fit::error(errno(EINVAL));
      }
      read += result.value();
      if (result != buffer.length_) {
        break;
      }
    }
    return fit::ok(read);
  }

  // impl<'a> UserBuffersInputBuffer<'a, CurrentTask>
  static fit::result<Errno, UserBuffersInputBuffer> unified_new(const CurrentTask& mm,
                                                                UserBuffers buffers) {
    return new_inner(&static_cast<const TaskMemoryAccessor&>(mm), ktl::move(buffers));
  }

  static fit::result<Errno, UserBuffersInputBuffer> unified_new_at(const CurrentTask& mm,
                                                                   UserAddress address,
                                                                   size_t length) {
    util::SmallVector<UserBuffer, 1> input_iovec;
    input_iovec.push_back(UserBuffer{.address_ = address, .length_ = length});
    return unified_new(mm, ktl::move(input_iovec));
  }

  // impl<'a> UserBuffersInputBuffer<'a, Task>

  // impl Buffer
  fit::result<Errno, size_t> segments_count() const final {
    return fit::ok(ktl::count_if(buffers_.begin(), buffers_.end(),
                                 [](const UserBuffer& b) { return b.is_null(); }));
  }

  // impl<'a, M: TaskMemoryAccessor> Buffer for UserBuffersInputBuffer<'a, M>
  fit::result<Errno> peek_each_segment(PeekBufferSegmentsCallback callback) final {
    // This `UserBuffersInputBuffer` made sure that each segment only pointed
    // to valid user-space address ranges on creation so each `buffer` is
    // safe to read from.
    for (auto& buffer : std::ranges::reverse_view(buffers_)) {
      if (buffer.is_null()) {
        continue;
      }
      callback(buffer);
    }

    return fit::ok();
  }

  // impl<'a, M: TaskMemoryAccessor> InputBuffer for UserBuffersInputBuffer<'a, M>
  fit::result<Errno, size_t> peek(ktl::span<uint8_t>& uninit_bytes) final {
    return peek_each_inner(
        [&](UserBuffer& buffer, size_t read_so_far) -> fit::result<Errno, size_t> {
          auto read_to = uninit_bytes.data() + read_so_far;
          size_t remaining_bytes = uninit_bytes.size() - read_so_far;
          auto read_count = ktl::min(buffer.length_, remaining_bytes);
          auto read_to_span = ktl::span<uint8_t>{read_to, read_count};
          auto read_bytes = mm_->read_memory(buffer.address_, read_to_span);
          if (read_bytes.is_error()) {
            return read_bytes.take_error();
          }
          ZX_DEBUG_ASSERT(read_bytes->size() == read_count);
          return fit::ok(read_count);
        });
  }

  fit::result<Errno, size_t> peek_each(InputBufferCallback callback) final {
    return peek_each_inner(
        [&](UserBuffer& buffer, size_t _read_so_far) -> fit::result<Errno, size_t> {
          auto bytes = mm_->read_memory_to_vec(buffer.address_, buffer.length_);
          if (bytes.is_error()) {
            return bytes.take_error();
          }
          return callback(bytes.value());
        });
  }

  size_t drain() final {
    auto result = available_;
    bytes_read_ += available_;
    available_ = 0;
    buffers_.clear();
    return result;
  }

  fit::result<Errno> advance(size_t length) final {
    if (length > available_) {
      return fit::error(errno(EINVAL));
    }
    available_ -= length;
    bytes_read_ += length;
    while (!buffers_.is_empty()) {
      auto buffer = buffers_.pop();
      if (!buffer.has_value()) {
        break;
      }

      if (length < buffer->length_) {
        auto result = buffer->advance(length);
        if (result.is_error()) {
          return result.take_error();
        }
        buffers_.push_back(buffer.value());
        return fit::ok();
      }
      length -= buffer->length_;
      if (length == 0) {
        return fit::ok();
      }
    }

    if (length != 0) {
      return fit::error(errno(EINVAL));
    }
    return fit::ok();
  }

  size_t available() const final { return available_; }

  size_t bytes_read() const final { return bytes_read_; }

 private:
  explicit UserBuffersInputBuffer(const M* mm, UserBuffers buffers, size_t available,
                                  size_t bytes_read)
      : mm_(mm), buffers_(ktl::move(buffers)), available_(available), bytes_read_(bytes_read) {}
};

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

  ktl::span<const uint8_t> data() const {
    return ktl::span<const uint8_t>{buffer_.data(), buffer_.size()};
  }

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

    auto callback_result = callback(buffer) _EP(callback_result);
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
    fbl::AllocChecker ac;
    if (length > available()) {
      return fit::error(errno(EINVAL));
    }

    capacity_ -= length;
    auto current_len = buffer_.size();
    buffer_.set_size(current_len + length);
    return fit::ok();
  }

 public:
  // C++
  ~VecOutputBuffer() override { buffer_.reset(); }

 private:
  friend bool unit_testing::test_vec_output_buffer();

  explicit VecOutputBuffer(size_t capacity) : capacity_(capacity) {
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
  static VecInputBuffer New(const ktl::span<const uint8_t>& data);

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

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_BUFFERS_IO_BUFFERS_H_
