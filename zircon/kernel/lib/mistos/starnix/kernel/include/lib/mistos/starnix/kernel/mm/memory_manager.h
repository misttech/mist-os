// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_MANAGER_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_MANAGER_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/range-map.h>
#include <lib/mistos/zx/vmar.h>
#include <stdint.h>
#include <zircon/types.h>

#include <functional>
#include <optional>
#include <utility>
#include <vector>

#include <fbl/canary.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
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
  std::optional<ProgramBreak> brk;

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
  UserAddress address = 0;
};

// Define MappingName enum
enum class MappingNameType { None, Stack, Heap, Vdso, Vvar, File, Vma };

struct MappingName {
  MappingNameType type;
  // NamespaceNode fileNode;
  // FsString vmaName;
};

struct PrivateAnonymous {};
struct MappingBackingVmo {
  // The base address of this mapping.
  //
  // Keep in mind that the mapping might be trimmed in the RangeMap if the
  // part of the mapping is unmapped, which means the base might extend
  // before the currently valid portion of the mapping.
  UserAddress base;

  // The VMO that contains the memory used in this mapping.
  zx::vmo vmo;

  // The offset in the VMO that corresponds to the base address.
  uint64_t vmo_offset;

  fit::result<Errno> write_memory(UserAddress addr, ktl::span<const std::byte> bytes);

  // Converts a `UserAddress` to an offset in this mapping's VMO.
  uint64_t address_to_offset(UserAddress addr);
};

struct MappingBacking {
  std::variant<MappingBackingVmo, PrivateAnonymous> variant;

  // Constructor for MappingBackingVmo
  MappingBacking(MappingBackingVmo vmo) : variant(std::move(vmo)) {}

  // Constructor for PrivateAnonymous
  MappingBacking() : variant(PrivateAnonymous{}) {}

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

class Mapping : public fbl::RefCounted<Mapping> {
 public:
  static zx_status_t New(UserAddress base, zx::vmo vmo, uint64_t vmo_offset, MappingFlagsImpl flags,
                         fbl::RefPtr<Mapping>* out);

  const MappingName& name() const { return name_; }
  MappingName& name() { return name_; }

  MappingBacking& backing() { return backing_; }

  MappingFlags flags() const { return flags_; }

  bool can_read() { return flags_.contains(MappingFlagsEnum::READ); }
  bool can_write() { return flags_.contains(MappingFlagsEnum::WRITE); }
  bool can_exec() { return flags_.contains(MappingFlagsEnum::EXEC); }

  bool private_anonymous() {
#if STARNIX_ANON_ALLOCS
#else
    return flags_.contains(MappingFlagsEnum::SHARED) &&
           flags_.contains(MappingFlagsEnum::ANONYMOUS);
#endif
  }

 private:
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(Mapping);

  Mapping(UserAddress base, zx::vmo vmo, uint64_t vmo_offset, MappingFlagsImpl flags,
          MappingName name);

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
};

class MemoryManagerState {
 public:
  MemoryManagerState(const MemoryManagerState&) = delete;
  MemoryManagerState& operator=(const MemoryManagerState&) = delete;

  MemoryManagerState() = default;

  bool Initialize(zx::vmar vmar, zx_info_vmar_t info, MemoryManagerForkableState state);

  MemoryManagerForkableState* operator->() { return &forkable_state_; }

  fit::result<Errno, UserAddress> map_anonymous(
      DesiredAddress addr, size_t length, ProtectionFlags prot_flags, MappingOptionsFlags options,
      MappingName name, std::vector<fbl::RefPtr<Mapping>>& released_mappings);

  fit::result<Errno, UserAddress> map_internal(DesiredAddress addr, zx::vmo& vmo,
                                               uint64_t vmo_offset, size_t length,
                                               MappingFlags flags, bool populate);

  fit::result<Errno> validate_addr(DesiredAddress addr, size_t length);
  fit::result<Errno, UserAddress> map_vmo(DesiredAddress addr, zx::vmo& vmo, uint64_t vmo_offset,
                                          size_t length, MappingFlags flags, bool populate,
                                          MappingName name,
                                          std::vector<fbl::RefPtr<Mapping>>& released_mappings);

  fit::result<Errno> unmap(UserAddress, size_t length,
                           std::vector<fbl::RefPtr<Mapping>>& released_mappings);

  zx::vmar& user_vmar() { return user_vmar_; }
  const zx::vmar& user_vmar() const { return user_vmar_; }

  const util::RangeMap<UserAddress, fbl::RefPtr<Mapping>>& mappings() const { return mappings_; }

 private:
  ZXTEST_FRIEND_TEST(MemoryManager, test_get_contiguous_mappings_at);

  friend class MemoryManager;

  bool check_has_unauthorized_splits(UserAddress addr, size_t length);

  fit::result<Errno, std::vector<std::pair<fbl::RefPtr<Mapping>, size_t>>>
  get_contiguous_mappings_at(UserAddress addr, size_t length);

  UserAddress max_address();

  fit::result<Errno> protect(UserAddress addr, size_t length, ProtectionFlags prot_flags);

  fit::result<Errno> update_after_unmap(UserAddress addr, size_t length,
                                        std::vector<fbl::RefPtr<Mapping>>& released_mappings);

