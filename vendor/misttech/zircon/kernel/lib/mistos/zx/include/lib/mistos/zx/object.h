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

#include <object/handle.h>

namespace zx {

class port;
class profile;

class object_base {
 public:
  void reset(HandleOwner handle_owner = nullptr) {
    close();
    handle_owner_.reset(handle_owner.release());
    handle_ = handle_owner_.get();
  }

  bool is_valid() const { return handle_ != nullptr; }
  explicit operator bool() const { return is_valid(); }

  Handle* get() const { return handle_; }

  // Reset the underlying handle, and then get the address of the
  // underlying internal handle storage.
  //
  // Note: The intended purpose is to facilitate interactions with C
  // APIs which expect to be provided a pointer to a handle used as
  // an out parameter.
  HandleOwner* reset_and_get_address() {
    reset();
    return &handle_owner_;
  }

  __attribute__((warn_unused_result)) HandleOwner release() {
    HandleOwner result = ktl::move(handle_owner_);
    handle_ = nullptr;
    return ktl::move(result);
  }

  zx_status_t get_info(uint32_t topic, void* buffer, size_t buffer_size, size_t* actual_count,
                       size_t* avail_count) const;
  zx_status_t get_property(uint32_t property, void* _value, size_t size) const;
  zx_status_t set_property(uint32_t property, const void* _value, size_t size) const;

 protected:
  constexpr object_base() : handle_(nullptr) {}

  explicit object_base(Handle* handle) : handle_(handle) {}

  explicit object_base(HandleOwner handle_owner)
      : handle_owner_(ktl::move(handle_owner)), handle_(handle_owner_.get()) {}

  ~object_base() { close(); }

  object_base(const object_base&) = delete;

  void operator=(const object_base&) = delete;

  void close() {
    if (handle_owner_ != nullptr) {
      handle_owner_.release();
      handle_owner_ = nullptr;
      handle_ = nullptr;
    }
  }

  HandleOwner handle_owner_;
  Handle* handle_;
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

  explicit object(HandleOwner handle_owner) : object_base(ktl::move(handle_owner)) {}

  explicit object(Handle* handle) : object_base(handle) {}

  template <typename U>
  object(object<U>&& other) : object_base(other.release()) {
    static_assert(is_same<T, void>::value, "Receiver must be compatible.");
  }

  template <typename U>
  object<T>& operator=(object<U>&& other) {
    static_assert(is_same<T, void>::value, "Receiver must be compatible.");
    reset(other.release());
    other.unonwned_handle_ = nullptr;
    return *this;
  }

  void swap(object<T>& other) {
    HandleOwner tmp = handle_owner_;
    handle_owner_ = other.handle_owner_;
    other.handle_owner_ = tmp;
  }

  zx_status_t duplicate(zx_rights_t rights, object<T>* result) const {
    static_assert(object_traits<T>::supports_duplication, "Object must support duplication.");
    if (!handle_)
      return ZX_ERR_BAD_HANDLE;
    if (!handle_->HasRights(ZX_RIGHT_DUPLICATE))
      return ZX_ERR_ACCESS_DENIED;
    if (rights == ZX_RIGHT_SAME_RIGHTS) {
      rights = handle_->rights();
    } else if ((handle_->rights() & rights) != rights) {
      return ZX_ERR_INVALID_ARGS;
    }

    HandleOwner handle = Handle::Dup(handle_, rights);
    if (!handle) {
      return ZX_ERR_NO_MEMORY;
    }
    result->reset(ktl::move(handle));
    return ZX_OK;
  }

  zx_status_t replace(zx_rights_t rights, object<T>* result) {
    if (!handle_)
      return ZX_ERR_BAD_HANDLE;
    if (rights == ZX_RIGHT_SAME_RIGHTS) {
      rights = handle_->rights();
    } else if ((handle_->rights() & rights) != rights) {
      handle_owner_.reset();
      return ZX_ERR_INVALID_ARGS;
    }

    HandleOwner handle = Handle::Dup(handle_, rights);
    if (!handle) {
      return ZX_ERR_NO_MEMORY;
    }
    handle_owner_.reset();
    result->reset(ktl::move(handle));
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
bool operator==(Handle* a, const object<T>& b) {
  return a == b.get();
}

template <typename T>
bool operator!=(Handle* a, const object<T>& b) {
  return !(a == b);
}

template <typename T>
bool operator<(Handle* a, const object<T>& b) {
  return a < b.get();
}

template <typename T>
bool operator>(Handle* a, const object<T>& b) {
  return a > b.get();
}

template <typename T>
bool operator<=(Handle* a, const object<T>& b) {
  return !(a > b.get());
}

template <typename T>
bool operator>=(Handle* a, const object<T>& b) {
  return !(a < b.get());
}

template <typename T>
bool operator==(const object<T>& a, Handle* b) {
  return a.get() == b;
}

template <typename T>
bool operator!=(const object<T>& a, Handle* b) {
  return !(a == b);
}

template <typename T>
bool operator<(const object<T>& a, Handle* b) {
  return a.get() < b;
}

template <typename T>
bool operator>(const object<T>& a, Handle* b) {
  return a.get() > b;
}

template <typename T>
bool operator<=(const object<T>& a, Handle* b) {
  return !(a.get() > b);
}

template <typename T>
bool operator>=(const object<T>& a, Handle* b) {
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
  explicit unowned(Handle* h) : value_(h) {}
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
    HandleOwner h = value_.release();
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
bool operator==(Handle* a, const unowned<T>& b) {
  return a == b->get();
}

template <typename T>
bool operator!=(Handle* a, const unowned<T>& b) {
  return !(a == b);
}

template <typename T>
bool operator<(Handle* a, const unowned<T>& b) {
  return a < b->get();
}

template <typename T>
bool operator>(Handle* a, const unowned<T>& b) {
  return a > b->get();
}

template <typename T>
bool operator<=(Handle* a, const unowned<T>& b) {
  return !(a > b);
}

template <typename T>
bool operator>=(Handle* a, const unowned<T>& b) {
  return !(a < b);
}

template <typename T>
bool operator==(const unowned<T>& a, Handle* b) {
  return a->get() == b;
}

template <typename T>
bool operator!=(const unowned<T>& a, Handle* b) {
  return !(a == b);
}

template <typename T>
bool operator<(const unowned<T>& a, Handle* b) {
  return a->get() < b;
}

template <typename T>
bool operator>(const unowned<T>& a, Handle* b) {
  return a->get() > b;
}

template <typename T>
bool operator<=(const unowned<T>& a, Handle* b) {
  return !(a > b);
}

template <typename T>
bool operator>=(const unowned<T>& a, Handle* b) {
  return !(a < b);
}

}  // namespace zx

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_OBJECT_H_
