// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_OBJECT_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_OBJECT_H_

#include <lib/mistos/zx/object_traits.h>
#include <lib/mistos/zx/time.h>
#include <zircon/errors.h>
#include <zircon/types.h>

namespace zx {

// Copies a single record, |src_record|, into the user buffer |dst_buffer| of size
// |dst_buffer_size|.
//
// If the copy succeeds, the value 1 is copied into |user_avail| and |user_actual| (if non-null).
//
// If the copy fails because the buffer it too small, |user_avail| and |user_actual| will receive
// the values 1 and 0 respectively (if non-null).
template <typename T>
zx_status_t single_record_result(void* dst_buffer, size_t dst_buffer_size, size_t* user_actual,
                                 size_t* user_avail, const T& src_record) {
  size_t avail = 1;
  size_t actual;
  if (dst_buffer_size >= sizeof(T)) {
    memcpy(dst_buffer, &src_record, sizeof(T));
    actual = 1;
  } else {
    actual = 0;
  }
  if (user_actual) {
    *user_actual = actual;
  }
  if (user_avail) {
    *user_avail = avail;
  }
  if (actual == 0)
    return ZX_ERR_BUFFER_TOO_SMALL;
  return ZX_OK;
}

template <typename T>
class object_base {
 public:
  void reset(T value = nullptr) {
    close();
    value_ = value;
  }

  bool is_valid() const { return value_ != nullptr; }
  explicit operator bool() const { return is_valid(); }

  const T& get() const { return value_; }

  // Reset the underlying handle, and then get the address of the
  // underlying internal handle storage.
  //
  // Note: The intended purpose is to facilitate interactions with C
  // APIs which expect to be provided a pointer to a handle used as
  // an out parameter.
  T* reset_and_get_address() {
    reset();
    return &value_;
  }

  __attribute__((warn_unused_result)) T release() {
    T result = value_;
    value_ = nullptr;
    return result;
  }

  virtual zx_status_t get_info(uint32_t topic, void* buffer, size_t buffer_size,
                               size_t* actual_count, size_t* avail_count) const {
    return ZX_ERR_NOT_SUPPORTED;
  }

  virtual zx_status_t get_property(uint32_t property, void* value, size_t size) const {
    return ZX_ERR_NOT_SUPPORTED;
  }

  virtual zx_status_t set_property(uint32_t property, const void* value, size_t size) const {
    return ZX_ERR_NOT_SUPPORTED;
  }

 protected:
  constexpr object_base() : value_(nullptr) {}

  explicit object_base(T value) : value_(value) {}

  ~object_base() { close(); }

  object_base(const object_base&) = delete;

  void operator=(const object_base&) = delete;

  void close() {
    if (value_ != nullptr) {
      value_ = nullptr;
    }
  }

  T value_;
};

// Forward declaration for borrow method.
template <typename T>
class unowned;

// Provides type-safe access to operations on a handle.
template <typename T>
class object : public object_base<typename object_traits<T>::StorageType> {
 public:
  using StorageType = typename object_traits<T>::StorageType;

  static constexpr zx_obj_type_t TYPE = ZX_OBJ_TYPE_NONE;

  constexpr object() = default;

  explicit object(StorageType value) : object_base<StorageType>(value) {}

  template <typename U>
  object(object<U>&& other) : object_base<StorageType>(other.release()) {
    static_assert(is_same<T, void>::value, "Receiver must be compatible.");
  }

  template <typename U>
  object<T>& operator=(object<U>&& other) {
    static_assert(is_same<T, void>::value, "Receiver must be compatible.");
    reset(other.release());
    return *this;
  }

  void swap(object<T>& other) {
    StorageType tmp = this->value_;
    this->value_ = other.value_;
    other.value_ = tmp;
  }

  zx_status_t duplicate(zx_rights_t rights, object<T>* result) const {
    static_assert(object_traits<T>::supports_duplication, "Object must support duplication.");
    result->reset(this->value_);
    return ZX_OK;
  }

  zx_status_t wait_one(zx_signals_t signals, zx::time deadline, zx_signals_t* pending) const;

  zx_status_t get_child(uint64_t koid, zx_rights_t rights, object<void>* result) const {
    static_assert(object_traits<T>::supports_get_child, "Object must support getting children.");
    object<void> h;
    result->reset(h.release());
    return ZX_OK;
  }

  // Returns a type-safe wrapper of the underlying handle that does not claim ownership.
  unowned<T> borrow() const { return unowned<T>(object_base<StorageType>::get()); }

 private:
  template <typename A, typename B>
  struct is_same {
    static const bool value = false;
  };

