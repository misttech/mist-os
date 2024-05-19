// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/mm/memory_manager.h"

#include <align.h>
#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/back_insert_iterator.h>
#include <lib/mistos/util/range-map.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>
#include <lib/user_copy/user_ptr.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <optional>
#include <utility>
#include <variant>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <object/thread_dispatcher.h>

#include "../kernel_priv.h"

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

constexpr size_t PRIVATE_ASPACE_BASE = USER_ASPACE_BASE;
constexpr size_t PRIVATE_ASPACE_SIZE = USER_ASPACE_SIZE;

namespace {

using namespace starnix;

fit::result<zx_status_t, zx::vmar> create_user_vmar(const zx::vmar& vmar,
                                                    const zx_info_vmar_t& vmar_info) {
  zx::vmar child;
  uintptr_t child_addr;
  constexpr zx_vm_option_t kFlags = ZX_VM_SPECIFIC | ZX_VM_CAN_MAP_SPECIFIC | ZX_VM_CAN_MAP_READ |
                                    ZX_VM_CAN_MAP_WRITE | ZX_VM_CAN_MAP_EXECUTE;

  LTRACEF("vmar_info.len %lu\n", vmar_info.len);
  zx_status_t status = vmar.allocate(kFlags, 0, vmar_info.len, &child, &child_addr);
  if (status != ZX_OK) {
    LTRACEF("allocate failed %d\n", status);
    return fit::error(status);
  }
  ASSERT(child_addr == vmar_info.base);
  return fit::ok(ktl::move(child));
}

fit::result<Errno, UserAddress> map_in_vmar(const zx::vmar& vmar, const zx_info_vmar_t& vmar_info,
                                            DesiredAddress addr, const zx::vmo& vmo,
                                            uint64_t vmo_offset, size_t length, MappingFlags flags,
                                            bool populate) {
  LTRACE;
  auto base_addr = UserAddress::from_ptr(vmar_info.base);
  auto mflags = MappingFlagsImpl(flags);

  auto match_addr =
      [mflags, base_addr](starnix::DesiredAddress addr) -> ktl::pair<uint64_t, zx_vm_option_t> {
    switch (addr.type) {
      case starnix::DesiredAddressType::Any: {
        if (mflags.contains(MappingFlagsEnum::LOWER_32BIT)) {
          return ktl::pair(0x80000000 - base_addr.ptr(), ZX_VM_OFFSET_IS_UPPER_LIMIT);
        }
        return ktl::pair(0, 0);
      }
      case starnix::DesiredAddressType::Hint:
      case starnix::DesiredAddressType::Fixed:
        return ktl::pair(addr.address - base_addr, ZX_VM_SPECIFIC);
      case starnix::DesiredAddressType::FixedOverwrite:
        return ktl::pair(addr.address - base_addr, ZX_VM_SPECIFIC_OVERWRITE);
    };
  };
  auto [vmar_offset, vmar_extra_flags] = match_addr(addr);

  if (populate) {
    uint32_t op;
    if (mflags.contains(MappingFlagsEnum::WRITE)) {
      // Requires ZX_RIGHT_WRITEABLE which we should expect when the mapping is writeable.
      op = ZX_VMO_OP_COMMIT;
    } else {
      // When we don't expect to have ZX_RIGHT_WRITEABLE, fall back to a VMO op that doesn't
      // need it.
      // TODO(https://fxbug.dev/42082608) use a gentler signal when available
      op = ZX_VMO_OP_ALWAYS_NEED;
    }
    vmo.op_range(op, vmo_offset, static_cast<uint64_t>(length), nullptr, 0);
    // "The mmap() call doesn't fail if the mapping cannot be populated."
  }

  bool contains_specific_overwrite =
      (ZX_VM_SPECIFIC_OVERWRITE & vmar_extra_flags) == ZX_VM_SPECIFIC_OVERWRITE;

  int vmar_maybe_map_range = (populate && !contains_specific_overwrite) ? ZX_VM_MAP_RANGE : 0;

  auto vmar_flags = mflags.prot_flags().to_vmar_flags() | ZX_VM_ALLOW_FAULTS | vmar_extra_flags |
                    vmar_maybe_map_range;

  LTRACEF("flags 0x%x vmar_offset %lu vmo_offset %lu length %lu\n", vmar_flags, vmar_offset,
          vmo_offset, length);
  zx_vaddr_t map_result;
  zx_status_t result = vmar.map(vmar_flags, vmar_offset, vmo, vmo_offset, length, &map_result);
  if (result != ZX_OK) {
    LTRACEF("Retry mapping if the target address was a Hint\n");
    if (addr.type == starnix::DesiredAddressType::Hint) {
      // let vmar_flags = vmar_flags - zx::VmarFlags::SPECIFIC;
      result = vmar.map(vmar_flags, 0, vmo, vmo_offset, length, &map_result);
    }
  }

  if (result != ZX_OK) {
    LTRACEF("vmar map failed %d\n", result);
    return fit::error(starnix::MemoryManager::get_errno_for_map_err(result));
  }

  return fit::ok(map_result);
}

}  // namespace

