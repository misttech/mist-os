// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_OBJECT_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_OBJECT_H_

#include <lib/mistos/zx/object_traits.h>
#include <lib/mistos/zx/time.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <utility>

#include <object/handle.h>

namespace zx {

class port;
class profile;

class Value : public fbl::RefCounted<Value> {
 public:
  Value() : value_(nullptr), handle_(nullptr) {}

  explicit Value(HandleOwner handle) : value_(handle.get()), handle_(ktl::move(handle)) {}

  explicit Value(Handle* handle) : value_(handle) {}

  Value(Value&& other) : value_(other.value_), handle_(ktl::move(other.handle_)) {
    other.value_ = nullptr;
  }

  Value& operator=(Value&& other) {
    if (this != &other) {
      value_ = other.value_;
      handle_ = ktl::move(other.handle_);
      other.value_ = nullptr;
    }
    return *this;
  }

  bool is_valid() const { return value_ != nullptr; }

  Handle* get() const { return value_; }

  void close() {
    if (value_ != nullptr) {
      if (handle_.get() != nullptr) {
        ZX_ASSERT(value_ == handle_.get());
        handle_.reset();
      }
      value_ = nullptr;
    }
  }

  ~Value() { close(); }

  const Handle& operator*() const { return *value_; }
  const Handle* operator->() const { return value_; }

 private:
  Handle* value_;
  HandleOwner handle_;
};

class object_base {
 public:
  void reset(fbl::RefPtr<Value> value = nullptr) {
    close();
    value_ = value;
  }

  bool is_valid() const { return value_ && value_->is_valid(); }
  explicit operator bool() const { return is_valid(); }

  fbl::RefPtr<Value> get() const { return value_; }

  // Reset the underlying handle, and then get the address of the
  // underlying internal handle storage.
  //
  // Note: The intended purpose is to facilitate interactions with C
  // APIs which expect to be provided a pointer to a handle used as
  // an out parameter.
  fbl::RefPtr<Value>* reset_and_get_address() {
    reset();
    return &value_;
  }

  __attribute__((warn_unused_result)) fbl::RefPtr<Value> release() {
    fbl::RefPtr<Value> result = value_;
    value_ = nullptr;
    return result;
  }

  zx_status_t get_info(uint32_t topic, void* buffer, size_t buffer_size, size_t* actual_count,
                       size_t* avail_count) const;
  zx_status_t get_property(uint32_t property, void* _value, size_t size) const;
  zx_status_t set_property(uint32_t property, const void* _value, size_t size) const;

 protected:
  constexpr object_base() : value_(nullptr) {}

  explicit object_base(fbl::RefPtr<Value> value) : value_(std::move(value)) {}

  ~object_base() { close(); }

  object_base(const object_base&) = delete;

  void operator=(const object_base&) = delete;

  void close() {
    if (value_ != nullptr) {
      value_->close();
      value_ = nullptr;
    }
  }

  fbl::RefPtr<Value> value_;
};

// Forward declaration for borrow method.
template <typename T>
class unowned;

// Provides type-safe access to operations on a handle.
template <typename T>
class object : public object_base {
 public:
  static constexpr zx_obj_type_t TYPE = ZX_OBJ_TYPE_NONE;

  constexpr object() = default;

  explicit object(fbl::RefPtr<Value> value) : object_base(value) {}

  template <typename U>
  object(object<U>&& other) : object_base(other.release()) {
    static_assert(is_same<T, void>::value, "Receiver must be compatible.");
  }

  template <typename U>
  object<T>& operator=(object<U>&& other) {
    static_assert(is_same<T, void>::value, "Receiver must be compatible.");
    reset(other.release());
    return *this;
  }

  void swap(object<T>& other) { value_.swap(other.value_); }

  zx_status_t duplicate(zx_rights_t rights, object<T>* result) const {
    static_assert(object_traits<T>::supports_duplication, "Object must support duplication.");
    if (!value_)
      return ZX_ERR_BAD_HANDLE;

    auto current_handle = value_->get();
    if (!current_handle)
      return ZX_ERR_BAD_HANDLE;

    if (!current_handle->HasRights(ZX_RIGHT_DUPLICATE))
      return ZX_ERR_ACCESS_DENIED;
    if (rights == ZX_RIGHT_SAME_RIGHTS) {
      rights = current_handle->rights();
    } else if ((current_handle->rights() & rights) != rights) {
      return ZX_ERR_INVALID_ARGS;
    }

    HandleOwner h = Handle::Dup(current_handle, rights);
    if (!h) {
      return ZX_ERR_NO_MEMORY;
    }
    fbl::AllocChecker ac;
    auto value = fbl::MakeRefCountedChecked<zx::Value>(&ac, ktl::move(h));
    if (!ac.check()) {
      return ZX_ERR_NO_MEMORY;
    }

    result->reset(value);
    return ZX_OK;
  }

