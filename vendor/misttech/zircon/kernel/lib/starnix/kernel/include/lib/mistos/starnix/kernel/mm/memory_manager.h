// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_MANAGER_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_MANAGER_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <lib/mistos/starnix/kernel/mm/memory.h>
#include <lib/mistos/starnix/kernel/mm/memory_accessor.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/range-map.h>
#include <lib/starnix_sync/locks.h>
#include <stdint.h>
#include <zircon/rights.h>
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
#include <object/handle.h>
#include <object/vm_address_region_dispatcher.h>

namespace unit_testing {

bool test_get_contiguous_mappings_at();
bool test_unmap_returned_mappings();
bool test_unmap_returns_multiple_mappings();

}  // namespace unit_testing

namespace starnix {

using namespace starnix_uapi;
using namespace starnix_sync;

constexpr uint64_t kProgramBreakLimit = 64 * 1024 * 1024;

struct ProgramBreak {
  // These base address at which the data segment is mapped.
  UserAddress base;

  // The current program break.
  //
  // The addresses from [base, current.round_up(*PAGE_SIZE)) are mapped into the
  // client address space from the underlying |vmo|.
  UserAddress current;

  // Placeholder memory object mapped to pages reserved for program break growth.
  fbl::RefPtr<MemoryObject> placeholder_memory;
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

struct MappingBackingMemory {
 private:
  // The base address of this mapping.
  //
  // Keep in mind that the mapping might be trimmed in the RangeMap if the
  // part of the mapping is unmapped, which means the base might extend
  // before the currently valid portion of the mapping.
  UserAddress base_;

  // The memory object that contains the memory used in this mapping.
  fbl::RefPtr<MemoryObject> memory_;

  // The offset in the memory object that corresponds to the base address.
  uint64_t memory_offset_;

 public:
  // impl MappingBackingMemory

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

  fit::result<Errno, size_t> zero(UserAddress addr, size_t length) const;

  // Converts a `UserAddress` to an offset in this mapping's VMO.
  uint64_t address_to_offset(UserAddress addr) const;

 public:
  // C++
  MappingBackingMemory(UserAddress base, fbl::RefPtr<MemoryObject> memory, uint64_t memory_offset)
      : base_(base), memory_(ktl::move(memory)), memory_offset_(memory_offset) {}

  bool operator==(const MappingBackingMemory& other) const {
    return base_ == other.base_ && memory_ == other.memory_ &&
           memory_offset_ == other.memory_offset_;
  }

  // const fbl::RefPtr<VmObjectDispatcher>& vmo() const { return vmo_; }

 private:
  friend bool unit_testing::test_unmap_returned_mappings();
  // friend bool unit_testing::test_unmap_beginning();
  // friend bool unit_testing::test_unmap_end();
  // friend bool unit_testing::test_unmap_middle();
  friend class MemoryManager;
  friend struct MemoryManagerState;
};

struct MappingBacking {
  ktl::variant<MappingBackingMemory, PrivateAnonymous> variant;

  // Constructor for MappingBackingVmo
  MappingBacking(MappingBackingMemory memory) : variant(ktl::move(memory)) {}

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
  static Mapping New(UserAddress base, fbl::RefPtr<MemoryObject> memory, uint64_t memory_offset,
                     MappingFlagsImpl flags);

  static Mapping with_name(UserAddress base, fbl::RefPtr<MemoryObject> memory,
                           uint64_t memory_offset, MappingFlagsImpl flags, MappingName name);

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

  const MappingName& name() const { return name_; }

  MappingName& name() { return name_; }

  MappingBacking& backing() { return backing_; }

  MappingFlags flags() const { return flags_; }

 private:
  // ZXTEST_FRIEND_TEST(MemoryManager, test_unmap_beginning);
  // ZXTEST_FRIEND_TEST(MemoryManager, test_unmap_end);
  // ZXTEST_FRIEND_TEST(MemoryManager, test_unmap_middle);
  // ZXTEST_FRIEND_TEST(MemoryManager, test_preserve_name_snapshot);
  friend class MemoryManager;

  friend struct MemoryManagerState;

  Mapping(UserAddress base, fbl::RefPtr<MemoryObject> memory, uint64_t memory_offset,
          MappingFlagsImpl flags, MappingName name);
};

struct Vmars {
  /// The VMAR in which userspace mappings occur.
  ///
  /// We map userspace memory in this child VMAR so that we can destroy the
  /// entire VMAR during exec.
  Vmar vmar;

  /// Cached VmarInfo for vmar.
  zx_info_vmar_t vmar_info;

  /// TODO(https://fxbug.dev/359302155): Remove this hack. There are parts of Android that want to
  /// allocate memory in the lower 4 GiB and it does this by probing addresses in that range until
  /// it finds something free.  To help, we create a sub-vmar so that other allocations don't fill
  /// up this region.  NOTE: This is different to LOWER_32BIT which, confusingly, only applies to
  /// the lower *2 GiB*.
  ktl::optional<Vmar> lower_4gb;

