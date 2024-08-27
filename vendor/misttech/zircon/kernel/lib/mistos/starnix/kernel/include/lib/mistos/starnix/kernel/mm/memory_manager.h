// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_MANAGER_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_MANAGER_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <lib/mistos/starnix/kernel/mm/memory_accessor.h>
#include <lib/mistos/starnix/kernel/sync/locks.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/range-map.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>
#include <stdint.h>
#include <zircon/types.h>

#include <functional>
#include <optional>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>
#include <kernel/mutex.h>
#include <ktl/span.h>
#include <zxtest/cpp/zxtest_prod.h>

using namespace starnix_uapi;

namespace starnix {

constexpr uint64_t kProgramBreakLimit = 64 * 1024 * 1024;

struct ProgramBreak {
  // These base address at which the data segment is mapped.
  UserAddress base;

  // The current program break.
  //
  // The addresses from [base, current.round_up(*PAGE_SIZE)) are mapped into the
  // client address space from the underlying |vmo|.
  UserAddress current;
};

/*
/// The policy about whether the address space can be dumped.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DumpPolicy {
    /// The address space cannot be dumped.
    ///
    /// Corresponds to SUID_DUMP_DISABLE.
    Disable,

    /// The address space can be dumped.
    ///
    /// Corresponds to SUID_DUMP_USER.
    User,
}
*/

struct MemoryManagerForkableState {
  /// State for the brk and sbrk syscalls.
  ktl::optional<ProgramBreak> brk;

  /// The namespace node that represents the executable associated with this task.
  // executable_node: Option<NamespaceNode>,

  /// Stack location and size
  UserAddress stack_base;
  size_t stack_size;
  UserAddress stack_start;
  UserAddress auxv_start;
  UserAddress auxv_end;
  UserAddress argv_start;
  UserAddress argv_end;
  UserAddress environ_start;
  UserAddress environ_end;

  /// vDSO location
  UserAddress vdso_base;
};

// Define DesiredAddress enum
enum class DesiredAddressType { Any, Hint, Fixed, FixedOverwrite };

// The user-space address at which a mapping should be placed. Used by [`MemoryManager::map`].
struct DesiredAddress {
  DesiredAddressType type;
  UserAddress address;
};

// Define MappingName enum
enum class MappingNameType {
  /// No name.
  None,

  /// This mapping is the initial stack.
  Stack,

  /// This mapping is the heap.
  Heap,

  /// This mapping is the vdso.
  Vdso,

  /// This mapping is the vvar.
  Vvar,

  /// The file backing this mapping.
  File,

  /// The name associated with the mapping. Set by prctl(PR_SET_VMA, PR_SET_VMA_ANON_NAME, ...).
  /// An empty name is distinct from an unnamed mapping. Mappings are initially created with no
  /// name and can be reset to the unnamed state by passing NULL to
  /// prctl(PR_SET_VMA, PR_SET_VMA_ANON_NAME, ...).
  Vma,
};

struct MappingName {
  MappingNameType type;

  // NamespaceNode fileNode;

  FsString vmaName;
};

struct PrivateAnonymous {
  bool operator==(const PrivateAnonymous& other) const { return false; }
};

struct MappingBackingVmo {
 private:
  // The base address of this mapping.
  //
  // Keep in mind that the mapping might be trimmed in the RangeMap if the
  // part of the mapping is unmapped, which means the base might extend
  // before the currently valid portion of the mapping.
  UserAddress base_;

  // The VMO that contains the memory used in this mapping.
  zx::ArcVmo vmo_;

  // The offset in the VMO that corresponds to the base address.
  uint64_t vmo_offset_;

 public:
  // impl MappingBackingVmo

  /// Reads exactly `bytes.len()` bytes of memory from `addr`.
  ///
  /// # Parameters
  /// - `addr`: The address to read data from.
  /// - `bytes`: The byte array to read into.
  fit::result<Errno, ktl::span<uint8_t>> read_memory(UserAddress addr,
                                                     ktl::span<uint8_t>& bytes) const;

  /// Writes the provided bytes to `addr`.
  ///
  /// # Parameters
  /// - `addr`: The address to write to.
  /// - `bytes`: The bytes to write to the VMO.
  fit::result<Errno> write_memory(UserAddress addr, const ktl::span<const uint8_t>& bytes) const;

  // Converts a `UserAddress` to an offset in this mapping's VMO.
  uint64_t address_to_offset(UserAddress addr) const;