  zx_status_t replace(zx_rights_t rights, object<T>* result) {
    if (!value_)
      return ZX_ERR_BAD_HANDLE;

    auto current_handle = value_->get();
    if (!current_handle)
      return ZX_ERR_BAD_HANDLE;

    if (rights == ZX_RIGHT_SAME_RIGHTS) {
      rights = current_handle->rights();
    } else if ((current_handle->rights() & rights) != rights) {
      // handle_owner_.reset();
      return ZX_ERR_INVALID_ARGS;
    }

    HandleOwner h = Handle::Dup(current_handle, rights);
    if (!h) {
      return ZX_ERR_NO_MEMORY;
    }

    fbl::AllocChecker ac;
    auto value = fbl::MakeRefCountedChecked<zx::Value>(&ac, ktl::move(h));
    if (!ac.check()) {
      return ZX_ERR_NO_MEMORY;
    }

    value_ = nullptr;
    result->reset(value);
    return ZX_OK;
  }

  zx_status_t wait_one(zx_signals_t signals, zx::time deadline, zx_signals_t* pending) const {
    static_assert(object_traits<T>::supports_wait, "Object is not waitable.");
    // return zx_object_wait_one(value_, signals, deadline.get(), pending);
    return ZX_ERR_UNAVAILABLE;
  }

  zx_status_t get_child(uint64_t koid, zx_rights_t rights, object<void>* result) const {
    static_assert(object_traits<T>::supports_get_child, "Object must support getting children.");
    // Allow for |result| and |this| being the same container, though that
    // can only happen for |T=void|, due to strict aliasing.
    // object<void> h;
    // zx_status_t status = zx_object_get_child(value_, koid, rights, h.reset_and_get_address());
    // result->reset(h.release());
    // return status;
    return ZX_ERR_UNAVAILABLE;
  }

  // Returns a type-safe wrapper of the underlying handle that does not claim ownership.
  unowned<T> borrow() const { return unowned<T>(get()); }

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
bool operator==(const fbl::RefPtr<Value>& a, const object<T>& b) {
  return a == b.get();
}

template <typename T>
bool operator!=(const fbl::RefPtr<Value>& a, const object<T>& b) {
  return !(a == b);
}

template <typename T>
bool operator<(const fbl::RefPtr<Value>& a, const object<T>& b) {
  return a < b.get();
}

template <typename T>
bool operator>(const fbl::RefPtr<Value>& a, const object<T>& b) {
  return a > b.get();
}

template <typename T>
bool operator<=(const fbl::RefPtr<Value>& a, const object<T>& b) {
  return !(a > b.get());
}

template <typename T>
bool operator>=(const fbl::RefPtr<Value>& a, const object<T>& b) {
  return !(a < b.get());
}

template <typename T>
bool operator==(const object<T>& a, const fbl::RefPtr<Value>& b) {
  return a.get() == b;
}

template <typename T>
bool operator!=(const object<T>& a, const fbl::RefPtr<Value>& b) {
  return !(a == b);
}

template <typename T>
bool operator<(const object<T>& a, const fbl::RefPtr<Value>& b) {
  return a.get() < b;
}

template <typename T>
bool operator>(const object<T>& a, const fbl::RefPtr<Value>& b) {
  return a.get() > b;
}

template <typename T>
bool operator<=(const object<T>& a, const fbl::RefPtr<Value>& b) {
  return !(a.get() > b);
}

template <typename T>
bool operator>=(const object<T>& a, const fbl::RefPtr<Value>& b) {
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
  explicit unowned(fbl::RefPtr<Value> h) : value_(h) { ZX_ASSERT(h); }
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
    value_ = static_cast<T&&>(other.value_);
    return *this;
  }

  const T& operator*() const { return value_; }
  const T* operator->() const { return &value_; }

 private:
  void release_value() {
    fbl::RefPtr<Value> h = value_.release();
    static_cast<void>(h);
  }

  T value_;
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
bool operator==(const fbl::RefPtr<Value>& a, const unowned<T>& b) {
  return a == b->get();
}

template <typename T>
bool operator!=(const fbl::RefPtr<Value>& a, const unowned<T>& b) {
  return !(a == b);
}

template <typename T>
bool operator<(const fbl::RefPtr<Value>& a, const unowned<T>& b) {
  return a < b->get();
}

template <typename T>
bool operator>(const fbl::RefPtr<Value>& a, const unowned<T>& b) {
  return a > b->get();
}

template <typename T>
bool operator<=(const fbl::RefPtr<Value>& a, const unowned<T>& b) {
  return !(a > b);
}

template <typename T>
bool operator>=(const fbl::RefPtr<Value>& a, const unowned<T>& b) {
  return !(a < b);
}

template <typename T>
bool operator==(const unowned<T>& a, const fbl::RefPtr<Value>& b) {
  return a->get() == b;
}

template <typename T>
bool operator!=(const unowned<T>& a, const fbl::RefPtr<Value>& b) {
  return !(a == b);
}

template <typename T>
bool operator<(const unowned<T>& a, const fbl::RefPtr<Value>& b) {
  return a->get() < b;
}

template <typename T>
bool operator>(const unowned<T>& a, const fbl::RefPtr<Value>& b) {
  return a->get() > b;
}

template <typename T>
bool operator<=(const unowned<T>& a, const fbl::RefPtr<Value>& b) {
  return !(a > b);
}

template <typename T>
bool operator>=(const unowned<T>& a, const fbl::RefPtr<Value>& b) {
  return !(a < b);
}

bool operator<(const fbl::RefPtr<Value>& a, const fbl::RefPtr<Value>& b);

}  // namespace zx

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_OBJECT_H_
