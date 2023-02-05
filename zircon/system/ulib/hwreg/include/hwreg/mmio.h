// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef HWREG_MMIO_H_
#define HWREG_MMIO_H_

#include <lib/mmio-ptr/mmio-ptr.h>

#include <cstdint>
#include <type_traits>

#include "internal.h"

namespace hwreg {

// This can be passed to ReadFrom and WriteTo methods.  The RegisterAddr
// object holds an offset from an MMIO base address stored in this object.
//
// The template parameter gives a factor applied to the offset before it's
// added to the base address.  This is used when mapping pio to mmio.
// For normal mmio, the unscaled RegisterMmio specialization is normally used.

template <uint32_t Scale>
class RegisterMmioScaled {
 public:
  RegisterMmioScaled(volatile void* mmio) : mmio_(InitPtr(mmio)) {}

  // Write |val| to the |sizeof(IntType)| byte field located |offset| bytes from
  // |base()|.
  template <typename IntType>
  void Write(IntType val, uint32_t offset) {
    MmioWrite(val, MmioPtr<IntType>(offset));
  }

  // Read the value of the |sizeof(IntType)| byte field located |offset| bytes from
  // |base()|.
  template <typename IntType>
  IntType Read(uint32_t offset) {
    return MmioRead(MmioPtr<const IntType>(offset));
  }

  uintptr_t base() const { return reinterpret_cast<uintptr_t>(mmio_); }

 private:
  MMIO_PTR volatile std::byte* InitPtr(volatile void* ptr) {
    uintptr_t addr = reinterpret_cast<uintptr_t>(ptr);
    return reinterpret_cast<MMIO_PTR volatile std::byte*>(addr);
  }

  template <typename T>
  MMIO_PTR volatile T* MmioPtr(uint32_t offset) {
    using PtrType = MMIO_PTR volatile T*;
    using IntType = std::remove_const_t<T>;
    static_assert(internal::IsSupportedInt<IntType>::value, "unsupported register access width");
    MMIO_PTR volatile std::byte* addr = mmio_ + (offset * Scale);
    return reinterpret_cast<PtrType>(addr);
  }

  MMIO_PTR volatile std::byte* mmio_ = nullptr;
};

using RegisterMmio = RegisterMmioScaled<1>;
static_assert(std::is_copy_constructible_v<RegisterMmio>);

}  // namespace hwreg

#endif  // HWREG_MMIO_H_