 public:
  // C++
  MappingBackingVmo(UserAddress base, zx::ArcVmo vmo, uint64_t vmo_offset)
      : base_(base), vmo_(ktl::move(vmo)), vmo_offset_(vmo_offset) {}

  bool operator==(const MappingBackingVmo& other) const {
    return base_ == other.base_ && vmo_ == other.vmo_ && vmo_offset_ == other.vmo_offset_;
  }

  zx::ArcVmo& vmo() { return vmo_; }

 private:
  ZXTEST_FRIEND_TEST(MemoryManager, test_unmap_beginning);
  ZXTEST_FRIEND_TEST(MemoryManager, test_unmap_end);
  ZXTEST_FRIEND_TEST(MemoryManager, test_unmap_middle);
  friend class MemoryManager;
  friend struct MemoryManagerState;
};

struct MappingBacking {
  std::variant<MappingBackingVmo, PrivateAnonymous> variant;

  // Constructor for MappingBackingVmo
  MappingBacking(MappingBackingVmo vmo) : variant(ktl::move(vmo)) {}

  // Constructor for PrivateAnonymous
  MappingBacking() : variant(PrivateAnonymous{}) {}

  bool operator==(const MappingBacking& other) const { return variant == other.variant; }

  // Helpers from the reference documentation for std::visit<>, to allow
  // visit-by-overload of the std::variant<> returned by GetLastReference():
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;
};

struct Mapping {
 private:
  // Object backing this mapping.
  MappingBacking backing_;

  // The flags used by the mapping, including protection.
  // flags: MappingFlags,
  MappingFlags flags_;

  // The name for this mapping.
  //
  // This may be a reference to the filesystem node backing this mapping or a userspace-assigned
  // name. The existence of this field is orthogonal to whether this mapping is anonymous -
  // mappings of the file '/dev/zero' are treated as anonymous mappings and anonymous mappings may
  // have a name assigned.
  //
  // Because of this exception, avoid using this field to check if a mapping is anonymous.
  // Instead, check if `options` bitfield contains `MappingOptions::ANONYMOUS`.
  MappingName name_;

  /// Lock guard held to prevent this file from being written while it's being executed.
  // file_write_guard: FileWriteGuardRef,

 public:
  /// impl Mapping
  static Mapping New(UserAddress base, zx::ArcVmo vmo, uint64_t vmo_offset, MappingFlagsImpl flags);

  // fn with_name

  /// Converts a `UserAddress` to an offset in this mapping's VMO.
  // fn address_to_offset(&self, addr: UserAddress) -> u64 {

  bool can_read() const { return flags_.contains(MappingFlagsEnum::READ); }
  bool can_write() const { return flags_.contains(MappingFlagsEnum::WRITE); }
  bool can_exec() const { return flags_.contains(MappingFlagsEnum::EXEC); }

  bool private_anonymous() const {
#if STARNIX_ANON_ALLOCS
#else
    return !flags_.contains(MappingFlagsEnum::SHARED) &&
           flags_.contains(MappingFlagsEnum::ANONYMOUS);
#endif
  }

 public:
  // C++
  Mapping() : flags_(MappingFlags::empty()) {}
  Mapping(const Mapping& other) = default;
  Mapping(Mapping&& other) = default;
  Mapping& operator=(const Mapping& other) = default;

  bool operator==(const Mapping& other) const { return backing_ == other.backing_; }

  // const MappingName& name() const { return name_; }

  // MappingName& name() { return name_; }

  MappingBacking& backing() { return backing_; }

  // MappingFlags flags() const { return flags_; }

 private:
  ZXTEST_FRIEND_TEST(MemoryManager, test_unmap_beginning);
  ZXTEST_FRIEND_TEST(MemoryManager, test_unmap_end);
  ZXTEST_FRIEND_TEST(MemoryManager, test_unmap_middle);
  ZXTEST_FRIEND_TEST(MemoryManager, test_preserve_name_snapshot);
  friend class MemoryManager;
  friend struct MemoryManagerState;

  Mapping(UserAddress base, zx::ArcVmo vmo, uint64_t vmo_offset, MappingFlagsImpl flags,
          MappingName name);
};

struct MemoryManagerState {
 public:
  // The VMAR in which userspace mappings occur.
  //
  // We map userspace memory in this child VMAR so that we can destroy the
  // entire VMAR during exec.
  zx::vmar user_vmar;

  // Cached VmarInfo for user_vmar.
  zx_info_vmar_t user_vmar_info;

  /// The memory mappings currently used by this address space.
  ///
  /// The mappings record which VMO backs each address.
  util::RangeMap<UserAddress, Mapping> mappings;