namespace starnix {

bool MemoryManagerState::Initialize(zx::vmar vmar, zx_info_vmar_t info,
                                    MemoryManagerForkableState state) {
  user_vmar_ = ktl::move(vmar);
  user_vmar_info_ = info;
  forkable_state_ = state;
  return true;
}

fit::result<Errno, UserAddress> MemoryManagerState::map_anonymous(
    DesiredAddress addr, size_t length, ProtectionFlags prot_flags, MappingOptionsFlags options,
    MappingName name, fbl::Vector<fbl::RefPtr<Mapping>>& released_mappings) {
  auto result = create_anonymous_mapping_vmo(length);
  if (result.is_error()) {
    return result.take_error();
  }
  auto arc_vmo = result.value();
  auto flags = MappingFlagsImpl::from_prot_flags_and_options(prot_flags, options);

  return map_vmo(addr, ktl::move(arc_vmo), 0, length, flags,
                 options.contains(MappingOptions::POPULATE), name, released_mappings);
}

fit::result<Errno, UserAddress> MemoryManagerState::map_internal(DesiredAddress addr,
                                                                 const zx::vmo& vmo,
                                                                 uint64_t vmo_offset, size_t length,
                                                                 MappingFlags flags,
                                                                 bool populate) {
  return map_in_vmar(user_vmar_, user_vmar_info_, addr, vmo, vmo_offset, length, flags, populate);
}

fit::result<Errno> MemoryManagerState::validate_addr(DesiredAddress addr, size_t length) {
  if (addr.type == DesiredAddressType::FixedOverwrite) {
    if (check_has_unauthorized_splits(addr.address, length)) {
      return fit::error(errno(ENOMEM));
    }
  }
  return fit::ok();
}

fit::result<Errno, UserAddress> MemoryManagerState::map_vmo(
    DesiredAddress addr, zx::ArcVmo vmo, uint64_t vmo_offset, size_t length, MappingFlags flags,
    bool populate, MappingName name, fbl::Vector<fbl::RefPtr<Mapping>>& released_mappings) {
  LTRACEF("length %zu flags 0x%x populate %s\n", length, flags.bits(), populate ? "true" : "false");
  auto va = validate_addr(addr, length);
  if (va.is_error())
    return va.take_error();

  auto mapped_addr = map_internal(addr, vmo->as_ref(), vmo_offset, length, flags, populate);
  if (mapped_addr.is_error())
    return mapped_addr.take_error();

  auto end = (mapped_addr.value() + length).round_up(PAGE_SIZE).value();
  if (addr.type == DesiredAddressType::FixedOverwrite) {
    ASSERT(addr.address == mapped_addr.value());
    auto uau = update_after_unmap(addr.address, end - addr.address, released_mappings);
    if (uau.is_error())
      return uau.take_error();
  }

  fbl::RefPtr<Mapping> mapping;
  if (zx_status_t status = Mapping::New(mapped_addr.value(), std::move(vmo), vmo_offset,
                                        MappingFlagsImpl(flags) /*, file_write_guard*/, &mapping);
      status != ZX_OK) {
    return fit::error(errno(from_status_like_fdio(status)));
  }
  DEBUG_ASSERT(mapping);
  mapping->name() = name;
  mappings_.insert({mapped_addr.value(), end}, mapping);

  if (flags.contains(MappingFlagsEnum::GROWSDOWN)) {
    // track_stub !(TODO("https://fxbug.dev/297373369"), "GROWSDOWN guard region");
  }

  return mapped_addr.take_value();
}

// Checks if an operation may be performed over the target mapping that may
// result in a split mapping.
//
// An operation may be forbidden if the target mapping only partially covers
// an existing mapping with the `MappingOptions::DONT_SPLIT` flag set.
bool MemoryManagerState::check_has_unauthorized_splits(UserAddress addr, size_t length) {
  /*
    let target_mapping = addr..addr.saturating_add(length);
    let mut intersection = self.mappings.intersection(target_mapping.clone());

    // A mapping is not OK if it disallows splitting and the target range
    // does not fully cover the mapping range.
    let check_if_mapping_has_unauthorized_split =
        |mapping: Option<(&Range<UserAddress>, &Mapping)>| {
            mapping.is_some_and(|(range, mapping)| {
                mapping.flags.contains(MappingFlags::DONT_SPLIT)
                    && (range.start < target_mapping.start || target_mapping.end < range.end)
            })
        };

    // We only check the first and last mappings in the range because naturally,
    // the mappings in the middle are fully covered by the target mapping and
    // won't be split.
    check_if_mapping_has_unauthorized_split(intersection.next())
        || check_if_mapping_has_unauthorized_split(intersection.last())
  */
  return false;
}

// Returns all the mappings starting at `addr`, and continuing until either `length` bytes have
// been covered or an unmapped page is reached.
//
// Mappings are returned in ascending order along with the number of bytes that intersect the
// requested range. The returned mappings are guaranteed to be contiguous and the total length
// corresponds to the number of contiguous mapped bytes starting from `addr`, i.e.:
// - 0 (empty iterator) if `addr` is not mapped.
// - exactly `length` if the requested range is fully mapped.
// - the offset of the first unmapped page (between 0 and `length`) if the requested range is
//   only partially mapped.
//
// Returns EFAULT if the requested range overflows or extends past the end of the vmar.
fit::result<Errno, fbl::Vector<ktl::pair<fbl::RefPtr<Mapping>, size_t>>>
MemoryManagerState::get_contiguous_mappings_at(UserAddress addr, size_t length) const {
  fbl::Vector<std::pair<fbl::RefPtr<Mapping>, size_t>> result;
  UserAddress end_addr;
  if (auto r = addr.checked_add(length); r) {
    end_addr = r.value();
  } else {
    return fit::error(errno(EFAULT));
  }

  if (end_addr > max_address()) {
    return fit::error(errno(EFAULT));
  }

  // Iterate over all contiguous mappings intersecting the requested range.
  auto mappings = mappings_.intersection(util::Range<UserAddress>({addr, end_addr}));
  std::optional<UserAddress> prev_range_end{};
  size_t offset = 0;
  for (const auto& pair : mappings) {
    if (offset != length) {
      auto [range, mapping] = pair;
      if (!prev_range_end.has_value() && range.start > addr) {
        // If this is the first mapping that we are considering, it may not actually
        // contain `addr` at all.
        continue;
      } else if (prev_range_end.has_value() && range.start != prev_range_end.value()) {
        // Subsequent mappings may not be contiguous.
        continue;
      } else {
        // This mapping can be returned.
        auto mapping_length = ktl::min(length, range.end - addr) - offset;
        offset += mapping_length;
        prev_range_end = range.end;
        fbl::AllocChecker ac;
        result.push_back(ktl::pair(mapping, mapping_length), &ac);
        if (!ac.check()) {
          return fit::error(errno(ENOMEM));
        }
      }
    }
  }
  LTRACEF_LEVEL(2, "found %zu mappings\n", result.size());
  return fit::ok(ktl::move(result));
}

UserAddress MemoryManagerState::max_address() const {
  return user_vmar_info_.base + user_vmar_info_.len;
}

// Unmaps the specified range. Unmapped mappings are placed in `released_mappings`.
fit::result<Errno> MemoryManagerState::unmap(UserAddress addr, size_t length,
                                             fbl::Vector<fbl::RefPtr<Mapping>>& released_mappings) {
  if (!addr.is_aligned(PAGE_SIZE)) {
    return fit::error(errno(EINVAL));
  }

  if (ROUNDUP_PAGE_SIZE(length) == 0) {
    return fit::error(errno(EINVAL));
  }

  if (check_has_unauthorized_splits(addr, length)) {
    return fit::error(errno(EINVAL));
  }

  // Unmap the range, including the the tail of any range that would have been split. This
  // operation is safe because we're operating on another process.
  zx_status_t status = user_vmar_.unmap(addr.ptr(), length);
  switch (status) {
    case ZX_OK:
    case ZX_ERR_NOT_FOUND:
      break;
    case ZX_ERR_INVALID_ARGS:
      return fit::error(errno(EINVAL));
    default:
      PANIC("encountered impossible error: %d", status);
  }

  return update_after_unmap(addr, length, released_mappings);
}

// Updates `self.mappings` after the specified range was unmaped.
//
// The range to unmap can span multiple mappings, and can split mappings if
// the range start or end falls in the middle of a mapping.
//
// For example, with this set of mappings and unmap range `R`:
//
//   [  A  ][ B ] [    C    ]     <- mappings
//      |-------------|           <- unmap range R
//
// Assuming the mappings are all MAP_ANONYMOUS:
// - the pages of A, B, and C that fall in range R are unmapped; the VMO backing B is dropped.
// - the VMO backing A is shrunk.
// - a COW child VMO is created from C, which is mapped in the range of C that falls outside R.
//
// File-backed mappings don't need to have their VMOs modified.
//
// Unmapped mappings are placed in `released_mappings`.
fit::result<Errno> MemoryManagerState::update_after_unmap(
    UserAddress addr, size_t length, fbl::Vector<fbl::RefPtr<Mapping>>& released_mappings) {
  UserAddress end_addr;
  if (auto result = addr.checked_add(length); result) {
    end_addr = result.value();
  } else {
    return fit::error(errno(EINVAL));
  }

#if STARNIX_ANON_ALLOCS
/*
  #[cfg(feature = "alternate_anon_allocs")]
  {
      let unmap_range = addr..end_addr;
      for (range, mapping) in self.mappings.intersection(&unmap_range) {
          // Deallocate any pages in the private, anonymous backing that are now unreachable.
          if let MappingBacking::PrivateAnonymous = mapping.backing {
              let unmapped_range = &unmap_range.intersect(range);
              self.private_anonymous
                  .zero(unmapped_range.start, unmapped_range.end - unmapped_range.start)?;
          }
      }
      released_mappings.extend(self.mappings.remove(&unmap_range));
      return Ok(());
  }
*/
#else

  // Find the private, anonymous mapping that will get its tail cut off by this unmap call.
  auto truncated_head =
      [&]() -> std::optional<std::pair<util::Range<UserAddress>, fbl::RefPtr<Mapping>>> {
    if (auto pair = mappings_.get(addr); pair) {
      if (pair->first.start != addr && pair->second->private_anonymous()) {
        return std::make_pair(util::Range<UserAddress>({pair->first.start, addr}), pair->second);
      }
    }
    return std::nullopt;
  }();

  // Find the private, anonymous mapping that will get its head cut off by this unmap call.
  auto truncated_tail =
      [&]() -> std::optional<std::pair<util::Range<UserAddress>, fbl::RefPtr<Mapping>>> {
    if (auto pair = mappings_.get(end_addr); pair) {
      if (pair->first.end != end_addr && pair->second->private_anonymous()) {
        return std::make_pair(util::Range<UserAddress>({end_addr, pair->first.end}), pair->second);
      }
    }
    return std::nullopt;
  }();

  // Remove the original range of mappings from our map.
  auto vec = mappings_.remove(util::Range<UserAddress>({addr, end_addr}));
  ktl::copy(vec.begin(), vec.end(), util::back_inserter(released_mappings));

  if (truncated_tail) {
    auto [range, mapping] = truncated_tail.value();

    auto& backing = std::get<MappingBackingVmo>(mapping->backing().variant);
    // Create and map a child COW VMO mapping that represents the truncated tail.
    auto child_vmo_offset = static_cast<uint64_t>(range.start - backing.base) + backing.vmo_offset;
    auto child_length = range.end - range.start;
    zx::vmo child_vmo;
    if (zx_status_t status =
            backing.vmo->as_ref().create_child(ZX_VMO_CHILD_SNAPSHOT | ZX_VMO_CHILD_RESIZABLE,
                                               child_vmo_offset, child_length, &child_vmo);
        status != ZX_OK) {
      return fit::error(MemoryManager::get_errno_for_map_err(status));
    }

    // Update the mapping.
    fbl::AllocChecker ac;
    backing.vmo = fbl::MakeRefCountedChecked<zx::Arc<zx::vmo>>(&ac, ktl::move(child_vmo));
    backing.base = range.start;
    backing.vmo_offset = 0;

    auto result = map_internal({DesiredAddressType::FixedOverwrite, range.start},
                               backing.vmo->as_ref(), 0, child_length, mapping->flags(), false);
    if (result.is_error()) {
      return result.take_error();
    }

    // Replace the mapping with a new one that contains updated VMO handle.
    mappings_.insert(range, mapping);
  }

  if (truncated_head) {
    util::Range<UserAddress> range;
    fbl::RefPtr<Mapping> mapping;
    std::tie(range, mapping) = truncated_head.value();
    auto& backing = std::get<MappingBackingVmo>(mapping->backing().variant);

    // Resize the VMO of the head mapping, whose tail was cut off.
    auto new_mapping_size = static_cast<uint64_t>(range.end - range.start);
    auto new_vmo_size = backing.vmo_offset + new_mapping_size;
    if (zx_status_t status = backing.vmo->as_ref().set_size(new_vmo_size); status != ZX_OK) {
      return fit::error(MemoryManager::get_errno_for_map_err(status));
    }
  }
  return fit::ok();
#endif
}

fit::result<Errno> MemoryManagerState::protect(UserAddress addr, size_t length,
                                               ProtectionFlags prot_flags) {
  return fit::error(errno(ENOSYS));
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManagerState::read_memory(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  size_t bytes_written = 0;
  auto vec = get_contiguous_mappings_at(addr, bytes.size());
  if (vec.is_error()) {
    LTRACEF_LEVEL(2, "error code %d\n", vec.error_value().error_code());
    return vec.take_error();
  }

  for (auto [mapping, len] : vec.value()) {
    auto next_offset = bytes_written + len;
    ktl::span<uint8_t> current_span{bytes.data() + bytes_written, next_offset};
    auto result = read_mapping_memory(addr + bytes_written, mapping, current_span);
    if (result.is_error())
      return result.take_error();
    bytes_written = next_offset;
  }

  if (bytes_written != bytes.size()) {
    return fit::error(errno(EFAULT));
  } else {
    return fit::ok(bytes);
  }
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManagerState::read_mapping_memory(
    UserAddress addr, fbl::RefPtr<Mapping>& mapping, ktl::span<uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  if (!mapping->can_read()) {
    return fit::error(errno(EFAULT));
  }

  return std::visit(MappingBacking::overloaded{
                        [](PrivateAnonymous&) {
                          return fit::result<Errno, ktl::span<uint8_t>>(fit::error(errno(EFAULT)));
                        },
                        [&](MappingBackingVmo& m) { return m.read_memory(addr, bytes); },
                    },
                    mapping->backing().variant);
}

fit::result<Errno, size_t> MemoryManagerState::write_memory(
    UserAddress addr, const ktl::span<const uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  size_t bytes_written = 0;
  auto vec = get_contiguous_mappings_at(addr, bytes.size());
  if (vec.is_error()) {
    LTRACEF_LEVEL(2, "error code %d\n", vec.error_value().error_code());
    return vec.take_error();
  }

  for (auto [mapping, len] : vec.value()) {
    auto next_offset = bytes_written + len;
    auto result = write_mapping_memory(addr + bytes_written, mapping,
                                       {bytes.data() + bytes_written, next_offset});
    if (result.is_error())
      return result.take_error();
    bytes_written = next_offset;
  }

  if (bytes_written != bytes.size()) {
    return fit::error(errno(EFAULT));
  } else {
    return fit::ok(bytes.size());
  }
}

fit::result<Errno> MemoryManagerState::write_mapping_memory(
    UserAddress addr, fbl::RefPtr<Mapping>& mapping, const ktl::span<const uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  if (!mapping->can_write()) {
    return fit::error(errno(EFAULT));
  }

  return std::visit(
      MappingBacking::overloaded{
          [](PrivateAnonymous&) { return fit::result<Errno>(fit::error(errno(EFAULT))); },
          [&](MappingBackingVmo& m) { return m.write_memory(addr, bytes); },
      },
      mapping->backing().variant);
}

fit::result<Errno, ktl::span<uint8_t>> MappingBackingVmo::read_memory(UserAddress addr,
                                                                      ktl::span<uint8_t>& bytes) {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  if (zx_status_t status = vmo->as_ref().read(bytes.data(), address_to_offset(addr), bytes.size());
      status != ZX_OK) {
    return fit::error(errno(EFAULT));
  }
  return fit::ok(bytes);
}

fit::result<Errno> MappingBackingVmo::write_memory(UserAddress addr,
                                                   const ktl::span<const uint8_t>& bytes) {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  if (zx_status_t status = vmo->as_ref().write(bytes.data(), address_to_offset(addr), bytes.size());
      status != ZX_OK) {
    LTRACEF("error to write in vmo %d\n", status);
    return fit::error(errno(EFAULT));
  }
  return fit::ok();
}

uint64_t MappingBackingVmo::address_to_offset(UserAddress addr) {
  return static_cast<uint64_t>(addr.ptr() - base.ptr()) + vmo_offset;
}

zx_status_t Mapping::New(UserAddress base, zx::ArcVmo vmo, uint64_t vmo_offset,
                         MappingFlagsImpl flags, fbl::RefPtr<Mapping>* out) {
  LTRACEF("base 0x%lx vmo %p vmo_offset %lu flags 0x%x\n", base.ptr(), vmo.get(), vmo_offset,
          flags.bits());
  fbl::AllocChecker ac;
  fbl::RefPtr<Mapping> mapping = fbl::AdoptRef(
      new (&ac) Mapping(base, std::move(vmo), vmo_offset, flags, {MappingNameType::None}));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  *out = ktl::move(mapping);
  return ZX_OK;
}

Mapping::Mapping(UserAddress base, zx::ArcVmo vmo, uint64_t vmo_offset, MappingFlagsImpl flags,
                 MappingName name)
    : backing_(MappingBackingVmo({base, std::move(vmo), vmo_offset})), flags_(flags), name_(name) {}

Errno MemoryManager::get_errno_for_map_err(zx_status_t status) {
  switch (status) {
    case ZX_ERR_INVALID_ARGS:
      return errno(EINVAL);
    case ZX_ERR_ACCESS_DENIED:
      return errno(EPERM);
    case ZX_ERR_NOT_SUPPORTED:
      return errno(ENODEV);
    case ZX_ERR_NO_MEMORY:
      return errno(ENOMEM);
    case ZX_ERR_NO_RESOURCES:
      return errno(ENOMEM);
    case ZX_ERR_OUT_OF_RANGE:
      return errno(ENOMEM);
    case ZX_ERR_ALREADY_EXISTS:
      return errno(EEXIST);
    case ZX_ERR_BAD_STATE:
      return errno(EINVAL);
    default:
      PANIC("encountered impossible error: %d", status);
  }
}

fit::result<Errno, UserAddress> MemoryManager::map_vmo(DesiredAddress addr, zx::ArcVmo vmo,
                                                       uint64_t vmo_offset, size_t length,
                                                       ProtectionFlags prot_flags,
                                                       MappingOptionsFlags options,
                                                       MappingName name) {
  auto flags = MappingFlagsImpl::from_prot_flags_and_options(prot_flags, options);
  //  Unmapped mappings must be released after the state is unlocked.
  fbl::Vector<fbl::RefPtr<Mapping>> released_mappings;
  Guard<Mutex> lock(mm_state_rw_lock());
  auto result = state_.map_vmo(addr, vmo, vmo_offset, length, flags,
                               options.contains(MappingOptions::POPULATE), name, released_mappings);

  // Drop the state before the unmapped mappings, since dropping a mapping may acquire a lock
  // in `DirEntry`'s `drop`.
  lock.Release();
  released_mappings.reset();

  return result;
}

zx_status_t MemoryManager::New(zx::vmar root_vmar, fbl::RefPtr<MemoryManager>* out) {
  LTRACE;
  zx_info_vmar_t info;
  zx_status_t status = root_vmar.get_info(ZX_INFO_VMAR, &info, sizeof(info), nullptr, nullptr);
  if (status != ZX_OK) {
    return status;
  }

  auto user_vmar = create_user_vmar(root_vmar, info);
  if (user_vmar.is_error()) {
    return user_vmar.error_value();
  }

  zx_info_vmar_t user_vmar_info;
  status =
      user_vmar->get_info(ZX_INFO_VMAR, &user_vmar_info, sizeof(user_vmar_info), nullptr, nullptr);
  if (status != ZX_OK) {
    return status;
  }

  DEBUG_ASSERT(PRIVATE_ASPACE_BASE == user_vmar_info.base);
  DEBUG_ASSERT(PRIVATE_ASPACE_SIZE == user_vmar_info.len);

  fbl::AllocChecker ac;
  fbl::RefPtr<MemoryManager> mm = fbl::AdoptRef(
      new (&ac) MemoryManager(std::move(root_vmar), std::move(*user_vmar), user_vmar_info));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  *out = std::move(mm);
  return ZX_OK;
}

MemoryManager::MemoryManager(zx::vmar root, zx::vmar user_vmar, zx_info_vmar_t user_vmar_info)
    : root_vmar(ktl::move(root)), base_addr(UserAddress::from_ptr(user_vmar_info.base)) {
  LTRACE;
  state_.Initialize(std::move(user_vmar), user_vmar_info,
                    MemoryManagerForkableState{{}, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0});
}

fit::result<Errno, UserAddress> MemoryManager::set_brk(const CurrentTask& current_task,
                                                       UserAddress addr) {
  // TODO (Herrera) use rlimit from current process
  uint64_t rlimit_data = ktl::min(kProgramBreakLimit, static_cast<uint64_t>(-1));

  fbl::Vector<fbl::RefPtr<Mapping>> released_mappings;
  Guard<Mutex> lock(&mm_state_rw_lock_);
  ProgramBreak brk;
  if (!state_->brk.has_value()) {
    zx::vmo vmo;
    zx_status_t status = zx::vmo::create(kProgramBreakLimit, 0, &vmo);
    if (status != ZX_OK) {
      return fit::error(errno(ENOMEM));
    }
    status = vmo.set_property(ZX_PROP_NAME, "starnix-brk", strlen("starnix-brk"));
    DEBUG_ASSERT(status == ZX_OK);
    size_t length = PAGE_SIZE;
    auto flags = MappingFlags(MappingFlagsEnum::READ) | MappingFlags(MappingFlagsEnum::WRITE);

    fbl::AllocChecker ac;
    auto arc_vmo = fbl::MakeRefCountedChecked<zx::Arc<zx::vmo>>(&ac, ktl::move(vmo));
    if (!ac.check()) {
      return fit::error(errno(ENOMEM));
    }

    auto map_addr =
        state_.map_vmo(DesiredAddress{DesiredAddressType::Any}, arc_vmo, 0, length, flags, false,
                       MappingName{MappingNameType::Heap}, released_mappings);
    if (map_addr.is_error()) {
      return map_addr.take_error();
    }
    state_->brk = brk = ProgramBreak{map_addr.value(), map_addr.value()};
  } else {
    brk = state_->brk.value();
  }

  if ((addr < brk.base) || (addr > (brk.base + rlimit_data))) {
    // The requested program break is out-of-range. We're supposed to simply
    // return the current program break.
    return fit::ok(brk.current);
  }

  auto opt = state_.mappings_.get(brk.current);
  if (!opt.has_value()) {
    return fit::error(errno(EFAULT));
  }

  auto [range, mapping] = opt.value();

  brk.current = addr;

  auto old_end = range.end;
  auto new_end = (brk.current + 1ul).round_up(PAGE_SIZE).value();

  if (new_end < old_end) {
    // We've been asked to free memory.
    auto delta = old_end - new_end;
    auto vmo_offset = old_end - brk.base;

    zx_status_t status = std::visit(MappingBacking::overloaded{
                                        [](PrivateAnonymous&) { return ZX_ERR_NOT_SUPPORTED; },
                                        [&](MappingBackingVmo& m) {
                                          return m.vmo->as_ref().op_range(
                                              ZX_VMO_OP_ZERO, vmo_offset, delta, nullptr, 0);
                                        },
                                    },
                                    mapping->backing().variant);

    if (status != ZX_OK) {
      PANIC("encountered impossible error: %d", status);
    }
    auto result = state_.unmap(new_end, delta, released_mappings);
    if (result.is_error())
      return result.take_error();

  } else if (new_end > old_end) {
    // We've been asked to map more memory.
    auto delta = new_end - old_end;
    auto vmo_offset = old_end - brk.base;

    zx_status_t status =
        std::visit(MappingBacking::overloaded{
                       [](PrivateAnonymous&) { return ZX_ERR_NOT_SUPPORTED; },
                       [&](MappingBackingVmo& m) TA_REQ(mm_state_rw_lock_) {
                         return state_.user_vmar().map(
                             ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_SPECIFIC,
                             old_end - base_addr, m.vmo->as_ref(), vmo_offset, delta, nullptr);
                       },
                   },
                   mapping->backing().variant);

    if (status == ZX_OK) {
      state_.mappings_.insert({brk.base, new_end}, mapping);
    } else {
      state_.mappings_.insert({brk.base, old_end}, mapping);
      return fit::error(get_errno_for_map_err(status));
    }
  }

  state_->brk = brk;
  return fit::ok(brk.current);
}

fit::result<zx_status_t> MemoryManager::exec(/*NamespaceNode exe_node*/) {
  LTRACE;
  // The previous mapping should be dropped only after the lock to state is released to
  // prevent lock order inversion.
  {
    Guard<Mutex> lock(&mm_state_rw_lock_);
    zx_info_vmar_t info;
    zx_status_t status = root_vmar.get_info(ZX_INFO_VMAR, &info, sizeof(info), nullptr, nullptr);
    if (status != ZX_OK) {
      return fit::error(status);
    }

    // SAFETY: This operation is safe because the VMAR is for another process.
    status = state_.user_vmar_.destroy();
    if (status != ZX_OK) {
      return fit::error(status);
    }

    auto user_vmar = create_user_vmar(root_vmar, info);
    if (user_vmar.is_error()) {
      return user_vmar.take_error();
    }

    state_.user_vmar_ = ktl::move(user_vmar.value());
    zx_info_vmar_t user_vmar_info;
    status = state_.user_vmar_.get_info(ZX_INFO_VMAR, &user_vmar_info, sizeof(user_vmar_info),
                                        nullptr, nullptr);
    if (status != ZX_OK) {
      return fit::error(status);
    }

    state_.user_vmar_info_ = user_vmar_info;
    state_->brk = {};
    // state.executable_node = Some(exe_node);

    state_.mappings_.clear();
  }
  return fit::ok();
}

// impl MemoryAccessor for MemoryManager
fit::result<Errno, ktl::span<uint8_t>> MemoryManager::read_memory(UserAddress addr,
                                                                  ktl::span<uint8_t>& bytes) const {
  return vmo_read_memory(addr, bytes);
}

fit::result<Errno, size_t> MemoryManager::write_memory(
    UserAddress addr, const ktl::span<const uint8_t>& bytes) const {
  return vmo_write_memory(addr, bytes);
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManager::unified_read_memory(
    const CurrentTask& current_task, UserAddress addr, ktl::span<uint8_t>& bytes) const {
  DEBUG_ASSERT(has_same_address_space(current_task->mm()));

  if (ThreadDispatcher::GetCurrent()) {
    user_in_ptr<const uint8_t> user_ptr =
        make_user_in_ptr(reinterpret_cast<const uint8_t*>(addr.ptr()));

    if (zx_status_t status = user_ptr.copy_array_from_user(bytes.data(), bytes.size());
        status != ZX_OK) {
      return fit::error(errno(from_status_like_fdio(status)));
    }
    return fit::ok(bytes);
  } else {
    return vmo_read_memory(addr, bytes);
  }
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManager::vmo_read_memory(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  Guard<Mutex> lock(&mm_state_rw_lock_);
  return state_.read_memory(addr, bytes);
}

fit::result<Errno, size_t> MemoryManager::unified_write_memory(
    const CurrentTask& current_task, UserAddress addr,
    const ktl::span<const uint8_t>& bytes) const {
  DEBUG_ASSERT(has_same_address_space(current_task->mm()));

  if (ThreadDispatcher::GetCurrent()) {
    user_out_ptr<uint8_t> user_ptr = make_user_out_ptr(reinterpret_cast<uint8_t*>(addr.ptr()));
    if (zx_status_t status = user_ptr.copy_array_to_user(bytes.data(), bytes.size());
        status != ZX_OK) {
      return fit::error(errno(from_status_like_fdio(status)));
    }
    return fit::ok(bytes.size());
  } else {
    return vmo_write_memory(addr, bytes);
  }
}

fit::result<Errno, size_t> MemoryManager::vmo_write_memory(
    UserAddress addr, const ktl::span<const uint8_t>& bytes) const {
  Guard<Mutex> lock(&mm_state_rw_lock_);
  return state_.write_memory(addr, bytes);
}

fit::result<Errno> MemoryManager::unmap(UserAddress addr, size_t length) {
  fbl::Vector<fbl::RefPtr<Mapping>> released_mappings;
  Guard<Mutex> lock(&mm_state_rw_lock_);
  return state_.unmap(addr, length, released_mappings);
}

fit::result<Errno> MemoryManager::protect(UserAddress addr, size_t length,
                                          ProtectionFlags prot_flags) {
  Guard<Mutex> lock(&mm_state_rw_lock_);
  return state_.protect(addr, length, prot_flags);
}

fit::result<Errno, UserAddress> MemoryManager::map_anonymous(DesiredAddress addr, size_t length,
                                                             ProtectionFlags prot_flags,
                                                             MappingOptionsFlags options,
                                                             MappingName name) {
  LTRACE;
  fbl::Vector<fbl::RefPtr<Mapping>> released_mappings;
  Guard<Mutex> lock(&mm_state_rw_lock_);
  return state_.map_anonymous(addr, length, prot_flags, options, name, released_mappings);
}

size_t MemoryManager::get_mapping_count() {
  Guard<Mutex> lock(&mm_state_rw_lock_);
  return state_.mappings_.iter().size();
}

UserAddress MemoryManager::get_random_base(size_t length) {
  Guard<Mutex> lock(&mm_state_rw_lock_);
  // Allocate a vmar of the correct size, get the random location, then immediately destroy it.
  // This randomizes the load address without loading into a sub-vmar and breaking mprotect.
  // This is different from how Linux actually lays out the address space. We might need to
  // rewrite it eventually.
  zx::vmar temp_vmar;
  uintptr_t base;
  ASSERT(state_.user_vmar().allocate(0, 0, length, &temp_vmar, &base) == ZX_OK);
  ASSERT(temp_vmar.destroy() == ZX_OK);
  return UserAddress::from_ptr(base);
}

// Creates a VMO that can be used in an anonymous mapping for the `mmap`
// syscall.
fit::result<Errno, zx::ArcVmo> create_anonymous_mapping_vmo(uint64_t size) {
  LTRACEF("size 0x%lx\n", size);
  // mremap can grow memory regions, so make sure the VMO is resizable.
  zx::vmo vmo;
  zx_status_t result = zx::vmo::create(size, ZX_VMO_RESIZABLE, &vmo);
  switch (result) {
    case ZX_OK:
      break;
    case ZX_ERR_NO_MEMORY:
    case ZX_ERR_OUT_OF_RANGE:
      return fit::error<Errno>(errno(ENOMEM));
    default:
      PANIC("encountered impossible error: %d", result);
  }

  result = vmo.set_property(ZX_PROP_NAME, "starnix-anon", strlen("starnix-anon"));
  DEBUG_ASSERT(result == ZX_OK);
  // TODO(https://fxbug.dev/42056890): Audit replace_as_executable usage
  result = vmo.replace_as_executable({}, &vmo);
  if (result != ZX_OK) {
    PANIC("encountered impossible error: %d", result);
  }

  fbl::AllocChecker ac;
  zx::ArcVmo arc_vmo = fbl::AdoptRef(new (&ac) zx::Arc<zx::vmo>(ktl::move(vmo)));
  if (!ac.check()) {
    return fit::error<Errno>(errno(ENOMEM));
  }

  return fit::ok(ktl::move(arc_vmo));
}

}  // namespace starnix
