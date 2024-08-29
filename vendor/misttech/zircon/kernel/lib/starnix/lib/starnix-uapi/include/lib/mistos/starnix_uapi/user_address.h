// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_USER_ADDRESS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_USER_ADDRESS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/math.h>
#include <zircon/types.h>

#include <optional>

#include <ktl/optional.h>

namespace starnix_uapi {

class UserAddress {
 private:
  static constexpr uint64_t kNullPtr = 0;

 public:
  static const UserAddress NULL_;

  UserAddress() = default;
  UserAddress(uint64_t address) : address_(address) {}

  // TODO(lindkvist): Remove this in favor of marking the From<u64> trait const once feature is
  // stabilized.
  static const UserAddress const_from(uint64_t address) { return UserAddress(address); }

  static UserAddress from_ptr(zx_vaddr_t ptr) { return UserAddress(static_cast<uint64_t>(ptr)); }

  zx_vaddr_t ptr() const { return static_cast<zx_vaddr_t>(address_); }

  fit::result<Errno, UserAddress> round_up(uint64_t increment) {
    auto result =
        round_up_to_increment(static_cast<size_t>(address_), static_cast<size_t>(increment));
    if (result.is_error()) {
      return result.take_error();
    }
    return fit::ok(UserAddress(static_cast<uint64_t>(result.value())));
  }

  bool is_aligned(uint64_t alignment) const { return address_ % alignment == 0; }

  bool is_null() const { return address_ == kNullPtr; }

  ktl::optional<UserAddress> checked_add(size_t rhs) {
    uint64_t result;
    if (add_overflow(address_, rhs, &result)) {
      return ktl::nullopt;
    }
    return UserAddress(result);
  }

  UserAddress saturating_add(size_t rhs) {
    uint64_t result;
    add_overflow(address_, rhs, &result);
    return UserAddress(result);
  }

  bool operator==(const UserAddress& rhs) const { return (address_ == rhs.address_); }
  bool operator!=(const UserAddress& rhs) const { return (address_ != rhs.address_); }
  bool operator>(const UserAddress& rhs) const { return (address_ > rhs.address_); }
  bool operator>=(const UserAddress& rhs) const { return (address_ >= rhs.address_); }
  bool operator<(const UserAddress& rhs) const { return (address_ < rhs.address_); }
  bool operator<=(const UserAddress& rhs) const { return (address_ <= rhs.address_); }

  UserAddress operator+(uint32_t rhs) const {
    return UserAddress(address_ + static_cast<uint64_t>(rhs));
  }
  UserAddress operator+(uint64_t rhs) const { return UserAddress(address_ + rhs); }
  UserAddress& operator+=(uint64_t rhs) {
    address_ += rhs;
    return *this;
  }

  UserAddress operator-(uint32_t rhs) const {
    return UserAddress(address_ - static_cast<uint64_t>(rhs));
  }
  UserAddress operator-(uint64_t rhs) const { return UserAddress(address_ - rhs); }
  UserAddress& operator-=(uint64_t rhs) {
    address_ -= rhs;
    return *this;
  }

  size_t operator-(const UserAddress& rhs) const { return ptr() - rhs.ptr(); }

  static UserAddress from(uint64_t value) { return UserAddress(value); }

 private:
  uint64_t address_ = kNullPtr;
};

using UserCString = UserAddress;

template <typename T>
class UserRef {
 public:
  UserRef(UserAddress addr) : addr_(addr) {}

  UserAddress addr() { return addr_; }

  UserRef<T> next() { return UserRef(addr() + sizeof(T)); }

  UserRef at(size_t index) { UserRef(addr() + index * sizeof(T)); }

  template <typename S>
  UserRef<S> cast() {
    return UserRef<S>(addr_);
  }

 private:
  UserAddress addr_;
};

}  // namespace starnix_uapi

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_USER_ADDRESS_H_
