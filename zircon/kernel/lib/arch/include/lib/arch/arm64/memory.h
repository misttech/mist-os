// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_ARM64_MEMORY_H_
#define ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_ARM64_MEMORY_H_

#include <lib/arch/internal/bits.h>
#include <lib/arch/sysreg.h>
#include <zircon/assert.h>

#include <cstdint>

#include <hwreg/bitfields.h>

namespace arch {

// Memory attributes.
//
// This is a list of used memory attributes, and not comprehensive.
enum class ArmMemoryAttribute : uint8_t {
  // Device memory: non write combining, no reorder, no early ack.
  kDevice_nGnRnE = 0b0000'0000,

  // Device memory: non write combining, no reorder, early ack.
  kDevice_nGnRE = 0b0000'0100,

  // Normal Memory, Outer Write-back non-transient Read/Write allocate, Inner
  // Write-back non-transient Read/Write allocate
  kNormalCached = 0b1111'1111,

  // Normal memory, Inner/Outer uncached, Write Combined
  kNormalUncached = 0b0100'0100,
};

// Memory Attribute Indirection Register
//
// [arm/v8]: D13.2.95  MAIR_EL1, Memory Attribute Indirection Register, EL1
// [arm/v8]: D13.2.96  MAIR_EL2, Memory Attribute Indirection Register, EL2
struct ArmMemoryAttrIndirectionRegister
    : public SysRegDerivedBase<ArmMemoryAttrIndirectionRegister, uint64_t> {
  DEF_ENUM_FIELD(ArmMemoryAttribute, 63, 56, attr7);
  DEF_ENUM_FIELD(ArmMemoryAttribute, 55, 48, attr6);
  DEF_ENUM_FIELD(ArmMemoryAttribute, 47, 40, attr5);
  DEF_ENUM_FIELD(ArmMemoryAttribute, 39, 32, attr4);
  DEF_ENUM_FIELD(ArmMemoryAttribute, 31, 24, attr3);
  DEF_ENUM_FIELD(ArmMemoryAttribute, 23, 16, attr2);
  DEF_ENUM_FIELD(ArmMemoryAttribute, 15, 8, attr1);
  DEF_ENUM_FIELD(ArmMemoryAttribute, 7, 0, attr0);

  static constexpr size_t kNumAttributes = 8;

  // Get the ArmMemoryAttribute at the given index.
  //
  // TODO(fxbug.dev/78027): Ideally hwreg would support this natively.
  ArmMemoryAttribute GetAttribute(size_t index) const {
    ZX_DEBUG_ASSERT(index < kNumAttributes);
    size_t low_bit = index * kAttributeBits;
    size_t high_bit = low_bit + kAttributeBits - 1;
    return static_cast<ArmMemoryAttribute>(internal::ExtractBits(high_bit, low_bit, reg_value()));
  }

  // Set the ArmMemoryAttribute at the given index.
  //
  // TODO(fxbug.dev/78027): Ideally hwreg would support this natively.
  ArmMemoryAttrIndirectionRegister& SetAttribute(size_t index, ArmMemoryAttribute value) {
    ZX_DEBUG_ASSERT(index < kNumAttributes);

    size_t low_bit = index * kAttributeBits;
    size_t high_bit = low_bit + kAttributeBits - 1;
    set_reg_value(
        internal::UpdateBits(high_bit, low_bit, reg_value(), static_cast<uint64_t>(value)));
    return *this;
  }

 private:
  static constexpr size_t kAttributeBits = 8;  // Width of each attribute (in bits).
};

struct ArmMairEl1 : public arch::SysRegDerived<ArmMairEl1, ArmMemoryAttrIndirectionRegister> {};
ARCH_ARM64_SYSREG(ArmMairEl1, "mair_el1");

struct ArmMairEl2 : public arch::SysRegDerived<ArmMairEl2, ArmMemoryAttrIndirectionRegister> {};
ARCH_ARM64_SYSREG(ArmMairEl2, "mair_el2");

}  // namespace arch

#endif  // ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_ARM64_MEMORY_H_