  /// VMO backing private, anonymous memory allocations in this address space.
  // #[cfg(feature = "alternate_anon_allocs")]
  // private_anonymous: PrivateAnonymousMemoryManager,

 private:
  MemoryManagerForkableState forkable_state_;

 public:
  /// Asynchronous I/O contexts.
  // pub aio_contexts: AioContexts,

  /// impl MemoryManagerState
  fit::result<Errno, UserAddress> map_anonymous(DesiredAddress addr, size_t length,
                                                ProtectionFlags prot_flags,
                                                MappingOptionsFlags options, MappingName name,
                                                fbl::Vector<Mapping>& released_mappings);

  fit::result<Errno, UserAddress> map_internal(DesiredAddress addr, const zx::vmo& vmo,
                                               uint64_t vmo_offset, size_t length,
                                               MappingFlags flags, bool populate);

  fit::result<Errno> validate_addr(DesiredAddress addr, size_t length);
  fit::result<Errno, UserAddress> map_vmo(DesiredAddress addr, zx::ArcVmo vmo, uint64_t vmo_offset,
                                          size_t length, MappingFlags flags, bool populate,
                                          MappingName name,
                                          fbl::Vector<Mapping>& released_mappings);

  fit::result<Errno> unmap(UserAddress, size_t length, fbl::Vector<Mapping>& released_mappings);

 public:
  MemoryManagerForkableState* operator->() { return &forkable_state_; }

 private:
  ZXTEST_FRIEND_TEST(MemoryManager, test_get_contiguous_mappings_at);

  friend class MemoryManager;

  bool check_has_unauthorized_splits(UserAddress addr, size_t length);

  fit::result<Errno, fbl::Vector<ktl::pair<Mapping, size_t>>> get_contiguous_mappings_at(
      UserAddress addr, size_t length) const;

  UserAddress max_address() const;

  fit::result<Errno> protect(UserAddress addr, size_t length, ProtectionFlags prot_flags);

  fit::result<Errno> update_after_unmap(UserAddress addr, size_t length,
                                        fbl::Vector<Mapping>& released_mappings);

  /// Reads exactly `bytes.len()` bytes of memory.
  ///
  /// # Parameters
  /// - `addr`: The address to read data from.
  /// - `bytes`: The byte array to read into.
  fit::result<Errno, ktl::span<uint8_t>> read_memory(UserAddress addr,
                                                     ktl::span<uint8_t>& bytes) const;

  /// Reads exactly `bytes.len()` bytes of memory from `addr`.
  ///
  /// # Parameters
  /// - `addr`: The address to read data from.
  /// - `bytes`: The byte array to read into.
  fit::result<Errno, ktl::span<uint8_t>> read_mapping_memory(UserAddress addr,
                                                             const Mapping& mapping,
                                                             ktl::span<uint8_t>& bytes) const;

  /// Reads bytes starting at `addr`, continuing until either `bytes.len()` bytes have been read
  /// or no more bytes can be read.
  ///
  /// This is used, for example, to read null-terminated strings where the exact length is not
  /// known, only the maximum length is.
  ///
  /// # Parameters
  /// - `addr`: The address to read data from.
  /// - `bytes`: The byte array to read into.
  fit::result<Errno, ktl::span<uint8_t>> read_memory_partial(UserAddress addr,
                                                             ktl::span<uint8_t>& bytes) const;

  /// Like `read_memory_partial` but only returns the bytes up to and including
  /// a null (zero) byte.
  fit::result<Errno, ktl::span<uint8_t>> read_memory_partial_until_null_byte(
      UserAddress addr, ktl::span<uint8_t>& bytes) const;

  /// Writes the provided bytes.
  ///
  /// In case of success, the number of bytes written will always be `bytes.len()`.
  ///
  /// # Parameters
  /// - `addr`: The address to write to.
  /// - `bytes`: The bytes to write.
  fit::result<Errno, size_t> write_memory(UserAddress addr,
                                          const ktl::span<const uint8_t>& bytes) const;

  /// Writes the provided bytes to `addr`.
  ///
  /// # Parameters
  /// - `addr`: The address to write to.
  /// - `bytes`: The bytes to write to the VMO.
  fit::result<Errno> write_mapping_memory(UserAddress addr, const Mapping& mapping,
                                          const ktl::span<const uint8_t>& bytes) const;
};

class CurrentTask;

class MemoryManager : public fbl::RefCounted<MemoryManager> {
 public:
  // The root VMAR for the child process.
  //
  // Instead of mapping memory directly in this VMAR, we map the memory in
  // `state.user_vmar`.
  const zx::vmar root_vmar;