  fit::result<Errno, size_t> write_memory(UserAddress addr, ktl::span<const std::byte> bytes);
  fit::result<Errno> write_mapping_memory(UserAddress addr, fbl::RefPtr<Mapping>& mapping,
                                          ktl::span<const std::byte> bytes);

  // The VMAR in which userspace mappings occur.
  //
  // We map userspace memory in this child VMAR so that we can destroy the
  // entire VMAR during exec.
  zx::vmar user_vmar_;

  // Cached VmarInfo for user_vmar.
  zx_info_vmar_t user_vmar_info_;

  /// The memory mappings currently used by this address space.
  ///
  /// The mappings record which VMO backs each address.
  util::RangeMap<UserAddress, fbl::RefPtr<Mapping>> mappings_;

  /// VMO backing private, anonymous memory allocations in this address space.
  // #[cfg(feature = "alternate_anon_allocs")]
  // private_anonymous: PrivateAnonymousMemoryManager,

  MemoryManagerForkableState forkable_state_;
};

class CurrentTask;

class MemoryManager : public fbl::RefCounted<MemoryManager> {
 public:
  static zx_status_t New(zx::vmar, fbl::RefPtr<MemoryManager>* out);

  bool has_same_address_space(const fbl::RefPtr<MemoryManager>& other) {
    return root_vmar_ == other->root_vmar_;
  }

  fit::result<zx_status_t> exec(/*NamespaceNode exe_node*/);

  static Errno get_errno_for_map_err(zx_status_t status);
  size_t get_mapping_count();

  fit::result<Errno, UserAddress> set_brk(const CurrentTask& current_task, UserAddress addr);
  fit::result<Errno> unmap(UserAddress, size_t length);

  fit::result<Errno, size_t> unified_write_memory(const CurrentTask& current_task, UserAddress addr,
                                                  ktl::span<const std::byte> bytes);

  fit::result<Errno, UserAddress> map_anonymous(DesiredAddress addr, size_t length,
                                                ProtectionFlags prot_flags,
                                                MappingOptionsFlags options, MappingName name);

  Lock<Mutex>* mm_state_rw_lock() const TA_RET_CAP(mm_state_rw_lock_) { return &mm_state_rw_lock_; }

  fit::result<Errno> protect(UserAddress addr, size_t length, ProtectionFlags prot_flags);

  MemoryManagerState& state() TA_REQ(mm_state_rw_lock_) { return state_; }
  const MemoryManagerState& state() const TA_REQ_SHARED(mm_state_rw_lock_) { return state_; }

  fit::result<Errno, size_t> vmo_write_memory(UserAddress addr, ktl::span<const std::byte> bytes);

 private:
  ZXTEST_FRIEND_TEST(MemoryManager, test_get_contiguous_mappings_at);

  MemoryManager(zx::vmar root, zx::vmar user_vmar, zx_info_vmar_t user_vmar_info);

  fbl::Canary<fbl::magic("MMGR")> canary_;
  // The root VMAR for the child process.
  //
  // Instead of mapping memory directly in this VMAR, we map the memory in
  // `state.user_vmar`.
  zx::vmar root_vmar_;

  // The base address of the root_vmar.
  UserAddress base_addr_;

  /// The futexes in this address space.
  // pub futex: FutexTable<PrivateFutexKey>,

  // Mutable state for the memory manager.
  mutable DECLARE_MUTEX(MemoryManager) mm_state_rw_lock_;
  MemoryManagerState state_ TA_GUARDED(mm_state_rw_lock_);

  /// Whether this address space is dumpable.
  // pub dumpable: OrderedMutex<DumpPolicy, MmDumpable>,

  /// Maximum valid user address for this vmar.
  // pub maximum_valid_user_address: UserAddress,
};

/// Holds the number of _elements_ read by the callback to [`read_to_vec`].
///
/// Used to make it clear to callers that the callback should return the number
/// of elements read and not the number of bytes read.
struct NumberOfElementsRead {
  size_t n_elements;
};

/// Performs a read into a `Vec` using the provided read function.
///
/// The read function returns the number of elements of type `T` read.
///
/// # Safety
///
/// The read function must only return `Ok(n)` if at least one element was read and `n` holds
/// the number of elements of type `T` read starting from the beginning of the slice.
template <typename T, typename E>
fit::result<E, std::vector<T>> read_to_vec(
    size_t max_len, std::function<fit::result<E, NumberOfElementsRead>(ktl::span<T>&)> read_fn) {
  auto buffer = std::vector<T>(max_len);
  ktl::span<T> capacity(buffer.data(), max_len);
  auto read_fn_result = read_fn(capacity);
  if (read_fn_result.is_error())
    return read_fn_result.take_error();

  NumberOfElementsRead read_elements{read_fn_result.value()};
  DEBUG_ASSERT_MSG(read_elements.n_elements <= max_len, "read_elements=%zu, max_len=%zu",
                   read_elements.n_elements, max_len);
  buffer.resize(read_elements.n_elements);
  return fit::ok(std::move(buffer));
}

/// Creates a VMO that can be used in an anonymous mapping for the `mmap`
/// syscall.
fit::result<Errno, zx::vmo> create_anonymous_mapping_vmo(size_t size);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_MANAGER_H_