 public:
  // impl Vmars
  static Vmars New(Vmar vmar, zx_info_vmar_t vmar_info);

  Vmar& vmar_for_addr(UserAddress addr);

  fit::result<Errno, UserAddress> map(DesiredAddress addr, fbl::RefPtr<MemoryObject> memory,
                                      uint64_t memory_offset, size_t length, MappingFlags flags,
                                      bool populate) const;

  fit::result<zx_status_t> unmap(UserAddress addr, size_t length);

  fit::result<zx_status_t> protect(UserAddress addr, size_t length, uint32_t flags);

  util::Range<UserAddress> address_range() const;

  fit::result<zx_status_t, size_t> raw_map(fbl::RefPtr<MemoryObject> memory, size_t vmar_offset,
                                           uint64_t memory_offset, size_t len, MappingFlags flags);

  fit::result<zx_status_t> destroy() const;

  UserAddress get_random_base(size_t length) const;
};

const UserAddress LOWER_4GB_LIMIT = UserAddress::const_from(0x100000000);

class MemoryManager;

struct MemoryManagerState {
 public:
  /// The vmars for the user.
  Vmars user_vmars;

  /// The memory mappings currently used by this address space.
  ///
  /// The mappings record which VMO backs each address.
  util::RangeMap<UserAddress, Mapping> mappings;

  /// VMO backing private, anonymous memory allocations in this address space.
  // #[cfg(feature = "alternate_anon_allocs")]
  // private_anonymous: PrivateAnonymousMemoryManager,

  /// Asynchronous I/O contexts.
  // pub aio_contexts: AioContexts,

 private:
  MemoryManagerForkableState forkable_state_;

 private:
  /// impl MemoryManagerState

  // Map the memory without updating `self.mappings`.
  fit::result<Errno, UserAddress> map_internal(DesiredAddress addr, fbl::RefPtr<MemoryObject> vmo,
                                               uint64_t memory_offset, size_t length,
                                               MappingFlags flags, bool populate);

  fit::result<Errno> validate_addr(DesiredAddress addr, size_t length);

  fit::result<Errno, UserAddress> map_memory(fbl::RefPtr<MemoryManager>, DesiredAddress addr,
                                             fbl::RefPtr<MemoryObject> memory, uint64_t vmo_offset,
                                             size_t length, MappingFlags flags, bool populate,
                                             MappingName name,
                                             fbl::Vector<Mapping>& released_mappings);

  fit::result<Errno, UserAddress> map_private_anonymous(
      fbl::RefPtr<MemoryManager> mm, DesiredAddress addr, size_t length, ProtectionFlags prot_flags,
      MappingOptionsFlags options, MappingName name, fbl::Vector<Mapping>& released_mappings);

  fit::result<Errno, UserAddress> map_anonymous(fbl::RefPtr<MemoryManager> mm, DesiredAddress addr,
                                                size_t length, ProtectionFlags prot_flags,
                                                MappingOptionsFlags options, MappingName name,
                                                fbl::Vector<Mapping>& released_mappings);

  fit::result<Errno, UserAddress> remap(fbl::RefPtr<MemoryManager> mm, UserAddress old_addr,
                                        size_t old_length,
                                        size_t new_length /*, MremapFlags flags*/,
                                        UserAddress new_addr,
                                        fbl::Vector<Mapping>& released_mappings);

  /// Attempts to grow or shrink the mapping in-place. Returns `Ok(Some(addr))` if the remap was
  /// successful. Returns `Ok(None)` if there was no space to grow.
  fit::result<Errno, ktl::optional<UserAddress>> try_remap_in_place(
      fbl::RefPtr<MemoryManager> mm, UserAddress old_addr, size_t old_length, size_t new_length,
      fbl::Vector<Mapping>& released_mappings);

  /// Grows or shrinks the mapping while moving it to a new destination.
  fit::result<Errno, UserAddress> remap_move(fbl::RefPtr<MemoryManager> mm, UserAddress src_addr,
                                             size_t src_length, ktl::optional<UserAddress> dst_addr,
                                             size_t dst_length,
                                             fbl::Vector<Mapping>& released_mappings);

  // Checks if an operation may be performed over the target mapping that may
  // result in a split mapping.
  //
  // An operation may be forbidden if the target mapping only partially covers
  // an existing mapping with the `MappingOptions::DONT_SPLIT` flag set.
  bool check_has_unauthorized_splits(UserAddress addr, size_t length);

  /// Unmaps the specified range. Unmapped mappings are placed in `released_mappings`.
  fit::result<Errno> unmap(fbl::RefPtr<MemoryManager> mm, UserAddress, size_t length,
                           fbl::Vector<Mapping>& released_mappings);

