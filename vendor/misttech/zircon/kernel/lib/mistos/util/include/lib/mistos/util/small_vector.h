// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_SMALL_VECTOR_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_SMALL_VECTOR_H_

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <ktl/array.h>
#include <ktl/move.h>
#include <ktl/optional.h>

namespace util {

template <typename T, size_t N>
class __OWNER(T) SmallVector {
 public:
  // Standard C++ named requirements Container API.
  using value_type = T;
  using reference = T&;
  using const_reference = const T&;
  using iterator = T*;
  using const_iterator = const T*;
  using difference_type = ptrdiff_t;
  using size_type = size_t;
  using reverse_iterator = std::reverse_iterator<iterator>;
  using const_reverse_iterator = std::reverse_iterator<const_iterator>;

  // move semantics only
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(SmallVector);

  constexpr SmallVector() = default;

  SmallVector(SmallVector&& other) : size_(other.size_), on_heap_(other.on_heap_) {
    data_ = ktl::move(other.data_);
    heap_data_ = ktl::move(other.heap_data_);
  }

  size_t size() const { return size_; }

  void push_back(const T& value) {
    if (size_ < N) {
      data_[size_] = value;  // Add to stack storage
    } else {
      if (!on_heap_) {
        move_to_heap();
      }
      fbl::AllocChecker ac;
      heap_data_.push_back(value, &ac);
      ZX_DEBUG_ASSERT(ac.check());
    }
    ++size_;
  }

  // Pop the last element
  void pop_back() {
    ZX_DEBUG_ASSERT(size_ > 0);
    if (on_heap_) {
      heap_data_.pop_back();
      if (size_ - 1 <= N) {
        move_back_to_stack(size_ - 1);
      }
    }
    --size_;
  }

  // Pop the last element and return it, or return nullopt if empty
  ktl::optional<T> pop() {
    if (size_ == 0) {
      return ktl::nullopt;  // Return null if empty
    }

    ktl::optional<T> last_element;
    if (on_heap_) {
      ZX_DEBUG_ASSERT(size_ == heap_data_.size());
      last_element = heap_data_[size_ - 1];
      heap_data_.pop_back();
      if (size_ - 1 <= N) {
        move_back_to_stack(size_ - 1);
      }
    } else {
      last_element = data_[size_ - 1];
    }

    --size_;
    return last_element;
  }

  const T* data() const { return on_heap_ ? heap_data_.data() : data_.data(); }

  T* data() { return on_heap_ ? heap_data_.data() : data_.data(); }

  bool is_empty() const { return size_ == 0; }

  const T& operator[](size_t i) const {
    ZX_DEBUG_ASSERT(i < size_);
    return (on_heap_) ? heap_data_[i] : data_[i];
  }

  T& operator[](size_t i) {
    ZX_DEBUG_ASSERT(i < size_);
    return (on_heap_) ? heap_data_[i] : data_[i];
  }

  T* begin() { return (on_heap_) ? heap_data_.begin() : data_.data(); }
  const T* begin() const { return (on_heap_) ? heap_data_.begin() : data_.data(); }

  T* end() { return (on_heap_) ? heap_data_.end() : &data_[size_]; }
  const T* end() const { return (on_heap_) ? heap_data_.end() : &data_[size_]; }

  //
  // Reverse Iterators
  //

  // clang-format off

  constexpr reverse_iterator       rbegin()        noexcept { return reverse_iterator(end()); }
  constexpr const_reverse_iterator rbegin()  const noexcept { return const_reverse_iterator(end()); }
  constexpr reverse_iterator       rend()          noexcept { return reverse_iterator(begin()); }
  constexpr const_reverse_iterator rend()    const noexcept { return const_reverse_iterator(begin()); }

  // clang-format on

  void truncate(size_t new_size) {
    if (new_size < size_) {
      if (on_heap_) {
        fbl::AllocChecker ac;
        // Truncate the heap vector
        heap_data_.resize(new_size, &ac);
        ZX_DEBUG_ASSERT(ac.check());
        if (new_size <= N) {
          move_back_to_stack(new_size);
        }
      } else {
        // Truncate stack size only
        size_ = new_size;
      }
    }
    size_ = new_size;
  }

  static SmallVector<T, N> from_vec(fbl::Vector<T>&& vec) {
    SmallVector<T, N> small_vec;
    small_vec.size_ = vec.size();

    if (vec.size() <= N) {
      // Copy elements to stack storage
      for (std::size_t i = 0; i < vec.size(); ++i) {
        small_vec.data_[i] = vec[i];
      }
    } else {
      // Use heap storage
      small_vec.on_heap_ = true;
      small_vec.heap_data_ = ktl::move(vec);
    }

    return small_vec;
  }

  // Create a SmallVec from a raw buffer and length without safety checks
  static SmallVector<T, N> from_buf_and_len_unchecked(const T* buf, size_t len) {
    SmallVector<T, N> small_vec;
    small_vec.size_ = len;

    if (len <= N) {
      // Copy to stack storage
      memcpy(small_vec.data_.data(), buf, len * sizeof(T));
    } else {
      // Copy to heap storage
      fbl::AllocChecker ac;
      fbl::Vector<T> tmp;
      tmp.reserve(len, &ac);
      ZX_DEBUG_ASSERT(ac.check());
      memcpy(tmp.data(), buf, len * sizeof(T));
      tmp.set_size(len);

      small_vec.heap_data_.swap(tmp);
      small_vec.on_heap_ = true;
    }
    return small_vec;
  }

  // Clear the vector
  void clear() {
    if (on_heap_) {
      heap_data_.reset();  // Deallocate heap memory
      on_heap_ = false;    // Reset back to stack storage
    }
    size_ = 0;  // Reset size to 0
  }

  // Reverse the elements in the vector
  void reverse() {
    if (on_heap_) {
      ktl::reverse(heap_data_.begin(), heap_data_.end());
    } else {
      ktl::reverse(data_.begin(), data_.begin() + size_);
    }
  }

 private:
  ktl::array<T, N> data_;     // Stack storage
  fbl::Vector<T> heap_data_;  // Heap storage
  size_t size_ = 0;
  bool on_heap_ = false;

  // Move elements from stack to heap when exceeding capacity
  void move_to_heap() {
    fbl::AllocChecker ac;
    heap_data_.reserve(N, &ac);
    ZX_DEBUG_ASSERT(ac.check());
    for (size_t i = 0; i < N; ++i) {
      heap_data_.push_back(data_[i], &ac);  // Move to heap storage
      ZX_DEBUG_ASSERT(ac.check());
    }
    on_heap_ = true;
  }

  // Move elements back to stack if heap size shrinks below capacity
  void move_back_to_stack(size_t new_size) {
    for (size_t i = 0; i < new_size; ++i) {
      data_[i] = heap_data_[i];
    }
    heap_data_.reset();
    on_heap_ = false;
  }
};

}  // namespace util

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_SMALL_VECTOR_H_
