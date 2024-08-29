// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/mm/memory_manager.h"

#include <align.h>
#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/logging/logging.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/back_insert_iterator.h>
#include <lib/mistos/util/range-map.h>
#include <lib/mistos/zx/handle.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>
#include <lib/user_copy/user_ptr.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/types.h>

#include <optional>
#include <utility>
#include <variant>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <ktl/iterator.h>
#include <ktl/optional.h>
#include <object/thread_dispatcher.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

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

fit::result<Errno, UserAddress> MemoryManagerState::map_anonymous(
    DesiredAddress addr, size_t length, ProtectionFlags prot_flags, MappingOptionsFlags options,
    MappingName name, fbl::Vector<Mapping>& released_mappings) {
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
  return map_in_vmar(user_vmar, user_vmar_info, addr, vmo, vmo_offset, length, flags, populate);
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
    bool populate, MappingName name, fbl::Vector<Mapping>& released_mappings) {
  LTRACEF("addr 0x%lx length %zu flags 0x%x populate %s\n", addr.address.ptr(), length,
          flags.bits(), populate ? "true" : "false");
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

  auto mapping = Mapping::New(mapped_addr.value(), ktl::move(vmo), vmo_offset,
                              MappingFlagsImpl(flags) /*, file_write_guard*/);
  mapping.name_ = name;
  mappings.insert({mapped_addr.value(), end}, mapping);

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
fit::result<Errno, fbl::Vector<ktl::pair<Mapping, size_t>>>
MemoryManagerState::get_contiguous_mappings_at(UserAddress addr, size_t length) const {
  fbl::Vector<ktl::pair<Mapping, size_t>> result;
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
  auto _mappings = mappings.intersection(util::Range<UserAddress>({addr, end_addr}));
  ktl::optional<UserAddress> prev_range_end{};
  size_t offset = 0;
  for (const auto& pair : _mappings) {
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
  return user_vmar_info.base + user_vmar_info.len;
}

// Unmaps the specified range. Unmapped mappings are placed in `released_mappings`.
fit::result<Errno> MemoryManagerState::unmap(UserAddress addr, size_t length,
                                             fbl::Vector<Mapping>& released_mappings) {
  if (!addr.is_aligned(PAGE_SIZE)) {
    return fit::error(errno(EINVAL));
  }

  auto length_or_error = round_up_to_system_page_size(length);
  if (length_or_error.is_error()) {
    return length_or_error.take_error();
  }
  if (length_or_error.value() == 0) {
    return fit::error(errno(EINVAL));
  }

  if (check_has_unauthorized_splits(addr, length)) {
    return fit::error(errno(EINVAL));
  }

  // Unmap the range, including the the tail of any range that would have been split. This
  // operation is safe because we're operating on another process.
  zx_status_t status = user_vmar.unmap(addr.ptr(), length_or_error.value());
  switch (status) {
    case ZX_OK:
    case ZX_ERR_NOT_FOUND:
      break;
    case ZX_ERR_INVALID_ARGS:
      return fit::error(errno(EINVAL));
    default:
      return fit::error<Errno>(impossible_error(status));
  }

  auto result = update_after_unmap(addr, length_or_error.value(), released_mappings);
  if (result.is_error())
    return result.take_error();

  return fit::ok();
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
fit::result<Errno> MemoryManagerState::update_after_unmap(UserAddress addr, size_t length,
                                                          fbl::Vector<Mapping>& released_mappings) {
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
  auto truncated_head = [&]() -> ktl::optional<ktl::pair<util::Range<UserAddress>, Mapping>> {
    if (auto pair = mappings.get(addr); pair) {
      auto& [range, mapping] = pair.value();
      if (range.start != addr && mapping.private_anonymous()) {
        return ktl::pair(util::Range<UserAddress>{range.start, addr}, mapping);
      }
    }
    return ktl::nullopt;
  }();

  // Find the private, anonymous mapping that will get its head cut off by this unmap call.
  auto truncated_tail = [&]() -> ktl::optional<ktl::pair<util::Range<UserAddress>, Mapping>> {
    if (auto pair = mappings.get(end_addr); pair) {
      auto& [range, mapping] = pair.value();
      if (range.end != end_addr && mapping.private_anonymous()) {
        return ktl::pair(util::Range<UserAddress>{end_addr, range.end}, mapping);
      }
    }
    return ktl::nullopt;
  }();

  // Remove the original range of mappings from our map.
  auto vec = mappings.remove(util::Range<UserAddress>{addr, end_addr});
  ktl::copy(vec.begin(), vec.end(), util::back_inserter(released_mappings));

  if (truncated_tail) {
    auto [range, mapping] = truncated_tail.value();
    auto& backing = ktl::get<MappingBackingVmo>(mapping.backing_.variant);
    // Create and map a child COW VMO mapping that represents the truncated tail.
    zx_info_handle_basic_t vmo_info;
    zx_status_t status = backing.vmo_->as_ref().get_info(ZX_INFO_HANDLE_BASIC, &vmo_info,
                                                         sizeof(vmo_info), nullptr, nullptr);
    if (status != ZX_OK) {
      impossible_error(status);
    }
    auto child_vmo_offset =
        static_cast<uint64_t>(range.start - backing.base_) + backing.vmo_offset_;
    auto child_length = range.end - range.start;
    zx::vmo child_vmo;
    if (status = backing.vmo_->as_ref().create_child(ZX_VMO_CHILD_SNAPSHOT | ZX_VMO_CHILD_RESIZABLE,
                                                     child_vmo_offset, child_length, &child_vmo);
        status != ZX_OK) {
      return fit::error(MemoryManager::get_errno_for_map_err(status));
    }

    if ((vmo_info.rights & ZX_RIGHT_EXECUTE) == ZX_RIGHT_EXECUTE) {
      status = child_vmo.replace_as_executable({}, &child_vmo);
      if (status != ZX_OK) {
        impossible_error(status);
      }
    }

    // Update the mapping.
    fbl::AllocChecker ac;
    backing.vmo_ = fbl::MakeRefCountedChecked<zx::Arc<zx::vmo>>(&ac, ktl::move(child_vmo));
    ASSERT(ac.check());
    backing.base_ = range.start;
    backing.vmo_offset_ = 0;

    auto result = map_internal({DesiredAddressType::FixedOverwrite, range.start},
                               backing.vmo_->as_ref(), 0, child_length, mapping.flags_, false);
    if (result.is_error()) {
      return result.take_error();
    }

    // Replace the mapping with a new one that contains updated VMO handle.
    mappings.insert(range, mapping);
  }

  if (truncated_head) {
    auto [range, mapping] = truncated_head.value();
    auto& backing = ktl::get<MappingBackingVmo>(mapping.backing_.variant);

    // Resize the VMO of the head mapping, whose tail was cut off.
    auto new_mapping_size = static_cast<uint64_t>(range.end - range.start);
    auto new_vmo_size = backing.vmo_offset_ + new_mapping_size;
    if (zx_status_t status = backing.vmo_->as_ref().set_size(new_vmo_size); status != ZX_OK) {
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
  size_t bytes_read = 0;
  auto vec = get_contiguous_mappings_at(addr, bytes.size());
  if (vec.is_error()) {
    LTRACEF_LEVEL(2, "error code %d\n", vec.error_value().error_code());
    return vec.take_error();
  }

  for (auto [mapping, len] : vec.value()) {
    auto next_offset = bytes_read + len;
    ktl::span<uint8_t> span{bytes.data() + bytes_read, bytes.data() + next_offset};
    auto result = read_mapping_memory(addr + bytes_read, mapping, span);
    if (result.is_error())
      return result.take_error();
    bytes_read = next_offset;
  }

  if (bytes_read != bytes.size()) {
    return fit::error(errno(EFAULT));
  } else {
    LTRACEF("bytes read %lu\n", bytes_read);
    return fit::ok(ktl::span<uint8_t>{bytes.data(), bytes_read});
  }
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManagerState::read_mapping_memory(
    UserAddress addr, const Mapping& mapping, ktl::span<uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  if (!mapping.can_read()) {
    return fit::error(errno(EFAULT));
  }

  return ktl::visit(MappingBacking::overloaded{
                        [](const PrivateAnonymous&) {
                          return fit::result<Errno, ktl::span<uint8_t>>(fit::error(errno(EFAULT)));
                        },
                        [&](const MappingBackingVmo& m) { return m.read_memory(addr, bytes); },
                    },
                    mapping.backing_.variant);
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManagerState::read_memory_partial(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  size_t bytes_read = 0;
  auto mappings_or_error = get_contiguous_mappings_at(addr, bytes.size());
  if (mappings_or_error.is_error()) {
    LTRACEF_LEVEL(2, "error code %d\n", mappings_or_error.error_value().error_code());
    return mappings_or_error.take_error();
  }
  for (auto [mapping, len] : mappings_or_error.value()) {
    auto next_offset = bytes_read + len;
    ktl::span<uint8_t> span{bytes.data() + bytes_read, bytes.data() + next_offset};
    if (read_mapping_memory(addr + bytes_read, mapping, span).is_error()) {
      break;
    }
    bytes_read = next_offset;
  }

  // If at least one byte was requested but we got none, it means that `addr` was invalid.
  if ((bytes.size() != 0) && (bytes_read == 0)) {
    return fit::error(errno(EFAULT));
  } else {
    LTRACEF("bytes read %lu\n", bytes_read);
    return fit::ok(ktl::span<uint8_t>(bytes.data(), bytes_read));
  }
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManagerState::read_memory_partial_until_null_byte(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  auto read_bytes_or_error = read_memory_partial(addr, bytes);
  if (read_bytes_or_error.is_error())
    return read_bytes_or_error.take_error();

  auto read_bytes = read_bytes_or_error.value();
  auto null_position = ktl::find(read_bytes.begin(), read_bytes.end(), '\0');

  size_t max_len = (null_position == read_bytes.end())
                       ? read_bytes.size()
                       : ktl::distance(read_bytes.begin(), null_position) + 1;

  return fit::ok(ktl::span<uint8_t>(bytes.data(), max_len));
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
    LTRACEF_LEVEL(2, "len %zu\n", len);
    auto next_offset = bytes_written + len;
    auto result = write_mapping_memory(addr + bytes_written, mapping,
                                       {bytes.data() + bytes_written, bytes.data() + next_offset});
    if (result.is_error())
      return result.take_error();
    bytes_written = next_offset;
  }

  if (bytes_written != bytes.size()) {
    LTRACEF_LEVEL(2, "bytes_written %zu bytes.size() %zu\n", bytes_written, bytes.size());
    return fit::error(errno(EFAULT));
  } else {
    return fit::ok(bytes.size());
  }
}

fit::result<Errno> MemoryManagerState::write_mapping_memory(
    UserAddress addr, const Mapping& mapping, const ktl::span<const uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  if (!mapping.can_write()) {
    return fit::error(errno(EFAULT));
  }

  return ktl::visit(
      MappingBacking::overloaded{
          [](const PrivateAnonymous&) { return fit::result<Errno>(fit::error(errno(EFAULT))); },
          [&](const MappingBackingVmo& m) { return m.write_memory(addr, bytes); },
      },
      mapping.backing_.variant);
}

fit::result<Errno, ktl::span<uint8_t>> MappingBackingVmo::read_memory(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  if (zx_status_t status = vmo_->as_ref().read(bytes.data(), address_to_offset(addr), bytes.size());
      status != ZX_OK) {
    return fit::error(errno(EFAULT));
  }
  return fit::ok(bytes);
}

fit::result<Errno> MappingBackingVmo::write_memory(UserAddress addr,
                                                   const ktl::span<const uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  if (zx_status_t status =
          vmo_->as_ref().write(bytes.data(), address_to_offset(addr), bytes.size());
      status != ZX_OK) {
    LTRACEF("error to write in vmo %d\n", status);
    return fit::error(errno(EFAULT));
  }
  return fit::ok();
}

uint64_t MappingBackingVmo::address_to_offset(UserAddress addr) const {
  return static_cast<uint64_t>(addr.ptr() - base_.ptr()) + vmo_offset_;
}

Mapping Mapping::New(UserAddress base, zx::ArcVmo vmo, uint64_t vmo_offset,
                     MappingFlagsImpl flags) {
  LTRACEF("base 0x%lx vmo %p vmo_offset %lu flags 0x%x\n", base.ptr(), vmo.get(), vmo_offset,
          flags.bits());
  return Mapping(base, ktl::move(vmo), vmo_offset, flags, {MappingNameType::None});
}

Mapping::Mapping(UserAddress base, zx::ArcVmo vmo, uint64_t vmo_offset, MappingFlagsImpl flags,
                 MappingName name)
    : backing_(MappingBackingVmo(base, ktl::move(vmo), vmo_offset)),
      flags_(flags),
      name_(ktl::move(name)) {}

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
      return impossible_error(status);
  }
}

fit::result<Errno, UserAddress> MemoryManager::map_vmo(DesiredAddress addr, zx::ArcVmo vmo,
                                                       uint64_t vmo_offset, size_t length,
                                                       ProtectionFlags prot_flags,
                                                       MappingOptionsFlags options,
                                                       MappingName name) {
  auto flags = MappingFlagsImpl::from_prot_flags_and_options(prot_flags, options);

  //  Unmapped mappings must be released after the state is unlocked.
  fbl::Vector<Mapping> released_mappings;
  fit::result<Errno, UserAddress> result = fit::error(errno(EINVAL));

  {
    auto _state = state.Write();
    result = _state->map_vmo(addr, vmo, vmo_offset, length, flags,
                             options.contains(MappingOptions::POPULATE), name, released_mappings);
    if (result.is_error())
      return result.take_error();
  }

  // Drop the state before the unmapped mappings, since dropping a mapping may acquire a lock
  // in `DirEntry`'s `drop`.
  released_mappings.reset();

  return result;
}

fit::result<zx_status_t, fbl::RefPtr<MemoryManager>> MemoryManager::New(zx::vmar root_vmar) {
  LTRACE;
  zx_info_vmar_t info;
  zx_status_t status = root_vmar.get_info(ZX_INFO_VMAR, &info, sizeof(info), nullptr, nullptr);
  if (status != ZX_OK) {
    return fit::error(status);
  }

  auto user_vmar = create_user_vmar(root_vmar, info);
  if (user_vmar.is_error()) {
    return user_vmar.take_error();
  }

  zx_info_vmar_t user_vmar_info;
  status =
      user_vmar->get_info(ZX_INFO_VMAR, &user_vmar_info, sizeof(user_vmar_info), nullptr, nullptr);
  if (status != ZX_OK) {
    return fit::error(status);
  }

  DEBUG_ASSERT(PRIVATE_ASPACE_BASE == user_vmar_info.base);
  DEBUG_ASSERT(PRIVATE_ASPACE_SIZE == user_vmar_info.len);

  fbl::AllocChecker ac;
  fbl::RefPtr<MemoryManager> mm = fbl::AdoptRef(
      new (&ac) MemoryManager(ktl::move(root_vmar), ktl::move(*user_vmar), user_vmar_info));
  if (!ac.check()) {
    return fit::error(ZX_ERR_NO_MEMORY);
  }

  return fit::ok(ktl::move(mm));
}

MemoryManager::MemoryManager(zx::vmar root, zx::vmar user_vmar, zx_info_vmar_t user_vmar_info)
    : root_vmar(ktl::move(root)), base_addr(UserAddress::from_ptr(user_vmar_info.base)) {
  LTRACE;

  auto _state = state.Write();
  _state->user_vmar = ktl::move(user_vmar);
  _state->user_vmar_info = user_vmar_info;
  _state->forkable_state_ = MemoryManagerForkableState{{}, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
}

fit::result<Errno, UserAddress> MemoryManager::set_brk(const CurrentTask& current_task,
                                                       UserAddress addr) {
  uint64_t rlimit_data =
      ktl::min(kProgramBreakLimit, current_task->thread_group->get_rlimit({ResourceEnum::DATA}));

  fbl::Vector<Mapping> released_mappings;
  auto _state = state.Write();

  // Ensure that there is address-space set aside for the program break.
  ProgramBreak brk;
  if (!(*_state)->brk.has_value()) {
    zx::vmo vmo;
    zx_status_t status = zx::vmo::create(kProgramBreakLimit, 0, &vmo);
    if (status != ZX_OK) {
      return fit::error(errno(ENOMEM));
    }

    set_zx_name(zx::unowned_handle(vmo.get()), {(uint8_t*)"starnix-brk", 11});

    size_t length = PAGE_SIZE;
    auto flags = MappingFlags(MappingFlagsEnum::READ) | MappingFlags(MappingFlagsEnum::WRITE);

    fbl::AllocChecker ac;
    auto arc_vmo = fbl::MakeRefCountedChecked<zx::Arc<zx::vmo>>(&ac, ktl::move(vmo));
    if (!ac.check()) {
      return fit::error(errno(ENOMEM));
    }

    auto map_addr =
        (*_state).map_vmo(DesiredAddress{DesiredAddressType::Any}, arc_vmo, 0, length, flags, false,
                          MappingName{MappingNameType::Heap}, released_mappings);
    if (map_addr.is_error()) {
      return map_addr.take_error();
    }
    (*_state)->brk = brk = ProgramBreak{map_addr.value(), map_addr.value()};
  } else {
    brk = (*_state)->brk.value();
  }

  if ((addr < brk.base) || (addr > (brk.base + rlimit_data))) {
    // The requested program break is out-of-range. We're supposed to simply
    // return the current program break.
    return fit::ok(brk.current);
  }

  auto opt = _state->mappings.get(brk.current);
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

    zx_status_t status = ktl::visit(
        MappingBacking::overloaded{
            [](const PrivateAnonymous&) { return ZX_ERR_NOT_SUPPORTED; },
            [&](const MappingBackingVmo& m) {
              return m.vmo_->as_ref().op_range(ZX_VMO_OP_ZERO, vmo_offset, delta, nullptr, 0);
            },
        },
        mapping.backing_.variant);

    if (status != ZX_OK) {
      return fit::error(impossible_error(status));
    }
    auto result = _state->unmap(new_end, delta, released_mappings);
    if (result.is_error())
      return result.take_error();

  } else if (new_end > old_end) {
    // We've been asked to map more memory.
    auto delta = new_end - old_end;
    auto vmo_offset = old_end - brk.base;

    zx_status_t status =
        ktl::visit(MappingBacking::overloaded{
                       [](const PrivateAnonymous&) { return ZX_ERR_NOT_SUPPORTED; },
                       [&](const MappingBackingVmo& m) {
                         return _state->user_vmar.map(
                             ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_SPECIFIC,
                             old_end - base_addr, m.vmo_->as_ref(), vmo_offset, delta, nullptr);
                       },
                   },
                   mapping.backing_.variant);

    if (status == ZX_OK) {
      _state->mappings.insert({brk.base, new_end}, mapping);
    } else {
      _state->mappings.insert({brk.base, old_end}, mapping);
      return fit::error(get_errno_for_map_err(status));
    }
  }

  (*_state)->brk = brk;
  return fit::ok(brk.current);
}

fit::result<Errno> MemoryManager::snapshot_to(fbl::RefPtr<MemoryManager>& target) {
  // TODO(https://fxbug.dev/42074633): When SNAPSHOT (or equivalent) is supported on pager-backed
  // VMOs we can remove the hack below (which also won't be performant). For now, as a workaround,
  // we use SNAPSHOT_AT_LEAST_ON_WRITE on both the child and the parent.
  LTRACE;

  struct VmoWrapper : public fbl::SinglyLinkedListable<ktl::unique_ptr<VmoWrapper>> {
    zx::ArcVmo vmo;

    // Required to instantiate fbl::DefaultKeyedObjectTraits.
    zx_koid_t GetKey() const {
      zx_info_handle_basic_t info;
      zx_status_t status =
          (*vmo)->get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
      if (status != ZX_OK) {
        impossible_error(status);
      }
      return info.koid;
    }

    // Required to instantiate fbl::DefaultHashTraits.
    static size_t GetHash(zx_koid_t key) { return key; }
  };

  struct VmoInfo : public fbl::SinglyLinkedListable<ktl::unique_ptr<VmoInfo>> {
    zx::ArcVmo vmo;
    uint64_t size;
    // Indicates whether or not the VMO needs to be replaced on the parent as well.
    bool needs_snapshot_on_parent;

    // Required to instantiate fbl::DefaultKeyedObjectTraits.
    zx_koid_t GetKey() const {
      zx_info_handle_basic_t info;
      zx_status_t status =
          (*vmo)->get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
      if (status != ZX_OK) {
        impossible_error(status);
      }
      return info.koid;
    }

    // Required to instantiate fbl::DefaultHashTraits.
    static size_t GetHash(zx_koid_t key) { return key; }
  };

  // Clones the `vmo` and returns the `VmoInfo` with the clone.
  auto clone_vmo = [](const zx::ArcVmo& vmo,
                      zx_rights_t rights) -> fit::result<Errno, ktl::unique_ptr<VmoInfo>> {
    zx_info_vmo_t info = {};
    zx_status_t status = (*vmo)->get_info(ZX_INFO_VMO, &info, sizeof(info), nullptr, nullptr);
    if (status != ZX_OK) {
      impossible_error(status);
    }
    bool pager_backed = (info.flags & ZX_INFO_VMO_PAGER_BACKED) == ZX_INFO_VMO_PAGER_BACKED;
    if (pager_backed && !((rights & ZX_RIGHT_WRITE) == ZX_RIGHT_WRITE)) {
      fbl::AllocChecker ac;
      auto vmo_info = ktl::unique_ptr<VmoInfo>(new (&ac) VmoInfo{
          .vmo = vmo, .size = info.size_bytes, .needs_snapshot_on_parent = false});
      ASSERT(ac.check());
      return fit::ok(ktl::move(vmo_info));
    } else {
      zx::vmo cloned_vmo;
      status = (*vmo)->create_child(
          (pager_backed ? ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE : ZX_VMO_CHILD_SNAPSHOT) |
              ZX_VMO_CHILD_RESIZABLE,
          0, info.size_bytes, &cloned_vmo);
      if (status != ZX_OK) {
        return fit::error(get_errno_for_map_err(status));
      }
      if ((rights & ZX_RIGHT_EXECUTE) == ZX_RIGHT_EXECUTE) {
        status = cloned_vmo.replace_as_executable({}, &cloned_vmo);
        if (status != ZX_OK) {
          impossible_error(status);
        }
      }
      fbl::AllocChecker ac;
      zx::ArcVmo arc_vmo = fbl::AdoptRef(new (&ac) zx::Arc<zx::vmo>(ktl::move(cloned_vmo)));
      ASSERT(ac.check());
      auto vmo_info =
          ktl::unique_ptr<VmoInfo>(new (&ac) VmoInfo{.vmo = ktl::move(arc_vmo),
                                                     .size = info.size_bytes,
                                                     .needs_snapshot_on_parent = pager_backed});
      ASSERT(ac.check());
      return fit::ok(ktl::move(vmo_info));
    }
  };

  auto snapshot_vmo = [](const zx::ArcVmo& vmo, uint64_t size,
                         zx_rights_t rights) -> fit::result<Errno, ktl::unique_ptr<VmoWrapper>> {
    zx::vmo cloned_vmo;
    zx_status_t status = (*vmo)->create_child(
        ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE | ZX_VMO_CHILD_RESIZABLE, 0, size, &cloned_vmo);
    if (status != ZX_OK) {
      return fit::error(get_errno_for_map_err(status));
    }
    if ((rights & ZX_RIGHT_EXECUTE) == ZX_RIGHT_EXECUTE) {
      status = cloned_vmo.replace_as_executable({}, &cloned_vmo);
      if (status != ZX_OK) {
        impossible_error(status);
      }
    }
    fbl::AllocChecker ac;
    zx::ArcVmo arc_vmo = fbl::AdoptRef(new (&ac) zx::Arc<zx::vmo>(ktl::move(cloned_vmo)));
    ASSERT(ac.check());
    auto vmo_info = ktl::unique_ptr<VmoWrapper>(new (&ac) VmoWrapper{.vmo = ktl::move(arc_vmo)});
    ASSERT(ac.check());
    return fit::ok(ktl::move(vmo_info));
  };

  auto& _state = *state.Write();
  auto target_state = target->state.Write();

  fbl::HashTable<zx_koid_t, ktl::unique_ptr<VmoInfo>> child_vmos;
  fbl::HashTable<zx_koid_t, ktl::unique_ptr<VmoWrapper>> replaced_vmos;

  for (auto& [range, mapping] : _state.mappings.iter()) {
    if (mapping.flags_.contains(MappingFlagsEnum::DONTFORK)) {
      continue;
    }

    auto result = ktl::visit(
        MappingBacking::overloaded{
            [&, &crange = range,
             &cmapping = mapping](MappingBackingVmo& backing) -> fit::result<Errno> {
              auto vmo_offset = backing.vmo_offset_ + (crange.start - backing.base_);
              auto length = crange.end - crange.start;

              zx::ArcVmo target_vmo;
              if (cmapping.flags_.contains(MappingFlagsEnum::SHARED) ||
                  cmapping.name_.type == MappingNameType::Vvar) {
                // Note that the Vvar is a special mapping that behaves like a shared mapping
                // but is private to each process.
                target_vmo = backing.vmo_;
              } else if (cmapping.flags_.contains(MappingFlagsEnum::WIPEONFORK)) {
                auto vmo_or_error = create_anonymous_mapping_vmo(length);
                if (vmo_or_error.is_error()) {
                  return vmo_or_error.take_error();
                }
                target_vmo = vmo_or_error.value();
              } else {
                zx_info_handle_basic_t basic_info;
                zx_status_t status = (*backing.vmo_)
                                         ->get_info(ZX_INFO_HANDLE_BASIC, &basic_info,
                                                    sizeof(basic_info), nullptr, nullptr);
                if (status != ZX_OK) {
                  impossible_error(status);
                }

                VmoInfo info;
                auto const child_it = child_vmos.find(basic_info.koid);
                if (child_it == child_vmos.end()) {
                  auto vmo_or_error = clone_vmo(backing.vmo_, basic_info.rights);
                  if (vmo_or_error.is_error())
                    return vmo_or_error.take_error();
                  auto& value_ref = vmo_or_error.value();
                  info.vmo = value_ref->vmo;
                  info.size = value_ref->size;
                  info.needs_snapshot_on_parent = value_ref->needs_snapshot_on_parent;
                  child_vmos.insert(ktl::move(vmo_or_error.value()));
                } else {
                  const auto& entry = child_it;
                  info.vmo = entry->vmo;
                  info.size = entry->size;
                  info.needs_snapshot_on_parent = entry->needs_snapshot_on_parent;
                }

                if (info.needs_snapshot_on_parent) {
                  zx::ArcVmo replaced_vmo;
                  auto replaced_it = replaced_vmos.find(basic_info.koid);
                  if (replaced_it == replaced_vmos.end()) {
                    auto vmo_or_error = snapshot_vmo(backing.vmo_, info.size, basic_info.rights);
                    if (vmo_or_error.is_error())
                      return vmo_or_error.take_error();
                    replaced_vmo = vmo_or_error.value()->vmo;
                    replaced_vmos.insert(ktl::move(vmo_or_error.value()));
                  } else {
                    replaced_vmo = replaced_it->vmo;
                  }

                  auto add_or_error = map_in_vmar(
                      _state.user_vmar, _state.user_vmar_info,
                      {DesiredAddressType::FixedOverwrite, crange.start}, replaced_vmo->as_ref(),
                      vmo_offset, length, cmapping.flags_, false);
                  if (add_or_error.is_error()) {
                    return add_or_error.take_error();
                  }
                  backing.vmo_ = ktl::move(replaced_vmo);
                }
                target_vmo = info.vmo;
              }

              fbl::Vector<Mapping> released_mappings;
              auto add_or_error = target_state->map_vmo(
                  {DesiredAddressType::Fixed, crange.start}, target_vmo, vmo_offset, length,
                  cmapping.flags_, false, cmapping.name_, released_mappings);
              if (add_or_error.is_error()) {
                return add_or_error.take_error();
              }
              ASSERT(released_mappings.is_empty());
              return fit::ok();
            },
            [](PrivateAnonymous&) { return fit::result<Errno>(fit::error(errno(ENOSYS))); },
        },
        mapping.backing_.variant);

    if (result.is_error()) {
      return result.take_error();
    }
  }

  (*target_state).forkable_state_ = _state.forkable_state_;

  // let self_dumpable = *self.dumpable.lock(locked);
  //*target.dumpable.lock(locked) = self_dumpable;
  return fit::ok();
}

fit::result<zx_status_t> MemoryManager::exec(/*NamespaceNode exe_node*/) {
  LTRACE;
  // The previous mapping should be dropped only after the lock to state is released to
  // prevent lock order inversion.
  {
    auto _state = state.Write();
    zx_info_vmar_t info;
    zx_status_t status = root_vmar.get_info(ZX_INFO_VMAR, &info, sizeof(info), nullptr, nullptr);
    if (status != ZX_OK) {
      return fit::error(status);
    }

    // SAFETY: This operation is safe because the VMAR is for another process.
    status = _state->user_vmar.destroy();
    if (status != ZX_OK) {
      return fit::error(status);
    }

    auto user_vmar = create_user_vmar(root_vmar, info);
    if (user_vmar.is_error()) {
      return user_vmar.take_error();
    }

    _state->user_vmar = ktl::move(user_vmar.value());
    zx_info_vmar_t user_vmar_info;
    status = _state->user_vmar.get_info(ZX_INFO_VMAR, &user_vmar_info, sizeof(user_vmar_info),
                                        nullptr, nullptr);
    if (status != ZX_OK) {
      return fit::error(status);
    }

    _state->user_vmar_info = user_vmar_info;
    (*_state)->brk = {};
    // state.executable_node = Some(exe_node);

    _state->mappings.clear();
  }
  return fit::ok();
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
  return state.Read()->read_memory(addr, bytes);
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManager::unified_read_memory_partial_until_null_byte(
    const CurrentTask& current_task, UserAddress addr, ktl::span<uint8_t>& bytes) const {
  DEBUG_ASSERT(has_same_address_space(current_task->mm()));
  if (ThreadDispatcher::GetCurrent()) {
    return fit::error(errno(ENOSYS));
  } else {
    return vmo_read_memory_partial_until_null_byte(addr, bytes);
  }
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManager::vmo_read_memory_partial_until_null_byte(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  return state.Read()->read_memory_partial_until_null_byte(addr, bytes);
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

fit::result<Errno, ktl::span<uint8_t>> MemoryManager::unified_read_memory_partial(
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
    return vmo_read_memory_partial(addr, bytes);
  }
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManager::vmo_read_memory_partial(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  return state.Read()->read_memory_partial(addr, bytes);
}

fit::result<Errno, size_t> MemoryManager::vmo_write_memory(
    UserAddress addr, const ktl::span<const uint8_t>& bytes) const {
  return state.Read()->write_memory(addr, bytes);
}

fit::result<Errno> MemoryManager::unmap(UserAddress addr, size_t length) {
  fbl::Vector<Mapping> released_mappings;
  {
    auto _state = state.Write();
    auto result = _state->unmap(addr, length, released_mappings);
    if (result.is_error())
      return result.take_error();
  }

  // Drop the state before the unmapped mappings, since dropping a mapping may acquire a lock
  // in `DirEntry`'s `drop`.
  released_mappings.reset();

  return fit::ok();
}

fit::result<Errno> MemoryManager::protect(UserAddress addr, size_t length,
                                          ProtectionFlags prot_flags) {
  auto _state = state.Write();
  return _state->protect(addr, length, prot_flags);
}

fit::result<Errno> MemoryManager::set_mapping_name(UserAddress addr, size_t length,
                                                   ktl::optional<FsString> name) {
  if ((addr.ptr() % PAGE_SIZE) != 0) {
    return fit::error(errno(EINVAL));
  }

  auto end = addr.checked_add(length);
  if (end.has_value()) {
    auto end_or_error = end->round_up(PAGE_SIZE).map_error([](Errno e) { return errno(ENOMEM); });
    if (end_or_error.is_error())
      return end_or_error.take_error();
    end = end_or_error.value();
  } else {
    return fit::error(errno(EINVAL));
  }

  auto _state = state.Write();

  fbl::Vector<ktl::pair<util::Range<UserAddress>, Mapping>> mappings_in_range;
  auto mappings = _state->mappings.intersection(util::Range<UserAddress>({addr, end.value()}));
  ktl::copy(mappings.begin(), mappings.end(), util::back_inserter(mappings_in_range));

  if (mappings_in_range.is_empty()) {
    return fit::error(errno(EINVAL));
  }

  if (!mappings_in_range.begin()->first.contains(addr)) {
    return fit::error(errno(ENOMEM));
  }

  ktl::optional<UserAddress> last_range_end;
  // There's no get_mut on RangeMap, because it would be hard to implement correctly in
  // combination with merging of adjacent mappings. Instead, make a copy, change the copy,
  // and insert the copy.
  for (auto [range, mapping] : mappings_in_range) {
    if (mapping.name_.type == MappingNameType::File) {
      // It's invalid to assign a name to a file-backed mapping.
      return fit::error(errno(EBADF));
    }
    if (range.start < addr) {
      // This mapping starts before the named region. Split the mapping so we can apply the name
      // only to the specified region.
      auto start_split_range = util::Range<UserAddress>{range.start, addr};
      auto start_split_length = addr - range.start;

      auto start_split_mapping = ktl::visit(
          MappingBacking::overloaded{
              [&, &crange = range, &cmapping = mapping](MappingBackingVmo& backing) {
                // Shrink the range of the named mapping to only the named area.
                backing.vmo_offset_ = start_split_length;
                auto new_mapping = Mapping::New(crange.start, backing.vmo_, backing.vmo_offset_,
                                                MappingFlagsImpl(cmapping.flags_));
                return new_mapping;
              },
              [](PrivateAnonymous&) {
                return Mapping::New(0, zx::ArcVmo(), 0, MappingFlagsImpl(MappingFlags::empty()));
              },
          },
          mapping.backing_.variant);

      _state->mappings.insert(start_split_range, start_split_mapping);
      range = util::Range<UserAddress>{addr, range.end};
    }
    if (last_range_end.has_value()) {
      if (last_range_end.value() != range.start) {
        // The name must apply to a contiguous range of mapped pages.
        return fit::error(errno(ENOMEM));
      }
    }
    auto add_or_error = range.end.round_up(PAGE_SIZE);
    if (add_or_error.is_error())
      return add_or_error.take_error();
    last_range_end = add_or_error.value();

    // TODO(b/310255065): We have no place to store names in a way visible to programs outside of
    // Starnix such as memory analysis tools.
#if STARNIX_ANON_ALLOCS
#[cfg(not(feature = "alternate_anon_allocs"))]
    {
      let MappingBacking::Vmo(backing) = &mapping.backing;
      match& name {
        Some(vmo_name) = > { set_zx_name(&*backing.vmo, vmo_name); }
        None = > { set_zx_name(&*backing.vmo, b ""); }
      }
    }
#endif
    if (range.end > end) {
      // The named region ends before the last mapping ends. Split the tail off of the
      // last mapping to have an unnamed mapping after the named region.
      auto tail_range = util::Range<UserAddress>{end.value(), range.end};
      auto tail_offset = range.end - end.value();
      auto tail_mapping = ktl::visit(
          MappingBacking::overloaded{
              [&, &cmapping = mapping](MappingBackingVmo& backing) {
                auto new_mapping =
                    Mapping::New(end.value(), backing.vmo_, backing.vmo_offset_ + tail_offset,
                                 MappingFlagsImpl(cmapping.flags_));
                return new_mapping;
              },
              [](PrivateAnonymous&) {
                return Mapping::New(0, zx::ArcVmo(), 0, MappingFlagsImpl(MappingFlags::empty()));
              },
          },
          mapping.backing_.variant);

      _state->mappings.insert(tail_range, tail_mapping);
      range.end = end.value();
    }

    if (name.has_value()) {
      mapping.name_ = MappingName{MappingNameType::Vma, name.value()};
    }
    _state->mappings.insert(range, mapping);
  }

  if (last_range_end.has_value()) {
    if (last_range_end.value() < end.value()) {
      // The name must apply to a contiguous range of mapped pages.
      return fit::error(errno(ENOMEM));
    }
  }

  return fit::ok();
}

fit::result<Errno, ktl::optional<FsString>> MemoryManager::get_mapping_name(UserAddress addr) {
  auto _state = state.Read();
  auto pair = _state->mappings.get(addr);
  if (!pair.has_value()) {
    return fit::error(errno(EFAULT));
  }
  auto& [range, mapping] = pair.value();
  if (mapping.name_.type == MappingNameType::Vma) {
    return fit::ok(mapping.name_.vmaName);
  } else {
    return fit::ok(ktl::nullopt);
  }
}

fit::result<Errno, UserAddress> MemoryManager::map_anonymous(DesiredAddress addr, size_t length,
                                                             ProtectionFlags prot_flags,
                                                             MappingOptionsFlags options,
                                                             MappingName name) {
  LTRACE;
  fbl::Vector<Mapping> released_mappings;
  fit::result<Errno, UserAddress> result = fit::error(errno(EINVAL));

  {
    auto _state = state.Write();
    result = _state->map_anonymous(addr, length, prot_flags, options, name, released_mappings);
    if (result.is_error()) {
      return result.take_error();
    }
  }
  // Drop the state before the unmapped mappings, since dropping a mapping may acquire a lock
  // in `DirEntry`'s `drop`.
  released_mappings.reset();

  return result;
}

size_t MemoryManager::get_mapping_count() {
  auto _state = state.Read();
  return _state->mappings.iter().size();
}

UserAddress MemoryManager::get_random_base(size_t length) {
  auto _state = state.Read();
  // Allocate a vmar of the correct size, get the random location, then immediately destroy it.
  // This randomizes the load address without loading into a sub-vmar and breaking mprotect.
  // This is different from how Linux actually lays out the address space. We might need to
  // rewrite it eventually.
  zx::vmar temp_vmar;
  uintptr_t base;
  ASSERT(_state->user_vmar.allocate(0, 0, length, &temp_vmar, &base) == ZX_OK);
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
      return fit::error<Errno>(impossible_error(result));
  }

  set_zx_name(zx::unowned_handle(vmo.get()), {(uint8_t*)"starnix-anon", strlen("starnix-anon")});

  // TODO(https://fxbug.dev/42056890): Audit replace_as_executable usage
  result = vmo.replace_as_executable({}, &vmo);
  if (result != ZX_OK) {
    return fit::error<Errno>(impossible_error(result));
  }

  fbl::AllocChecker ac;
  zx::ArcVmo arc_vmo = fbl::AdoptRef(new (&ac) zx::Arc<zx::vmo>(ktl::move(vmo)));
  if (!ac.check()) {
    return fit::error<Errno>(errno(ENOMEM));
  }

  return fit::ok(ktl::move(arc_vmo));
}

}  // namespace starnix