  fit::result<Errno> update_after_unmap(fbl::RefPtr<MemoryManager> mm, UserAddress addr,
                                        size_t length, fbl::Vector<Mapping>& released_mappings);

  fit::result<Errno> protect(UserAddress addr, size_t length, ProtectionFlags prot_flags);

  UserAddress max_address() const;

  fit::result<Errno, fbl::Vector<ktl::pair<Mapping, size_t>>> get_contiguous_mappings_at(
      UserAddress addr, size_t length) const;

 public:
  MemoryManagerForkableState* operator->() { return &forkable_state_; }

 private:
  friend bool unit_testing::test_get_contiguous_mappings_at();
  friend bool unit_testing::test_unmap_returned_mappings();
  friend bool unit_testing::test_unmap_returns_multiple_mappings();
  friend class MemoryManager;

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
  const Vmar root_vmar;

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
  static fit::result<zx_status_t, fbl::RefPtr<MemoryManager>> New(Vmar root_vmar);

  static fbl::RefPtr<MemoryManager> new_empty();

  static fbl::RefPtr<MemoryManager> from_vmar(Vmar root_vmar, Vmar user_vmar,
                                              zx_info_vmar_t user_vmar_info);

  fit::result<Errno, UserAddress> set_brk(const CurrentTask& current_task, UserAddress addr);

 private:
  bool extend_brk(MemoryManagerState& state, fbl::RefPtr<MemoryManager>& mm, UserAddress old_end,
                  size_t delta, UserAddress brk_base, fbl::Vector<Mapping>& released_mappings);

 public:
  fit::result<Errno> snapshot_to(fbl::RefPtr<MemoryManager>& target);

  fit::result<zx_status_t> exec(/*NamespaceNode exe_node*/);

  // private:
  static Errno get_errno_for_map_err(zx_status_t status);

 public:
  fit::result<Errno, UserAddress> map_memory(DesiredAddress addr, fbl::RefPtr<MemoryObject> memory,
                                             uint64_t memory_offset, size_t length,
                                             ProtectionFlags prot_flags,
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
    return root_vmar.vmar == other->root_vmar.vmar;
  }

  fit::result<Errno, ktl::span<uint8_t>> unified_read_memory(const CurrentTask& current_task,
                                                             UserAddress addr,
                                                             ktl::span<uint8_t>& bytes) const;

  fit::result<Errno, ktl::span<uint8_t>> syscall_read_memory(UserAddress addr,
                                                             ktl::span<uint8_t>& bytes) const;

  fit::result<Errno, ktl::span<uint8_t>> unified_read_memory_partial_until_null_byte(
      const CurrentTask& current_task, UserAddress addr, ktl::span<uint8_t>& bytes) const;

  fit::result<Errno, ktl::span<uint8_t>> syscall_read_memory_partial_until_null_byte(
      UserAddress addr, ktl::span<uint8_t>& bytes) const;

  fit::result<Errno, ktl::span<uint8_t>> unified_read_memory_partial(
      const CurrentTask& current_task, UserAddress addr, ktl::span<uint8_t>& bytes) const;

  fit::result<Errno, ktl::span<uint8_t>> syscall_read_memory_partial(
      UserAddress addr, ktl::span<uint8_t>& bytes) const;

  fit::result<Errno, size_t> unified_write_memory(const CurrentTask& current_task, UserAddress addr,
                                                  const ktl::span<const uint8_t>& bytes) const;

  fit::result<Errno, size_t> syscall_write_memory(UserAddress addr,
                                                  const ktl::span<const uint8_t>& bytes) const;

  fit::result<Errno, size_t> unified_write_memory_partial(
      const CurrentTask& current_task, UserAddress addr,
      const ktl::span<const uint8_t>& bytes) const;

  fit::result<Errno, size_t> syscall_write_memory_partial(
      const CurrentTask& current_task, UserAddress addr,
      const ktl::span<const uint8_t>& bytes) const;

  fit::result<Errno, size_t> unified_zero(const CurrentTask& current_task, UserAddress addr,
                                          size_t length) const;

  fit::result<Errno, size_t> syscall_zero(const CurrentTask& current_task, UserAddress addr,
                                          size_t length) const;

 private:
  friend bool unit_testing::test_get_contiguous_mappings_at();
  // friend bool unit_testing::test_read_write_crossing_mappings();
  // friend bool unit_testing::test_read_write_errors();

  MemoryManager(Vmar root, Vmar user_vmar, zx_info_vmar_t user_vmar_info);
};

// Creates a memory object that can be used in an anonymous mapping for the `mmap` syscall.
fit::result<Errno, fbl::RefPtr<MemoryObject>> create_anonymous_mapping_memory(uint64_t size);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_MANAGER_H_