  template <typename A>
  struct is_same<A, A> {
    static const bool value = true;
  };
};

template <typename T>
bool operator==(const object<T>& a, const object<T>& b) {
  return a.get() == b.get();
}

template <typename T>
bool operator!=(const object<T>& a, const object<T>& b) {
  return !(a == b);
}

template <typename T>
bool operator<(const object<T>& a, const object<T>& b) {
  return a.get() < b.get();
}

template <typename T>
bool operator>(const object<T>& a, const object<T>& b) {
  return a.get() > b.get();
}

template <typename T>
bool operator<=(const object<T>& a, const object<T>& b) {
  return !(a.get() > b.get());
}

template <typename T>
bool operator>=(const object<T>& a, const object<T>& b) {
  return !(a.get() < b.get());
}

template <typename T>
bool operator==(typename object_traits<T>::StorageType a, const object<T>& b) {
  return a == b.get();
}

template <typename T>
bool operator!=(typename object_traits<T>::StorageType a, const object<T>& b) {
  return !(a == b);
}

template <typename T>
bool operator<(typename object_traits<T>::StorageType a, const object<T>& b) {
  return a < b.get();
}

template <typename T>
bool operator>(typename object_traits<T>::StorageType a, const object<T>& b) {
  return a > b.get();
}

template <typename T>
bool operator<=(typename object_traits<T>::StorageType a, const object<T>& b) {
  return !(a > b.get());
}

template <typename T>
bool operator>=(typename object_traits<T>::StorageType a, const object<T>& b) {
  return !(a < b.get());
}

template <typename T>
bool operator==(const object<T>& a, typename object_traits<T>::StorageType b) {
  return a.get() == b;
}

template <typename T>
bool operator!=(const object<T>& a, typename object_traits<T>::StorageType b) {
  return !(a == b);
}

template <typename T>
bool operator<(const object<T>& a, typename object_traits<T>::StorageType b) {
  return a.get() < b;
}

template <typename T>
bool operator>(const object<T>& a, typename object_traits<T>::StorageType b) {
  return a.get() > b;
}

template <typename T>
bool operator<=(const object<T>& a, typename object_traits<T>::StorageType b) {
  return !(a.get() > b);
}

template <typename T>
bool operator>=(const object<T>& a, typename object_traits<T>::StorageType b) {
  return !(a.get() < b);
}

// Wraps a handle to an object to provide type-safe access to its operations
// but does not take ownership of it.  The handle is not closed when the
// wrapper is destroyed.
//
// All use of unowned<object<T>> as an object<T> is via a dereference operator,
// as illustrated below:
//
// void do_something(const zx::event& event);
//
// void example(zx_handle_t event_handle) {
//     do_something(*zx::unowned<event>(event_handle));
// }
//
// Convenience aliases are provided for all object types, for example:
//
// zx::unowned_event(handle)->signal(..)
template <typename T>
class unowned final {
 public:
  using StorageType = typename object_traits<T>::StorageType;
  explicit unowned(StorageType h) : object_(h) {}
  explicit unowned(const T& owner) : unowned(owner.get()) {}
  explicit unowned(const unowned& other) : unowned(*other) {}
  constexpr unowned() = default;
  unowned(unowned&& other) = default;

  ~unowned() { release_value(); }

  unowned& operator=(const unowned& other) {
    if (&other == this) {
      return *this;
    }

    *this = unowned(other);
    return *this;
  }
  unowned& operator=(unowned&& other) {
    release_value();
    object_ = static_cast<T&&>(other.object_);
    return *this;
  }

  const T& operator*() const { return object_; }
  const T* operator->() const { return &object_; }

 private:
  void release_value() {
    StorageType h = object_.release();
    static_cast<void>(h);
  }

  T object_;
};

template <typename T>
bool operator==(const unowned<T>& a, const unowned<T>& b) {
  return a->get() == b->get();
}

template <typename T>
bool operator!=(const unowned<T>& a, const unowned<T>& b) {
  return !(a == b);
}

template <typename T>
bool operator<(const unowned<T>& a, const unowned<T>& b) {
  return a->get() < b->get();
}

template <typename T>
bool operator>(const unowned<T>& a, const unowned<T>& b) {
  return a->get() > b->get();
}

template <typename T>
bool operator<=(const unowned<T>& a, const unowned<T>& b) {
  return !(a > b);
}

template <typename T>
bool operator>=(const unowned<T>& a, const unowned<T>& b) {
  return !(a < b);
}

template <typename T>
bool operator==(typename object_traits<T>::StorageType a, const unowned<T>& b) {
  return a == b->get();
}

template <typename T>
bool operator!=(typename object_traits<T>::StorageType a, const unowned<T>& b) {
  return !(a == b);
}

template <typename T>
bool operator<(typename object_traits<T>::StorageType a, const unowned<T>& b) {
  return a < b->get();
}

template <typename T>
bool operator>(typename object_traits<T>::StorageType a, const unowned<T>& b) {
  return a > b->get();
}

template <typename T>
bool operator<=(typename object_traits<T>::StorageType a, const unowned<T>& b) {
  return !(a > b);
}

template <typename T>
bool operator>=(typename object_traits<T>::StorageType a, const unowned<T>& b) {
  return !(a < b);
}

template <typename T>
bool operator==(const unowned<T>& a, typename object_traits<T>::StorageType b) {
  return a->get() == b;
}

template <typename T>
bool operator!=(const unowned<T>& a, typename object_traits<T>::StorageType b) {
  return !(a == b);
}

template <typename T>
bool operator<(const unowned<T>& a, typename object_traits<T>::StorageType b) {
  return a->get() < b;
}

template <typename T>
bool operator>(const unowned<T>& a, typename object_traits<T>::StorageType b) {
  return a->get() > b;
}

template <typename T>
bool operator<=(const unowned<T>& a, typename object_traits<T>::StorageType b) {
  return !(a > b);
}

template <typename T>
bool operator>=(const unowned<T>& a, typename object_traits<T>::StorageType b) {
  return !(a < b);
}

}  // namespace zx

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_OBJECT_H_
