// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_FLAGS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_FLAGS_H_

#include <lib/mistos/util/bitflags.h>
#include <zircon/types.h>

#include <linux/mman.h>

namespace starnix {

// Enum class to represent mapping options
enum class MappingOptions : uint32_t {
  SHARED = 1,
  ANONYMOUS = 2,
  LOWER_32BIT = 4,
  GROWSDOWN = 8,
  ELF_BINARY = 16,
  DONTFORK = 32,
  WIPEONFORK = 64,
  DONT_SPLIT = 128,
  POPULATE = 256
};

using MappingOptionsFlags = Flags<MappingOptions>;

// Enum class to represent protection flags
enum class ProtectionFlagsEnum : uint32_t { READ = 0x1, WRITE = 0x2, EXEC = 0x4 };

using ProtectionFlags = Flags<starnix::ProtectionFlagsEnum>;

class ProtectionFlagsImpl : public ProtectionFlags {
 public:
  ProtectionFlagsImpl(const ProtectionFlags& flags) : ProtectionFlags(flags) {}

  zx_vm_option_t to_vmar_flags() {
    zx_vm_option_t flags = 0;
    if (contains(ProtectionFlagsEnum::READ)) {
      flags |= ZX_VM_PERM_READ;
    }
    if (contains(ProtectionFlagsEnum::WRITE)) {
      flags |= ZX_VM_PERM_READ | ZX_VM_PERM_WRITE;
    }
    if (contains(ProtectionFlagsEnum::EXEC)) {
      flags |= ZX_VM_PERM_EXECUTE | ZX_VM_PERM_READ_IF_XOM_UNSUPPORTED;
    }
    return flags;
  }

  static ProtectionFlags from_vmar_flags(zx_vm_option_t vmar_flags) {
    auto prot_flags = empty();
    if ((vmar_flags & ZX_VM_PERM_READ) == ZX_VM_PERM_READ) {
      prot_flags |= ProtectionFlagsEnum::READ;
    }
    if ((vmar_flags & ZX_VM_PERM_WRITE) == ZX_VM_PERM_WRITE) {
      prot_flags |= ProtectionFlagsEnum::WRITE;
    }
    if ((vmar_flags & ZX_VM_PERM_EXECUTE) == ZX_VM_PERM_EXECUTE) {
      prot_flags |= ProtectionFlagsEnum::EXEC;
    }
    return prot_flags;
  }
};

// Enum class to represent mapping flags
enum class MappingFlagsEnum : uint32_t {
  READ = 1 << 0,   // PROT_READ
  WRITE = 1 << 1,  // PROT_WRITE
  EXEC = 1 << 2,   // PROT_EXEC
  SHARED = 1 << 3,
  ANONYMOUS = 1 << 4,
  LOWER_32BIT = 1 << 5,
  GROWSDOWN = 1 << 6,
  ELF_BINARY = 1 << 7,
  DONTFORK = 1 << 8,
  WIPEONFORK = 1 << 9,
  DONT_SPLIT = 1 << 10
};

// The low three bits of MappingFlags match ProtectionFlags.
static_assert(static_cast<uint32_t>(MappingFlagsEnum::READ) == PROT_READ);
static_assert(static_cast<uint32_t>(MappingFlagsEnum::WRITE) == PROT_WRITE);
static_assert(static_cast<uint32_t>(MappingFlagsEnum::EXEC) == PROT_EXEC);

// The next bits of MappingFlags match MappingOptions, shifted up.
static_assert(static_cast<uint32_t>(MappingFlagsEnum::SHARED) ==
              static_cast<uint32_t>(MappingOptions::SHARED) << 3);
static_assert(static_cast<uint32_t>(MappingFlagsEnum::ANONYMOUS) ==
              static_cast<uint32_t>(MappingOptions::ANONYMOUS) << 3);
static_assert(static_cast<uint32_t>(MappingFlagsEnum::LOWER_32BIT) ==
              static_cast<uint32_t>(MappingOptions::LOWER_32BIT) << 3);
static_assert(static_cast<uint32_t>(MappingFlagsEnum::GROWSDOWN) ==
              static_cast<uint32_t>(MappingOptions::GROWSDOWN) << 3);
static_assert(static_cast<uint32_t>(MappingFlagsEnum::ELF_BINARY) ==
              static_cast<uint32_t>(MappingOptions::ELF_BINARY) << 3);
static_assert(static_cast<uint32_t>(MappingFlagsEnum::DONTFORK) ==
              static_cast<uint32_t>(MappingOptions::DONTFORK) << 3);
static_assert(static_cast<uint32_t>(MappingFlagsEnum::WIPEONFORK) ==
              static_cast<uint32_t>(MappingOptions::WIPEONFORK) << 3);
static_assert(static_cast<uint32_t>(MappingFlagsEnum::DONT_SPLIT) ==
              static_cast<uint32_t>(MappingOptions::DONT_SPLIT) << 3);

using MappingFlags = Flags<starnix::MappingFlagsEnum>;

class MappingFlagsImpl : public MappingFlags {
 public:
  explicit MappingFlagsImpl(MappingFlags flag) : MappingFlags(flag) {}

  ProtectionFlagsImpl prot_flags() {
    return ProtectionFlagsImpl(ProtectionFlags::from_bits_truncate(bits()));
  }

  static MappingFlags from_prot_flags_and_options(const ProtectionFlags& prot_flags,
                                                  const MappingOptionsFlags& options) {
    return from_bits_truncate(prot_flags.bits()) | from_bits_truncate(options.bits() << 3);
  }
};

}  // namespace starnix

template <>
constexpr Flag<starnix::MappingOptions> Flags<starnix::MappingOptions>::FLAGS[] = {
    {starnix::MappingOptions::SHARED},      {starnix::MappingOptions::ANONYMOUS},
    {starnix::MappingOptions::LOWER_32BIT}, {starnix::MappingOptions::GROWSDOWN},
    {starnix::MappingOptions::ELF_BINARY},  {starnix::MappingOptions::DONTFORK},
    {starnix::MappingOptions::WIPEONFORK},  {starnix::MappingOptions::DONT_SPLIT},
    {starnix::MappingOptions::POPULATE},
};

template <>
constexpr Flag<starnix::ProtectionFlagsEnum> Flags<starnix::ProtectionFlagsEnum>::FLAGS[] = {
    {starnix::ProtectionFlagsEnum::READ},
    {starnix::ProtectionFlagsEnum::WRITE},
    {starnix::ProtectionFlagsEnum::EXEC},
    // Add more flags as needed
};

template <>
constexpr Flag<starnix::MappingFlagsEnum> Flags<starnix::MappingFlagsEnum>::FLAGS[] = {
    {starnix::MappingFlagsEnum::READ},       {starnix::MappingFlagsEnum::WRITE},
    {starnix::MappingFlagsEnum::EXEC},       {starnix::MappingFlagsEnum::SHARED},
    {starnix::MappingFlagsEnum::ANONYMOUS},  {starnix::MappingFlagsEnum::LOWER_32BIT},
    {starnix::MappingFlagsEnum::GROWSDOWN},  {starnix::MappingFlagsEnum::ELF_BINARY},
    {starnix::MappingFlagsEnum::DONTFORK},   {starnix::MappingFlagsEnum::WIPEONFORK},
    {starnix::MappingFlagsEnum::DONT_SPLIT},
};

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_FLAGS_H_