  // The base address of the root_vmar.
  UserAddress base_addr;

  /// The futexes in this address space.
  // pub futex: FutexTable<PrivateFutexKey>,

  // Mutable state for the memory manager.
  mutable RwLock<MemoryManagerState> state;

  /// Whether this address space is dumpable.
  // pub dumpable: OrderedMutex<DumpPolicy, MmDumpable>,

  /// Maximum valid user address for this vmar.
  // pub maximum_valid_user_address: UserAddress,

 public:
  static fit::result<zx_status_t, fbl::RefPtr<MemoryManager>> New(zx::vmar root_vmar);

  // pub fn new_empty() -> Self;

  // fn from_vmar(root_vmar: zx::Vmar, user_vmar: zx::Vmar, user_vmar_info: zx::VmarInfo)

  fit::result<Errno, UserAddress> set_brk(const CurrentTask& current_task, UserAddress addr);

  fit::result<Errno> snapshot_to(fbl::RefPtr<MemoryManager>& target);

  fit::result<zx_status_t> exec(/*NamespaceNode exe_node*/);

  // private:
  static Errno get_errno_for_map_err(zx_status_t status);

 public:
  fit::result<Errno, UserAddress> map_vmo(DesiredAddress addr, zx::ArcVmo vmo, uint64_t vmo_offset,
                                          size_t length, ProtectionFlags prot_flags,
                                          MappingOptionsFlags options, MappingName name);

  fit::result<Errno, UserAddress> map_anonymous(DesiredAddress addr, size_t length,
                                                ProtectionFlags prot_flags,
                                                MappingOptionsFlags options, MappingName name);

  // pub fn remap

  fit::result<Errno> unmap(UserAddress, size_t length);

  fit::result<Errno> protect(UserAddress addr, size_t length, ProtectionFlags prot_flags);

  fit::result<Errno> set_mapping_name(UserAddress addr, size_t length,
                                      ktl::optional<FsString> name);

  // #[cfg(test)]
  fit::result<Errno, ktl::optional<FsString>> get_mapping_name(UserAddress addr);

  // #[cfg(test)]
  size_t get_mapping_count();

  UserAddress get_random_base(size_t length);

 public:
  /// impl MemoryManager
  bool has_same_address_space(const fbl::RefPtr<MemoryManager>& other) const {
    return root_vmar == other->root_vmar;
  }

  fit::result<Errno, ktl::span<uint8_t>> unified_read_memory(const CurrentTask& current_task,
                                                             UserAddress addr,
                                                             ktl::span<uint8_t>& bytes) const;

  fit::result<Errno, ktl::span<uint8_t>> vmo_read_memory(UserAddress addr,
                                                         ktl::span<uint8_t>& bytes) const;

  fit::result<Errno, ktl::span<uint8_t>> unified_read_memory_partial(
      const CurrentTask& current_task, UserAddress addr, ktl::span<uint8_t>& bytes) const;

  fit::result<Errno, ktl::span<uint8_t>> vmo_read_memory_partial(UserAddress addr,
                                                                 ktl::span<uint8_t>& bytes) const;

  fit::result<Errno, ktl::span<uint8_t>> unified_read_memory_partial_until_null_byte(
      const CurrentTask& current_task, UserAddress addr, ktl::span<uint8_t>& bytes) const;

  fit::result<Errno, ktl::span<uint8_t>> vmo_read_memory_partial_until_null_byte(
      UserAddress addr, ktl::span<uint8_t>& bytes) const;

  fit::result<Errno, size_t> unified_write_memory(const CurrentTask& current_task, UserAddress addr,
                                                  const ktl::span<const uint8_t>& bytes) const;

  fit::result<Errno, size_t> vmo_write_memory(UserAddress addr,
                                              const ktl::span<const uint8_t>& bytes) const;

 private:
  ZXTEST_FRIEND_TEST(MemoryManager, test_get_contiguous_mappings_at);
  ZXTEST_FRIEND_TEST(MemoryManager, test_read_write_crossing_mappings);
  ZXTEST_FRIEND_TEST(MemoryManager, test_read_write_errors);

  MemoryManager(zx::vmar root, zx::vmar user_vmar, zx_info_vmar_t user_vmar_info);
};

/// Creates a VMO that can be used in an anonymous mapping for the `mmap`
/// syscall.
fit::result<Errno, zx::ArcVmo> create_anonymous_mapping_vmo(uint64_t size);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_MANAGER_H_
