// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/mm/memory_manager.h"

#include <align.h>
#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/logging/logging.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <lib/mistos/starnix/kernel/mm/memory.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/math.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/starnix_uapi/user_buffer.h>
#include <lib/mistos/util/back_insert_iterator.h>
#include <lib/mistos/util/cprng.h>
#include <lib/mistos/util/error_propagation.h>
#include <lib/mistos/util/num.h>
#include <lib/mistos/util/range-map.h>
#include <lib/mistos/util/range_ext.h>
#include <lib/user_copy/user_ptr.h>
#include <trace.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/features.h>
#include <zircon/rights.h>
#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <ktl/iterator.h>
#include <ktl/optional.h>
#include <ktl/variant.h>
#include <object/process_dispatcher.h>
#include <object/thread_dispatcher.h>
#include <object/vm_address_region_dispatcher.h>
#include <object/vm_object_dispatcher.h>

#include "../kernel_priv.h"

namespace ktl {

using std::addressof;
using std::destroy_at;

}  // namespace ktl

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace {

using MappingFlagsImpl = starnix::MappingFlagsImpl;
using MappingFlags = starnix::MappingFlags;
using DesiredAddress = starnix::DesiredAddress;
using MemoryObject = starnix::MemoryObject;
using MappingFlagsEnum = starnix::MappingFlagsEnum;

fit::result<zx_status_t, zx::vmar> create_user_vmar(const zx::vmar& vmar,
                                                    const zx_info_vmar_t& vmar_info) {
  LTRACEF("vmar_info={.base=%lx,.len=%lu}\n", vmar_info.base, vmar_info.len);
  size_t ptr;
  zx::vmar out;
  vmar.allocate(ZX_VM_SPECIFIC | ZX_VM_CAN_MAP_SPECIFIC | ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE |
                    ZX_VM_CAN_MAP_EXECUTE,
                0, vmar_info.len, &out, &ptr);
  ZX_ASSERT(ptr == vmar_info.base);
  return fit::ok(ktl::move(out));
}

namespace {
uint64_t from_le_bytes(const ktl::array<uint8_t, 8>& bytes) {
  return (static_cast<uint64_t>(bytes[0])) | (static_cast<uint64_t>(bytes[1]) << 8) |
         (static_cast<uint64_t>(bytes[2]) << 16) | (static_cast<uint64_t>(bytes[3]) << 24) |
         (static_cast<uint64_t>(bytes[4]) << 32) | (static_cast<uint64_t>(bytes[5]) << 40) |
         (static_cast<uint64_t>(bytes[6]) << 48) | (static_cast<uint64_t>(bytes[7]) << 56);
}
}  // namespace

size_t generate_random_offset_for_aslr() {
  // Generate a number with ASLR_RANDOM_BITS.
  auto randomness = []() -> size_t {
    const size_t MASK = (1 << starnix::ASLR_RANDOM_BITS) - 1;
    ktl::array<uint8_t, sizeof(size_t)> bytes{0};
    cprng_draw(bytes.data(), bytes.size());
    size_t value = from_le_bytes(bytes);
    return value & MASK;
  }();

  // Transform it into a page-aligned offset.
  return randomness * static_cast<size_t>(PAGE_SIZE);
}

fit::result<Errno, UserAddress> map_in_vmar(const zx::vmar& vmar, zx_info_vmar_t vmar_info,
                                            DesiredAddress addr, fbl::RefPtr<MemoryObject>& memory,
                                            uint64_t memory_offset, size_t length,
                                            MappingFlags flags, bool populate) {
  // profile_duration!("MapInVmar");

  auto base_addr = UserAddress::from_ptr(vmar_info.base);
  auto mflags = MappingFlagsImpl(flags);

  auto match_addr =
      [mflags, base_addr](starnix::DesiredAddress addr) -> ktl::pair<uint64_t, zx_vm_option_t> {
    switch (addr.type) {
      case starnix::DesiredAddressType::Any: {
        if (mflags.contains(MappingFlagsEnum::LOWER_32BIT)) {
          // MAP_32BIT specifies that the memory allocated will
          // be within the first 2 GB of the process address space.
          return ktl::pair(0x80000000 - base_addr.ptr(), ZX_VM_OFFSET_IS_UPPER_LIMIT);
        }
        __UNREACHABLE;
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
      op = ZX_VMO_OP_PREFETCH;
    }
    auto _ = memory->op_range(op, &memory_offset, &length);
    // "The mmap() call doesn't fail if the mapping cannot be populated."
  }

  bool contains_specific_overwrite =
      (ZX_VM_SPECIFIC_OVERWRITE & vmar_extra_flags) == ZX_VM_SPECIFIC_OVERWRITE;

  int vmar_maybe_map_range = (populate && !contains_specific_overwrite) ? ZX_VM_MAP_RANGE : 0;

  auto vmar_flags = mflags.prot_flags().to_vmar_flags() | ZX_VM_ALLOW_FAULTS | vmar_extra_flags |
                    vmar_maybe_map_range;

  auto map_result = memory->map_in_vmar(vmar, vmar_offset, &memory_offset, length, vmar_flags);
  if (map_result.is_error()) {
    if (addr.type == starnix::DesiredAddressType::Hint) {
      vmar_flags &= ~ZX_VM_SPECIFIC;
      map_result = memory->map_in_vmar(vmar, 0, &memory_offset, length, vmar_flags);
    }
  }

  if (map_result.is_error()) {
    return fit::error(starnix::MemoryManager::get_errno_for_map_err(map_result.error_value()));
  }

  return fit::ok(map_result.value());
}

}  // namespace

namespace starnix {

ktl::optional<UserAddress> MemoryManagerState::find_next_unused_range(size_t length) const {
  // Iterate over existing mappings within range, in descending order
  auto map_iter = mappings.iter_ending_at(forkable_state_.mmap_top);

  auto checked_sub = forkable_state_.mmap_top.checked_sub(length);
  if (!checked_sub.has_value()) {
    return ktl::nullopt;
  }
  // Currently considered range. Will be moved downwards if it intersects the current mapping.
  auto candidate = util::Range<UserAddress>{.start = *checked_sub, .end = forkable_state_.mmap_top};

  auto it = map_iter.rbegin();
  while (true) {
    // Is there a next mapping? If not, the candidate is already good.
    if (it == map_iter.rend()) {
      return candidate.start;
    }

    const auto& [mapping, _] = *it;

    // If it doesn't overlap, the gap is big enough to fit.
    if (mapping.end <= candidate.start) {
      return candidate.start;
    }

    auto value = mapping.start.checked_sub(length);
    if (!value.has_value()) {
      return ktl::nullopt;
    }
    // If there was a mapping in the way, the next range to consider will be `length` bytes
    // below.
    candidate = util::Range<UserAddress>{.start = *value, .end = mapping.start};

    ++it;
  }
  return ktl::nullopt;
}

fit::result<Errno, UserAddress> MemoryManagerState::map_internal(DesiredAddress addr,
                                                                 fbl::RefPtr<MemoryObject>& memory,
                                                                 uint64_t memory_offset,
                                                                 size_t length, MappingFlags flags,
                                                                 bool populate) {
  auto new_addr = [&]() -> fit::result<Errno, DesiredAddress> {
    if (addr.type == DesiredAddressType::Any) {
      if (!flags.contains(MappingFlagsEnum::LOWER_32BIT)) {
        // profile_duration!("FindAddressForMmap");
        auto new_addr = find_next_unused_range(round_up_to_system_page_size(length).value());
        if (!new_addr.has_value()) {
          return fit::error(errno(ENOMEM));
        }
        return fit::ok(DesiredAddress{.type = DesiredAddressType::Fixed, .address = *new_addr});
      }
    }
    return fit::ok(addr);
  }() _EP(new_addr);

  return map_in_vmar(user_vmar_, user_vmar_info_, addr, memory, memory_offset, length, flags,
                     populate);
}

fit::result<Errno> MemoryManagerState::validate_addr(DesiredAddress addr, size_t length) {
  if (addr.type == DesiredAddressType::FixedOverwrite) {
    if (check_has_unauthorized_splits(addr.address, length)) {
      return fit::error(errno(ENOMEM));
    }
  }
  return fit::ok();
}

fit::result<Errno, UserAddress> MemoryManagerState::map_memory(
    fbl::RefPtr<MemoryManager> mm, DesiredAddress addr, fbl::RefPtr<MemoryObject> memory,
    uint64_t vmo_offset, size_t length, MappingFlags flags, bool populate, MappingName name,
    fbl::Vector<Mapping>& released_mappings) {
  LTRACEF("addr 0x%lx length %zu flags 0x%x populate %s\n", addr.address.ptr(), length,
          flags.bits(), populate ? "true" : "false");
  auto va = validate_addr(addr, length) _EP(va);

  auto mapped_addr =
      map_internal(addr, memory, vmo_offset, length, flags, populate) _EP(mapped_addr);

  // profile_duration!("FinishMapping");
  auto end = (mapped_addr.value() + length).round_up(PAGE_SIZE).value();

  if (addr.type == DesiredAddressType::FixedOverwrite) {
    ASSERT(addr.address == mapped_addr.value());
    auto result =
        update_after_unmap(mm, addr.address, end - addr.address, released_mappings) _EP(result);
  }

  auto mapping = Mapping::New(*mapped_addr, memory, vmo_offset, MappingFlagsImpl(flags));
  mapping.name_ = ktl::move(name);
  mappings.insert({.start = *mapped_addr, .end = end}, mapping);

  if (flags.contains(MappingFlagsEnum::GROWSDOWN)) {
    // track_stub !(TODO("https://fxbug.dev/297373369"), "GROWSDOWN guard region");
  }

  LTRACEF("mapped at 0x%lx\n", mapped_addr->ptr());

  return fit::ok(*mapped_addr);
}

fit::result<Errno, UserAddress> MemoryManagerState::map_anonymous(
    fbl::RefPtr<MemoryManager> mm, DesiredAddress addr, size_t length, ProtectionFlags prot_flags,
    MappingOptionsFlags options, MappingName name, fbl::Vector<Mapping>& released_mappings) {
  auto result = create_anonymous_mapping_memory(length) _EP(result);
  auto flags = MappingFlagsImpl::from_prot_flags_and_options(prot_flags, options);
  return map_memory(ktl::move(mm), addr, ktl::move(result.value()), 0, length, flags,
                    options.contains(MappingOptions::POPULATE), ktl::move(name), released_mappings);
}

fit::result<Errno, UserAddress> MemoryManagerState::remap(fbl::RefPtr<MemoryManager>& mm,
                                                          UserAddress old_addr, size_t old_length,
                                                          size_t new_length /*, MremapFlags flags*/,
                                                          UserAddress new_addr,
                                                          fbl::Vector<Mapping>& released_mappings) {
  return fit::error(errno(ENOSYS));
}

fit::result<Errno, ktl::optional<UserAddress>> MemoryManagerState::try_remap_in_place(
    fbl::RefPtr<MemoryManager> mm, UserAddress old_addr, size_t old_length, size_t new_length,
    fbl::Vector<Mapping>& released_mappings) {
  auto old_range_end = old_addr.checked_add(old_length);
  if (!old_range_end.has_value()) {
    return fit::error(errno(EINVAL));
  }

  auto new_range_in_place_end = old_addr.checked_add(new_length);
  if (!new_range_in_place_end.has_value()) {
    return fit::error(errno(EINVAL));
  }

  util::Range<UserAddress> old_range{.start = old_addr, .end = *old_range_end};
  util::Range<UserAddress> new_range_in_place{.start = old_addr, .end = *new_range_in_place_end};

  if (new_length <= old_length) {
    // Shrink the mapping in-place, which should always succeed.
    // This is done by unmapping the extraneous region.
    if (new_length != old_length) {
      auto result =
          unmap(mm, new_range_in_place.end, old_length - new_length, released_mappings) _EP(result);
    }
    return fit::ok(old_addr);
  }

  // There is space to grow in-place. The old range must be one contiguous mapping.
  auto result = mappings.get(old_addr);
  if (!result.has_value()) {
    return fit::error(errno(EINVAL));
  }
  auto& [original_range, mapping] = *result;

  for (auto& [_, intersect_mapping] : mappings.intersection(
           util::Range<UserAddress>{.start = old_range.end, .end = new_range_in_place.end})) {
    // There is some mapping in the growth range prevening an in-place growth. Allow
    // growing over it only in case we're remapping the heap on the range that was reserved
    // for it.
    auto cond = (mapping.name_.type == MappingNameType::Heap) &&
                (intersect_mapping.name_.type == MappingNameType::ReservedForHeap);
    if (!cond) {
      return fit::ok(ktl::nullopt);
    }
  }

  if (old_range.end > original_range.end) {
    return fit::error(errno(EFAULT));
  }

  auto original_range_clone = original_range;
  auto original_mapping = mapping;

  // Compute the new length of the entire mapping once it has grown.
  auto final_length =
      (original_range_clone.end - original_range_clone.start) + (new_length - old_length);

  auto private_anonymous = original_mapping.private_anonymous();

  // As a special case for private, anonymous mappings, allocate more space in the
  // memory object. FD-backed mappings have their backing memory handled by the file system.
  return ktl::visit(
      MappingBacking::overloaded{
          [&](const MappingBackingMemory& backing)
              -> fit::result<Errno, ktl::optional<UserAddress>> {
            if (private_anonymous) {
              auto new_memory_size = mtl::checked_add(backing.memory_offset_, final_length);
              if (!new_memory_size.has_value()) {
                return fit::error(errno(EINVAL));
              }
              auto result = backing.memory_->set_size(new_memory_size.value());
              if (result.is_error()) {
                return fit::error(MemoryManager::get_errno_for_map_err(result.error_value()));
              }
              // Zero-out the pages that were added when growing. This is not necessary, but ensures
              // correctness of our COW implementation. Ignore any errors.
              auto original_length = original_range.end - original_range.start;
              auto offset = backing.memory_offset_ + static_cast<uint64_t>(original_length);
              auto size = static_cast<uint64_t>(final_length - original_length);
              auto _ = backing.memory_->op_range(ZX_VMO_OP_ZERO, &offset, &size);
            }

            // Re-map the original range, which may include pages before the requested range.
            return map_memory(
                mm, {.type = DesiredAddressType::FixedOverwrite, .address = original_range.start},
                backing.memory_, backing.memory_offset_, final_length, original_mapping.flags(),
                false, original_mapping.name() /*,
                      original_mapping.file_write_guard*/
                ,
                released_mappings);
          },
          [](const PrivateAnonymous&) -> fit::result<Errno, ktl::optional<UserAddress>> {
            return fit::error(errno(EFAULT));
          },
      },
      original_mapping.backing_.variant);
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
  ktl::optional<UserAddress> end_addr = addr.checked_add(length);
  if (!end_addr.has_value()) {
    return fit::error(errno(EFAULT));
  }

  if (end_addr > max_address()) {
    return fit::error(errno(EFAULT));
  }

  // Iterate over all contiguous mappings intersecting the requested range.
  auto _mappings =
      mappings.intersection(util::Range<UserAddress>({.start = addr, .end = *end_addr}));
  ktl::optional<UserAddress> prev_range_end{};
  size_t offset = 0;
  for (const auto& pair : _mappings) {
    if (offset != length) {
      auto [range, mapping] = pair;
      if (!prev_range_end.has_value() && range.start > addr) {
        // If this is the first mapping that we are considering, it may not actually
        // contain `addr` at all.
        continue;
      }

      if (prev_range_end.has_value() && range.start != prev_range_end.value()) {
        // Subsequent mappings may not be contiguous.
        continue;
      }

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
  LTRACEF_LEVEL(2, "found %zu mappings\n", result.size());
  return fit::ok(ktl::move(result));
}

UserAddress MemoryManagerState::max_address() const {
  return UserAddress::from_ptr(user_vmar_info_.base + user_vmar_info_.len);
}

// Unmaps the specified range. Unmapped mappings are placed in `released_mappings`.
fit::result<Errno> MemoryManagerState::unmap(fbl::RefPtr<MemoryManager> mm, UserAddress addr,
                                             size_t length,
                                             fbl::Vector<Mapping>& released_mappings) {
  if (!addr.is_aligned(PAGE_SIZE)) {
    return fit::error(errno(EINVAL));
  }

  auto local_length = round_up_to_system_page_size(length) _EP(local_length);
  if (*local_length == 0) {
    return fit::error(errno(EINVAL));
  }

  if (check_has_unauthorized_splits(addr, *local_length)) {
    return fit::error(errno(EINVAL));
  }

  // Unmap the range, including the the tail of any range that would have been split. This
  // operation is safe because we're operating on another process.
  zx_status_t status = user_vmar_.unmap(addr.ptr(), local_length.value());
  switch (status) {
    case ZX_OK:
    case ZX_ERR_NOT_FOUND:
      break;
    case ZX_ERR_INVALID_ARGS:
      return fit::error(errno(EINVAL));
    default:
      return fit::error<Errno>(impossible_error(status));
  }

  auto result = update_after_unmap(mm, addr, *local_length, released_mappings) _EP(result);
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
fit::result<Errno> MemoryManagerState::update_after_unmap(fbl::RefPtr<MemoryManager>& mm,
                                                          UserAddress addr, size_t length,
                                                          fbl::Vector<Mapping>& released_mappings) {
  ktl::optional<UserAddress> end_addr = addr.checked_add(length);
  if (!end_addr.has_value()) {
    return fit::error(errno(EFAULT));
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
        return ktl::pair(util::Range<UserAddress>{.start = range.start, .end = addr}, mapping);
      }
    }
    return ktl::nullopt;
  }();

  // Find the private, anonymous mapping that will get its head cut off by this unmap call.
  auto truncated_tail = [&]() -> ktl::optional<ktl::pair<util::Range<UserAddress>, Mapping>> {
    if (auto pair = mappings.get(*end_addr); pair) {
      auto& [range, mapping] = pair.value();
      if (range.end != end_addr && mapping.private_anonymous()) {
        return ktl::pair(util::Range<UserAddress>{.start = *end_addr, .end = range.end}, mapping);
      }
    }
    return ktl::nullopt;
  }();

  // Remove the original range of mappings from our map.
  auto vec = mappings.remove(util::Range<UserAddress>{.start = addr, .end = *end_addr});
  ktl::copy(vec.begin(), vec.end(), util::back_inserter(released_mappings));

  if (truncated_tail) {
    auto [range, mapping] = truncated_tail.value();
    auto& backing = ktl::get<MappingBackingMemory>(mapping.backing_.variant);

    // Create and map a child COW VMO mapping that represents the truncated tail.
    auto memory_info = backing.memory_->basic_info();
    auto child_memory_offset =
        static_cast<uint64_t>(range.start - backing.base_) + backing.memory_offset_;
    auto child_length = range.end - range.start;

    auto memory_or_error = backing.memory_
                               ->create_child(ZX_VMO_CHILD_SNAPSHOT | ZX_VMO_CHILD_RESIZABLE,
                                              child_memory_offset, child_length)
                               .map_error([](zx_status_t s) -> Errno {
                                 return MemoryManager::get_errno_for_map_err(s);
                               });

    if (memory_or_error.is_error()) {
      return memory_or_error.take_error();
    }

    auto child_memory = memory_or_error.value();

    if ((memory_info.rights & ZX_RIGHT_EXECUTE) == ZX_RIGHT_EXECUTE) {
      auto replace_or_error = child_memory->replace_as_executable().map_error(
          [](zx_status_t s) -> Errno { return MemoryManager::get_errno_for_map_err(s); });
      if (replace_or_error.is_error()) {
        return fit::error(replace_or_error.take_error());
      }
      child_memory = replace_or_error.value();
    }

    // Update the mapping.
    backing.memory_ = ktl::move(child_memory);
    backing.base_ = range.start;
    backing.memory_offset_ = 0;

    auto result = map_internal({.type = DesiredAddressType::FixedOverwrite, .address = range.start},
                               backing.memory_, 0, child_length, mapping.flags_, false);
    if (result.is_error()) {
      return result.take_error();
    }

    // Replace the mapping with a new one that contains updated VMO handle.
    mappings.insert(range, mapping);
  }

  if (truncated_head) {
    auto [range, mapping] = truncated_head.value();
    auto& backing = ktl::get<MappingBackingMemory>(mapping.backing_.variant);

    // mm.inflight_vmspliced_payloads.handle_unmapping(&backing.memory,
    // &unmap_range.intersect(&range))?;

    // Resize the memory object of the head mapping, whose tail was cut off.
    auto new_mapping_size = static_cast<uint64_t>(range.end - range.start);
    auto new_vmo_size = backing.memory_offset_ + new_mapping_size;
    if (auto status = backing.memory_->set_size(new_vmo_size); status.is_error()) {
      return fit::error(MemoryManager::get_errno_for_map_err(status.error_value()));
    }
  }

  return fit::ok();
#endif
}

fit::result<Errno> MemoryManagerState::protect(UserAddress addr, size_t length,
                                               ProtectionFlags prot_flags) {
  // profile_duration !("Protect");
  //  TODO(https://fxbug.dev/42179751): If the mprotect flags include PROT_GROWSDOWN then the
  //  specified protection may extend below the provided address if the lowest mapping is a
  //  MAP_GROWSDOWN mapping. This function has to compute the potentially extended range before
  //  modifying the Zircon protections or metadata.
  auto vmar_flags = ProtectionFlagsImpl(prot_flags).to_vmar_flags();

  if (check_has_unauthorized_splits(addr, length)) {
    return fit::error(errno(EINVAL));
  }

  // Make one call to mprotect to update all the zircon protections.
  // SAFETY: This is safe because the vmar belongs to a different process.
  if (!VmAddressRegionDispatcher::is_valid_mapping_protection(vmar_flags)) {
    return fit::error(errno(EINVAL));
  }
  auto result = user_vmar_.protect(vmar_flags, addr.ptr(), length);
  if (result != ZX_OK) {
    switch (result) {
      case ZX_ERR_INVALID_ARGS:
        return fit::error(errno(EINVAL));
      case ZX_ERR_NOT_FOUND: {
        // track_stub!(
        //             TODO("https://fxbug.dev/322875024"),
        //             "mprotect: succeed and update prot after NOT_FOUND"
        //         );
        return fit::error(errno(EINVAL));
      }
      case ZX_ERR_ACCESS_DENIED:
        return fit::error(errno(EACCES));
      default:
        impossible_error(result);
    }
  }

  // Update the flags on each mapping in the range.
  auto end = (addr + length).round_up(PAGE_SIZE) _EP(end);
  util::Range<UserAddress> prot_range{.start = addr, .end = end.value()};
  fbl::Vector<ktl::pair<util::RangeMap<UserAddress, Mapping>::RangeType,
                        util::RangeMap<UserAddress, Mapping>::ValueType>>
      updates;
  fbl::AllocChecker ac;
  for (auto& [range, mapping] :
       mappings.intersection(util::Range<UserAddress>{.start = addr, .end = end.value()})) {
    auto r = util::intersect(range, prot_range);
    auto m = mapping;
    auto new_flags = mapping.flags() & (MappingFlags(MappingFlagsEnum::READ) |
                                        MappingFlags(MappingFlagsEnum::WRITE) |
                                        MappingFlags(MappingFlagsEnum::EXEC))
                                           .complement() |
                     MappingFlags::from_bits_truncate(prot_flags.bits());
    m.flags_ = new_flags;
    updates.push_back(ktl::pair(r, m), &ac);
    ZX_ASSERT(ac.check());
  }
  // Use a separate loop to avoid mutating the mappings structure while iterating over it.
  for (auto& [range, mapping] : updates) {
    mappings.insert(range, mapping);
  }
  return fit::ok();
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManagerState::read_memory(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  size_t bytes_read = 0;
  auto vec = get_contiguous_mappings_at(addr, bytes.size()) _EP(vec);

  for (auto& [mapping, len] : vec.value()) {
    auto next_offset = bytes_read + len;
    ktl::span<uint8_t> span{bytes.data() + bytes_read, bytes.data() + next_offset};
    auto result = read_mapping_memory(addr + bytes_read, mapping, span);
    if (result.is_error())
      return result.take_error();
    bytes_read = next_offset;
  }

  if (bytes_read != bytes.size()) {
    return fit::error(errno(EFAULT));
  }
  LTRACEF("bytes read %lu\n", bytes_read);
  return fit::ok(ktl::span<uint8_t>{bytes.data(), bytes_read});
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManagerState::read_mapping_memory(
    UserAddress addr, const Mapping& mapping, ktl::span<uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  if (!mapping.can_read()) {
    return fit::error(errno(EFAULT));
  }

  return ktl::visit(
      MappingBacking::overloaded{
          [](const PrivateAnonymous&) -> fit::result<Errno, ktl::span<uint8_t>> {
            return fit::error(errno(EFAULT));
          },
          [&](const MappingBackingMemory& m) -> fit::result<Errno, ktl::span<uint8_t>> {
            return m.read_memory(addr, bytes);
          },
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
  for (const auto& [mapping, len] : mappings_or_error.value()) {
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
  }
  LTRACEF("bytes read %lu\n", bytes_read);
  return fit::ok(ktl::span<uint8_t>(bytes.data(), bytes_read));
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManagerState::read_memory_partial_until_null_byte(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  auto read_bytes = read_memory_partial(addr, bytes) _EP(read_bytes);
  auto null_position = ktl::find(read_bytes->begin(), read_bytes->end(), '\0');

  size_t max_len = (null_position == read_bytes->end())
                       ? read_bytes->size()
                       : ktl::distance(read_bytes->begin(), null_position) + 1;

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

  for (auto& [mapping, len] : vec.value()) {
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
  }
  return fit::ok(bytes.size());
}

fit::result<Errno> MemoryManagerState::write_mapping_memory(
    UserAddress addr, const Mapping& mapping, const ktl::span<const uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  if (!mapping.can_write()) {
    return fit::error(errno(EFAULT));
  }

  return ktl::visit(
      MappingBacking::overloaded{
          [](const PrivateAnonymous&) -> fit::result<Errno> { return fit::error(errno(EFAULT)); },
          [&](const MappingBackingMemory& m) -> fit::result<Errno> {
            return m.write_memory(addr, bytes);
          },
      },
      mapping.backing_.variant);
}

fit::result<Errno, size_t> MemoryManagerState::write_memory_partial(
    UserAddress addr, const ktl::span<const uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());

  // profile_duration !("WriteMemoryPartial");
  size_t bytes_written = 0;
  auto vec = get_contiguous_mappings_at(addr, bytes.size());
  if (vec.is_error()) {
    LTRACEF_LEVEL(2, "error code %d\n", vec.error_value().error_code());
    return vec.take_error();
  }

  for (auto& [mapping, len] : vec.value()) {
    auto next_offset = bytes_written + len;
    if (write_mapping_memory(addr + bytes_written, mapping,
                             {bytes.data() + bytes_written, bytes.data() + next_offset})
            .is_error()) {
      break;
    }
    bytes_written = next_offset;
  }

  if (!bytes.empty() && (bytes_written == 0)) {
    return fit::error(errno(EFAULT));
  }
  return fit::ok(bytes.size());
}

fit::result<Errno, size_t> MemoryManagerState::zero(UserAddress addr, size_t length) const {
  LTRACEF("addr 0x%lx length 0x%zx\n", addr.ptr(), length);

  // profile_duration!("Zero");
  size_t bytes_written = 0;
  auto vec = get_contiguous_mappings_at(addr, length);
  if (vec.is_error()) {
    LTRACEF_LEVEL(2, "error code %d\n", vec.error_value().error_code());
    return vec.take_error();
  }

  for (auto& [mapping, len] : vec.value()) {
    auto next_offset = bytes_written + len;
    if (zero_mapping(addr, mapping, len).is_error()) {
      break;
    }
    bytes_written = next_offset;
  }

  if (length != bytes_written) {
    return fit::error(errno(EFAULT));
  }
  return fit::ok(length);
}

fit::result<Errno, size_t> MemoryManagerState::zero_mapping(UserAddress addr,
                                                            const Mapping& mapping, size_t length) {
  LTRACEF("addr 0x%lx length 0x%zx\n", addr.ptr(), length);

  // profile_duration!("MappingZeroMemory");
  if (!mapping.can_write()) {
    return fit::error(errno(EFAULT));
  }

  return ktl::visit(MappingBacking::overloaded{
                        [](const PrivateAnonymous&) -> fit::result<Errno, size_t> {
                          return fit::error(errno(EFAULT));
                        },
                        [&](const MappingBackingMemory& m) -> fit::result<Errno, size_t> {
                          return m.zero(addr, length);
                        },
                    },
                    mapping.backing_.variant);
}

fit::result<Errno, ktl::span<uint8_t>> MappingBackingMemory::read_memory(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  auto result = memory_->read_uninit(bytes, address_to_offset(addr));
  if (result.is_error()) {
    return fit::error(errno(EFAULT));
  }
  return fit::ok(bytes);
}

fit::result<Errno> MappingBackingMemory::write_memory(UserAddress addr,
                                                      const ktl::span<const uint8_t>& bytes) const {
  LTRACEF("addr 0x%lx data %p size 0x%zx\n", addr.ptr(), bytes.data(), bytes.size());
  auto result = memory_->write(bytes, address_to_offset(addr));
  if (result.is_error()) {
    return fit::error(errno(EFAULT));
  }
  return fit::ok();
}

fit::result<Errno, size_t> MappingBackingMemory::zero(UserAddress addr, size_t length) const {
  auto offset = address_to_offset(addr);
  auto status = memory_->op_range(ZX_VMO_OP_ZERO, &offset, &length);
  if (status.is_error()) {
    return fit::error(errno(EFAULT));
  }
  return fit::ok(length);
}

uint64_t MappingBackingMemory::address_to_offset(UserAddress addr) const {
  return static_cast<uint64_t>(addr.ptr() - base_.ptr()) + memory_offset_;
}

Mapping Mapping::New(UserAddress base, fbl::RefPtr<MemoryObject> memory, uint64_t memory_offset,
                     MappingFlagsImpl flags) {
  return Mapping::with_name(base, ktl::move(memory), memory_offset, flags,
                            {.type = MappingNameType::None});
}

Mapping Mapping::with_name(UserAddress base, fbl::RefPtr<MemoryObject> memory,
                           uint64_t memory_offset, MappingFlagsImpl flags, MappingName name) {
  LTRACEF("base 0x%lx memory %p memory_offset %lu flags 0x%x\n", base.ptr(), memory.get(),
          memory_offset, flags.bits());
  return Mapping(base, ktl::move(memory), memory_offset, flags, ktl::move(name));
}

Mapping::Mapping(UserAddress base, fbl::RefPtr<MemoryObject> memory, uint64_t memory_offset,
                 MappingFlagsImpl flags, MappingName name)
    : backing_(MappingBackingMemory(base, ktl::move(memory), memory_offset)),
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
    case ZX_ERR_NO_RESOURCES:
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

fit::result<Errno, UserAddress> MemoryManager::map_memory(
    DesiredAddress addr, fbl::RefPtr<MemoryObject> memory, uint64_t memory_offset, size_t length,
    ProtectionFlags prot_flags, MappingOptionsFlags options, MappingName name) {
  auto flags = MappingFlagsImpl::from_prot_flags_and_options(prot_flags, options);

  //  Unmapped mappings must be released after the state is unlocked.
  fbl::Vector<Mapping> released_mappings;
  fit::result<Errno, UserAddress> result = fit::error(errno(EINVAL));
  {
    auto state = state_.Write();
    result = state->map_memory(
        fbl::RefPtr<MemoryManager>(this), addr, ktl::move(memory), memory_offset, length, flags,
        options.contains(MappingOptions::POPULATE), ktl::move(name), released_mappings) _EP(result);
  }

  // Drop the state before the unmapped mappings, since dropping a mapping may acquire a lock
  // in `DirEntry`'s `drop`.
  released_mappings.reset();

  return result;
}

fit::result<zx_status_t, fbl::RefPtr<MemoryManager>> MemoryManager::New(zx::vmar root_vmar) {
  zx_info_vmar_t info = {};
  zx_status_t status = root_vmar.get_info(ZX_INFO_VMAR, &info, sizeof(info), nullptr, nullptr);
  if (status != ZX_OK) {
    return fit::error{status};
  }

  auto user_vmar = create_user_vmar(root_vmar, info) _EP(user_vmar);
  zx_info_vmar_t user_vmar_info;
  status =
      user_vmar->get_info(ZX_INFO_VMAR, &user_vmar_info, sizeof(user_vmar_info), nullptr, nullptr);
  if (status != ZX_OK) {
    return fit::error{status};
  }

  DEBUG_ASSERT(PRIVATE_ASPACE_BASE == user_vmar_info.base);
  DEBUG_ASSERT(PRIVATE_ASPACE_SIZE == user_vmar_info.len);

  return fit::ok(from_vmar(ktl::move(root_vmar), ktl::move(user_vmar.value()), user_vmar_info));
}

fbl::RefPtr<MemoryManager> MemoryManager::new_empty() {
  zx::vmar root_vmar;
  zx::vmar user_vmar;
  return from_vmar(ktl::move(root_vmar), ktl::move(user_vmar), {});
}

fbl::RefPtr<MemoryManager> MemoryManager::from_vmar(zx::vmar root_vmar, zx::vmar user_vmar,
                                                    zx_info_vmar_t user_vmar_info) {
  fbl::AllocChecker ac;
  fbl::RefPtr<MemoryManager> mm = fbl::AdoptRef(
      new (&ac) MemoryManager(ktl::move(root_vmar), ktl::move(user_vmar), user_vmar_info));
  if (!ac.check()) {
    return fbl::RefPtr<MemoryManager>();
  }
  return ktl::move(mm);
}

MemoryManager::MemoryManager(zx::vmar root, zx::vmar user_vmar, zx_info_vmar_t user_vmar_info)
    : root_vmar_(ktl::move(root)),
      base_addr_(UserAddress::from_ptr(user_vmar_info.base)),
      maximum_valid_user_address_(UserAddress::from_ptr(user_vmar_info.base + user_vmar_info.len)) {
  LTRACEF("user_vmar_info={.base=%lx,.len=%lu}\n", user_vmar_info.base, user_vmar_info.len);

  auto state = state_.Write();
  state->user_vmar_ = ktl::move(user_vmar);
  state->user_vmar_info_ = user_vmar_info;
}

fit::result<Errno, UserAddress> MemoryManager::set_brk(const CurrentTask& current_task,
                                                       UserAddress addr) {
  uint64_t rlimit_data =
      ktl::min(PROGRAM_BREAK_LIMIT, current_task->thread_group_->get_rlimit({ResourceEnum::DATA}));

  fbl::Vector<Mapping> released_mappings;
  // Hold the lock throughout the operation to uphold memory manager's invariants.
  // See mm/README.md.
  auto state = state_.Write();

  // Ensure that there is address-space set aside for the program break.
  ProgramBreak brk;
  if (!(*state)->brk.has_value()) {
    zx::vmo vmo;
    auto s = zx::vmo::create(PROGRAM_BREAK_LIMIT, ZX_VMO_RESIZABLE, &vmo);
    if (s != ZX_OK) {
      return fit::error(errno(ENOMEM));
    }
    auto memory = MemoryObject::From(ktl::move(vmo));
    memory->set_zx_name("starnix-brk");

    /// Map the whole program-break memory object to prevent other mappings using the
    // range, unless they do so deliberately. Pages in this range are made writable as
    // the caller grows the program break.
    auto origin = state->forkable_state_.brk_origin;
    auto base = (*state).map_memory(
        fbl::RefPtr<MemoryManager>(this),
        DesiredAddress{.type = DesiredAddressType::Fixed, .address = origin}, memory, 0ul,
        PROGRAM_BREAK_LIMIT, MappingFlags(MappingFlagsEnum::ANONYMOUS), false,
        MappingName{.type = MappingNameType::ReservedForHeap}, released_mappings) _EP(base);
    ZX_ASSERT(released_mappings.is_empty());

    auto brk_inner = ProgramBreak{.base = *base, .current = *base, .placeholder_memory = memory};
    (*state)->brk = brk = brk_inner;
  } else {
    brk = (*state)->brk.value();
  }

  if ((addr < brk.base) || (addr > (brk.base + rlimit_data))) {
    // The requested program break is out-of-range. We're supposed to simply
    // return the current program break.
    return fit::ok(brk.current);
  }

  auto old_end = brk.current.round_up(PAGE_SIZE).value();
  auto new_end = addr.round_up(PAGE_SIZE).value();

  // std::cmp::Ordering::Less
  if (new_end < old_end) {
    // Shrinking the program break removes any mapped pages in the
    // affected range, regardless of whether they were actually program
    // break pages, or other mappings.
    auto delta = old_end - new_end;

    // Overwrite the released range with a placeholder Zircon mapping, without
    // reflecting the change in `mappings`.

#if STARNIX_ANON_ALLOCS
#else
    auto memory_offset = new_end - brk.base;
    auto result = (*state).map_memory(
        fbl::RefPtr<MemoryManager>(this),
        DesiredAddress{.type = DesiredAddressType::FixedOverwrite, .address = new_end},
        brk.placeholder_memory, memory_offset, delta, MappingFlags(MappingFlagsEnum::ANONYMOUS),
        false, MappingName{.type = MappingNameType::ReservedForHeap}, released_mappings);

    if (result.is_error()) {
      printf("Here\n");
      return fit::ok(brk.current);
    }
#endif
  }
  // std::cmp::Ordering::Greater
  else if (new_end > old_end) {
    util::Range<UserAddress> range{.start = old_end, .end = new_end};
    auto delta = new_end - old_end;

    // Check for mappings over the program break region. The space reserved for heap is
    // ok, we can overwrite it.
    for (auto& [_, mapping] : (*state).mappings.intersection(range)) {
      if (mapping.name_.type != MappingNameType::ReservedForHeap) {
        printf("Here1\n");
        return fit::ok(brk.current);
      }
    }

    // TODO(b/310255065): Call `map_anonymous()` directly once
    // `alternate_anon_allocs` is always on.
    fbl::RefPtr<MemoryManager> self(this);
    if (!extend_brk((*state), self, old_end, delta, brk.base, released_mappings)) {
      printf("Here2\n");
      return fit::ok(brk.current);
    }
  }

  // Any required updates to the program break succeeded, so update internal state.
  auto new_brk = brk;
  new_brk.current = addr;
  (*state)->brk = new_brk;
  return fit::ok(addr);
}

bool MemoryManager::extend_brk(MemoryManagerState& _state, fbl::RefPtr<MemoryManager>& mm,
                               UserAddress old_end, size_t delta, UserAddress brk_base,
                               fbl::Vector<Mapping>& released_mappings) {
#ifndef STARNIX_ANON_ALLOCS
  // If there was previously at least one page of program break then we can
  // extend that mapping, rather than making a new allocation.
  auto existing = [&]() -> ktl::optional<ktl::pair<util::Range<UserAddress>, Mapping>> {
    if (old_end > brk_base) {
      auto last_page = old_end - static_cast<size_t>(PAGE_SIZE);
      if (auto opt = _state.mappings.get(last_page); opt) {
        auto& [_, m] = *opt;
        if (m.name_.type == MappingNameType::Heap) {
          return opt;
        }
      }
    }
    return ktl::nullopt;
  }();

  if (existing.has_value()) {
    auto& [range, _] = existing.value();
    auto range_start = range.start;
    auto old_length = range.end - range.start;
    return _state
        .try_remap_in_place(mm, range_start, old_length, old_length + delta, released_mappings)
        .value_or(ktl::optional<UserAddress>())
        .has_value();
  }
#else
#endif

  // Otherwise, allocating fresh anonymous pages is good-enough.
  return _state
      .map_anonymous(
          mm, DesiredAddress{.type = DesiredAddressType::FixedOverwrite, .address = old_end}, delta,
          ProtectionFlags(ProtectionFlagsEnum::READ) | ProtectionFlags(ProtectionFlagsEnum::WRITE),
          MappingOptionsFlags(MappingOptions::ANONYMOUS), {.type = MappingNameType::Heap},
          released_mappings)
      .is_ok();
}

fit::result<Errno> MemoryManager::snapshot_to(const fbl::RefPtr<MemoryManager>& target) const {
  // TODO(https://fxbug.dev/42074633): When SNAPSHOT (or equivalent) is supported on
  // pager-backed VMOs we can remove the hack below (which also won't be performant). For now,
  // as a workaround, we use SNAPSHOT_AT_LEAST_ON_WRITE on both the child and the parent.
  LTRACE;

  struct MemoryWrapper : public fbl::SinglyLinkedListable<ktl::unique_ptr<MemoryWrapper>> {
    fbl::RefPtr<MemoryObject> memory;

    // Required to instantiate fbl::DefaultKeyedObjectTraits.
    zx_koid_t GetKey() const { return memory->get_koid(); }

    // Required to instantiate fbl::DefaultHashTraits.
    static size_t GetHash(zx_koid_t key) { return key; }
  };

  struct MemoryInfo : public fbl::SinglyLinkedListable<ktl::unique_ptr<MemoryInfo>> {
    fbl::RefPtr<MemoryObject> memory;
    uint64_t size;

    // Indicates whether or not the VMO needs to be replaced on the parent as well.
    bool needs_snapshot_on_parent;

    // Required to instantiate fbl::DefaultKeyedObjectTraits.
    zx_koid_t GetKey() const { return memory->get_koid(); }

    // Required to instantiate fbl::DefaultHashTraits.
    static size_t GetHash(zx_koid_t key) { return key; }
  };

  // Clones the `memory` and returns the `MemoryInfo` with the clone.
  auto clone_memory = [](fbl::RefPtr<MemoryObject>& memory,
                         zx_rights_t rights) -> fit::result<Errno, ktl::unique_ptr<MemoryInfo>> {
    auto memory_info = memory->info() _EP(memory_info);
    bool pager_backed =
        (memory_info.value().flags & ZX_INFO_VMO_PAGER_BACKED) == ZX_INFO_VMO_PAGER_BACKED;
    if (pager_backed && !((rights & ZX_RIGHT_WRITE) == ZX_RIGHT_WRITE)) {
      fbl::AllocChecker ac;
      auto vmo_info =
          ktl::unique_ptr<MemoryInfo>(new (&ac) MemoryInfo{.memory = memory,
                                                           .size = memory_info.value().size_bytes,
                                                           .needs_snapshot_on_parent = false});
      ASSERT(ac.check());
      return fit::ok(ktl::move(vmo_info));
    } else {
      auto cloned_memory_or_error = memory->create_child(
          (pager_backed ? ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE : ZX_VMO_CHILD_SNAPSHOT) |
              ZX_VMO_CHILD_RESIZABLE,
          0, memory_info.value().size_bytes);
      if (cloned_memory_or_error.is_error()) {
        return fit::error(get_errno_for_map_err(cloned_memory_or_error.error_value()));
      }

      auto cloned_memory = cloned_memory_or_error.value();

      if ((rights & ZX_RIGHT_EXECUTE) == ZX_RIGHT_EXECUTE) {
        cloned_memory_or_error = cloned_memory->replace_as_executable();
        if (cloned_memory_or_error.is_error()) {
          return fit::error(impossible_error(cloned_memory_or_error.error_value()));
        }
        cloned_memory = cloned_memory_or_error.value();
      }

      fbl::AllocChecker ac;
      auto mi = ktl::unique_ptr<MemoryInfo>(
          new (&ac) MemoryInfo{.memory = cloned_memory,
                               .size = memory_info.value().size_bytes,
                               .needs_snapshot_on_parent = pager_backed});
      ASSERT(ac.check());
      return fit::ok(ktl::move(mi));
    }
  };

  auto snapshot_memory =
      [](MemoryObject& memory, uint64_t size,
         zx_rights_t rights) -> fit::result<Errno, ktl::unique_ptr<MemoryWrapper>> {
    auto cloned_memory_or_error = memory.create_child(
        (ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE | ZX_VMO_CHILD_RESIZABLE), 0, size);
    if (cloned_memory_or_error.is_error()) {
      return fit::error(get_errno_for_map_err(cloned_memory_or_error.error_value()));
    }

    auto cloned_memory = cloned_memory_or_error.value();

    if ((rights & ZX_RIGHT_EXECUTE) == ZX_RIGHT_EXECUTE) {
      cloned_memory_or_error = cloned_memory->replace_as_executable();
      if (cloned_memory_or_error.is_error()) {
        return fit::error(impossible_error(cloned_memory_or_error.error_value()));
      }
      cloned_memory = cloned_memory_or_error.value();
    }

    fbl::AllocChecker ac;
    auto mw = ktl::unique_ptr<MemoryWrapper>(new (&ac) MemoryWrapper{.memory = cloned_memory});
    ASSERT(ac.check());
    return fit::ok(ktl::move(mw));
  };

  // Hold the lock throughout the operation to uphold memory manager's invariants.
  // See mm/README.md.
  auto& state = *state_.Write();
  auto target_state = target->state_.Write();

  fbl::HashTable<zx_koid_t, ktl::unique_ptr<MemoryInfo>> child_memorys;
  fbl::HashTable<zx_koid_t, ktl::unique_ptr<MemoryWrapper>> replaced_memorys;

  for (auto& [range, mapping] : state.mappings.iter()) {
    if (mapping.flags_.contains(MappingFlagsEnum::DONTFORK)) {
      continue;
    }

    _EP(ktl::visit(
        MappingBacking::overloaded{
            [&, &crange = range,
             &cmapping = mapping](MappingBackingMemory& backing) -> fit::result<Errno> {
              auto memory_offset = backing.memory_offset_ + (crange.start - backing.base_);
              auto length = crange.end - crange.start;

              fbl::RefPtr<MemoryObject> target_memory;
              if (cmapping.flags_.contains(MappingFlagsEnum::SHARED) ||
                  cmapping.name_.type == MappingNameType::Vvar) {
                // Note that the Vvar is a special mapping that behaves like a shared mapping
                // but is private to each process.
                target_memory = backing.memory_;
              } else if (cmapping.flags_.contains(MappingFlagsEnum::WIPEONFORK)) {
                auto memory = create_anonymous_mapping_memory(length) _EP(memory);
                target_memory = memory.value();
              } else {
                MemoryInfo* info;
                auto basic_info = backing.memory_->basic_info();
                auto const child_it = child_memorys.find(backing.memory_->get_koid());
                if (child_it == child_memorys.end()) {
                  auto memory = clone_memory(backing.memory_, basic_info.rights) _EP(memory);
                  info = &*memory.value();
                  child_memorys.insert(ktl::move(memory.value()));
                } else {
                  info = &*child_it;
                }

                if (info->needs_snapshot_on_parent) {
                  MemoryWrapper* replaced_memory;
                  auto replaced_it = replaced_memorys.find(basic_info.koid);
                  if (replaced_it == replaced_memorys.end()) {
                    auto memory = snapshot_memory(*backing.memory_, info->size, basic_info.rights)
                        _EP(memory);
                    replaced_memory = &*memory.value();
                    replaced_memorys.insert(ktl::move(memory.value()));
                  } else {
                    replaced_memory = &*replaced_it;
                  }

                  auto map = map_in_vmar(
                      state.user_vmar_, state.user_vmar_info_,
                      {.type = DesiredAddressType::FixedOverwrite, .address = crange.start},
                      replaced_memory->memory, memory_offset, length, cmapping.flags_, false)
                      _EP(map);

                  backing.memory_ = replaced_memory->memory;
                }
                target_memory = info->memory;
              }

              fbl::Vector<Mapping> released_mappings;
              auto map = target_state->map_memory(
                  target, {.type = DesiredAddressType::Fixed, .address = crange.start},
                  target_memory, memory_offset, length, cmapping.flags_, false, cmapping.name_,
                  released_mappings) _EP(map);
              ASSERT(released_mappings.is_empty());
              return fit::ok();
            },
            [](PrivateAnonymous&) { return fit::result<Errno>(fit::error(errno(ENOSYS))); },
        },
        mapping.backing_.variant));
  }

  (*target_state).forkable_state_ = state.forkable_state_;

  // let self_dumpable = *self.dumpable.lock(locked);
  //*target.dumpable.lock(locked) = self_dumpable;
  return fit::ok();
}

fit::result<zx_status_t> MemoryManager::exec(NamespaceNode exe_node) {
  LTRACE;
  // The previous mapping should be dropped only after the lock to state is released to
  // prevent lock order inversion.
  {
    auto state = state_.Write();
    zx_info_vmar_t info = {};
    zx_status_t status = root_vmar_.get_info(ZX_INFO_VMAR, &info, sizeof(info), nullptr, nullptr);
    if (status != ZX_OK) {
      return fit::error{status};
    }
    // SAFETY: This operation is safe because the VMAR is for another process.
    ASSERT(state->user_vmar_.destroy() == ZX_OK);
    auto user_vmar = create_user_vmar(root_vmar_, info) _EP(user_vmar);
    state->user_vmar_ = ktl::move(user_vmar.value());
    status = state->user_vmar_.get_info(ZX_INFO_VMAR, &state->user_vmar_info_,
                                        sizeof(state->user_vmar_info_), nullptr, nullptr);
    if (status != ZX_OK) {
      return fit::error{status};
    }
    (*state)->brk = {};
    state->forkable_state_.executable_node = exe_node;

    state->mappings.clear();
  }
  auto result = initialize_mmap_layout();
  // TODO (Herrera) Implement Mapping from Errno to zx_status_t
  if (result.is_error()) {
    return fit::error(ZX_ERR_NOT_SUPPORTED);
  }
  return fit::ok();
}

fit::result<Errno> MemoryManager::initialize_mmap_layout() const {
  auto local_state = state_.Write();

  // Place the stack at the end of the address space, subject to ASLR adjustment.
  auto result = UserAddress::from_ptr(ASPACE_HIGHEST_ADDRESS - MAX_STACK_SIZE -
                                      generate_random_offset_for_aslr())
                    .round_up(PAGE_SIZE) _EP(result);
  local_state->forkable_state_.stack_origin = *result;

  // Set the highest address that `mmap` will assign to the allocations that don't ask for a
  // specific address, subject to ASLR adjustment.
  auto sub_result =
      local_state->forkable_state_.stack_origin.checked_sub(generate_random_offset_for_aslr());
  if (!sub_result.has_value()) {
    return fit::error(errno(EINVAL));
  }
  local_state->forkable_state_.mmap_top = *sub_result;
  return fit::ok();
}

void MemoryManager::initialize_mmap_layout_for_test() const {
  initialize_mmap_layout().is_ok();
  auto fake_executable_addr = get_random_base_for_executable(0);
  initialize_brk_origin(fake_executable_addr.value()).is_ok();
}

fit::result<Errno> MemoryManager::initialize_brk_origin(UserAddress executable_end) const {
  auto lstate = state_.Write();

  auto result = executable_end.checked_add(generate_random_offset_for_aslr());
  if (!result.has_value()) {
    return fit::error(errno(EINVAL));
  }
  lstate->forkable_state_.brk_origin = *result;
  return fit::ok();
}

fit::result<Errno, UserAddress> MemoryManager::get_random_base_for_executable(size_t length) const {
  auto lstate = state_.Read();

  // Place it at approx. 2/3 of the available mmap space, subject to ASLR adjustment.
  auto base = round_up_to_system_page_size(2 * lstate->forkable_state_.mmap_top.ptr() / 3).value() +
              generate_random_offset_for_aslr();

  auto based_checked_add = mtl::checked_add(base, length);
  if (!based_checked_add.has_value()) {
    return fit::error(errno(EINVAL));
  }
  if (based_checked_add.value() <= lstate->forkable_state_.mmap_top.ptr()) {
    return fit::ok(UserAddress::from_ptr(*based_checked_add));
  }
  return fit::error(errno(EINVAL));
}

ktl::optional<NamespaceNode> MemoryManager::executable_node() const {
  return state_.Read()->forkable_state_.executable_node;
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManager::unified_read_memory(
    const CurrentTask& current_task, UserAddress addr, ktl::span<uint8_t>& bytes) const {
  DEBUG_ASSERT(has_same_address_space(current_task->mm()));
  return syscall_read_memory(addr, bytes);
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManager::syscall_read_memory(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  return state_.Read()->read_memory(addr, bytes);
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManager::unified_read_memory_partial_until_null_byte(
    const CurrentTask& current_task, UserAddress addr, ktl::span<uint8_t>& bytes) const {
  DEBUG_ASSERT(has_same_address_space(current_task->mm()));
  return syscall_read_memory_partial_until_null_byte(addr, bytes);
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManager::syscall_read_memory_partial_until_null_byte(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  return state_.Read()->read_memory_partial_until_null_byte(addr, bytes);
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManager::unified_read_memory_partial(
    const CurrentTask& current_task, UserAddress addr, ktl::span<uint8_t>& bytes) const {
  DEBUG_ASSERT(has_same_address_space(current_task->mm()));
  return syscall_read_memory_partial(addr, bytes);
}

fit::result<Errno, ktl::span<uint8_t>> MemoryManager::syscall_read_memory_partial(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  return state_.Read()->read_memory_partial(addr, bytes);
}

fit::result<Errno, size_t> MemoryManager::unified_write_memory(
    const CurrentTask& current_task, UserAddress addr,
    const ktl::span<const uint8_t>& bytes) const {
  DEBUG_ASSERT(has_same_address_space(current_task->mm()));
  return syscall_write_memory(addr, bytes);
}

fit::result<Errno, size_t> MemoryManager::syscall_write_memory(
    UserAddress addr, const ktl::span<const uint8_t>& bytes) const {
  return state_.Read()->write_memory(addr, bytes);
}

fit::result<Errno, size_t> MemoryManager::unified_write_memory_partial(
    const CurrentTask& current_task, UserAddress addr,
    const ktl::span<const uint8_t>& bytes) const {
  DEBUG_ASSERT(has_same_address_space(current_task->mm()));
  return syscall_write_memory_partial(addr, bytes);
}

fit::result<Errno, size_t> MemoryManager::syscall_write_memory_partial(
    UserAddress addr, const ktl::span<const uint8_t>& bytes) const {
  return state_.Read()->write_memory_partial(addr, bytes);
}

fit::result<Errno, size_t> MemoryManager::unified_zero(const CurrentTask& current_task,
                                                       UserAddress addr, size_t length) const {
  DEBUG_ASSERT(has_same_address_space(current_task->mm()));

  {
    auto page_size = static_cast<size_t>(PAGE_SIZE);
    // Get the page boundary immediately following `addr` if `addr` is
    // not page aligned.
    auto next_page_boundary = round_up_to_system_page_size(addr.ptr());
    if (next_page_boundary.is_error()) {
      return next_page_boundary.take_error();
    }
    // The number of bytes needed to zero at least a full page (not just
    // a pages worth of bytes) starting at `addr`.
    auto length_with_atleast_one_full_page = page_size + (next_page_boundary.value() - addr.ptr());
    // If at least one full page is being zeroed, go through the memory object since Zircon
    // can swap the mapped pages with the zero page which should be cheaper than zeroing
    // out a pages worth of bytes manually.
    //
    // If we are not zeroing out a full page, then go through usercopy
    // if unified aspaces is enabled.
    if (length >= length_with_atleast_one_full_page) {
      return syscall_zero(addr, length);
    }
  }
  return syscall_zero(addr, length);
}

fit::result<Errno, size_t> MemoryManager::syscall_zero(UserAddress addr, size_t length) const {
  return state_.Read()->zero(addr, length);
}

fit::result<Errno> MemoryManager::unmap(UserAddress addr, size_t length) {
  fbl::Vector<Mapping> released_mappings;
  {
    auto state = state_.Write();
    _EP(state->unmap(fbl::RefPtr<MemoryManager>(this), addr, length, released_mappings));
  }

  // Drop the state before the unmapped mappings, since dropping a mapping may acquire a lock
  // in `DirEntry`'s `drop`.
  released_mappings.reset();

  return fit::ok();
}

fit::result<Errno> MemoryManager::protect(UserAddress addr, size_t length,
                                          ProtectionFlags prot_flags) {
  auto state = state_.Write();
  return state->protect(addr, length, prot_flags);
}

fit::result<Errno> MemoryManager::set_mapping_name(UserAddress addr, size_t length,
                                                   ktl::optional<FsString> name) {
  if ((addr.ptr() % PAGE_SIZE) != 0) {
    return fit::error(errno(EINVAL));
  }

  auto end = addr.checked_add(length);
  if (end.has_value()) {
    auto new_end =
        end->round_up(PAGE_SIZE).map_error([](Errno e) { return errno(ENOMEM); }) _EP(new_end);
    end = new_end.value();
  } else {
    return fit::error(errno(EINVAL));
  }

  auto state = state_.Write();

  fbl::Vector<ktl::pair<util::Range<UserAddress>, Mapping>> mappings_in_range;
  auto mappings =
      state->mappings.intersection(util::Range<UserAddress>({.start = addr, .end = end.value()}));
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
      auto start_split_range = util::Range<UserAddress>{.start = range.start, .end = addr};
      auto start_split_length = addr - range.start;

      auto start_split_mapping =
          ktl::visit(MappingBacking::overloaded{
                         [&, &crange = range, &cmapping = mapping](MappingBackingMemory& backing) {
                           // Shrink the range of the named mapping to only the named area.
                           backing.memory_offset_ = start_split_length;
                           auto new_mapping = Mapping::New(crange.start, ktl::move(backing.memory_),
                                                           backing.memory_offset_,
                                                           MappingFlagsImpl(cmapping.flags_));
                           return new_mapping;
                         },
                         [](PrivateAnonymous&) {
                           return Mapping::New(UserAddress::NULL_, fbl::RefPtr<MemoryObject>(), 0,
                                               MappingFlagsImpl(MappingFlags::empty()));
                         },
                     },
                     mapping.backing_.variant);

      state->mappings.insert(start_split_range, start_split_mapping);
      range = util::Range<UserAddress>{.start = addr, .end = range.end};
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

    // TODO(b/310255065): We have no place to store names in a way visible to programs outside
    // of Starnix such as memory analysis tools.
#if STARNIX_ANON_ALLOCS
#endif
    if (range.end > end) {
      // The named region ends before the last mapping ends. Split the tail off of the
      // last mapping to have an unnamed mapping after the named region.
      auto tail_range = util::Range<UserAddress>{.start = end.value(), .end = range.end};
      auto tail_offset = range.end - end.value();
      auto tail_mapping =
          ktl::visit(MappingBacking::overloaded{
                         [&, &cmapping = mapping](MappingBackingMemory& backing) {
                           auto new_mapping = Mapping::New(end.value(), ktl::move(backing.memory_),
                                                           backing.memory_offset_ + tail_offset,
                                                           MappingFlagsImpl(cmapping.flags_));
                           return new_mapping;
                         },
                         [](PrivateAnonymous&) {
                           return Mapping::New(UserAddress::NULL_, fbl::RefPtr<MemoryObject>(), 0,
                                               MappingFlagsImpl(MappingFlags::empty()));
                         },
                     },
                     mapping.backing_.variant);

      state->mappings.insert(tail_range, tail_mapping);
      range.end = end.value();
    }

    if (name.has_value()) {
      mapping.name_ = MappingName{.type = MappingNameType::Vma, .vmaName = name.value()};
    }
    state->mappings.insert(range, mapping);
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
  auto state = state_.Read();
  auto pair = state->mappings.get(addr);
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

  // Hold the lock throughout the operation to uphold memory manager's invariants.
  // See mm/README.md.
  auto state = state_.Write();
  auto result = state->map_anonymous(fbl::RefPtr<MemoryManager>(this), addr, length, prot_flags,
                                     options, ktl::move(name), released_mappings) _EP(result);

  // Drop the state before the unmapped mappings, since dropping a mapping may acquire a lock
  // in `DirEntry`'s `drop`.
  ktl::destroy_at(ktl::addressof(state));
  released_mappings.reset();

  return result;
}

fit::result<Errno, UserAddress> MemoryManager::map_stack(size_t length,
                                                         ProtectionFlags prot_flags) {
  ZX_ASSERT(length <= MAX_STACK_SIZE);
  auto addr = state_.Read()->forkable_state_.stack_origin;
  return map_anonymous({.type = DesiredAddressType::Fixed, .address = addr}, length, prot_flags,
                       MappingOptionsFlags(MappingOptions::ANONYMOUS),
                       {.type = MappingNameType::Stack});
}

size_t MemoryManager::get_mapping_count() {
  auto state = state_.Read();
  return state->mappings.iter().size();
}

#if 0
UserAddress MemoryManager::get_random_base(size_t length) {
  return state.Read()->user_vmars.get_random_base(length);
}
#endif

fit::result<Errno, fbl::RefPtr<MemoryObject>> create_anonymous_mapping_memory(uint64_t size) {
  // let mut profile = ProfileDuration::enter("CreatAnonVmo");
  // mremap can grow memory regions, so make sure the memory object is resizable.
  zx::vmo vmo;
  auto s = zx::vmo::create(size, ZX_VMO_RESIZABLE, &vmo);
  if (s != ZX_OK) {
    switch (s) {
      case ZX_ERR_NO_MEMORY:
      case ZX_ERR_OUT_OF_RANGE:
        return fit::error(errno(ENOMEM));
      default:
        impossible_error(s);
    }
  }

  auto memory = MemoryObject::From(ktl::move(vmo));
  // profile.pivot("SetAnonVmoName");
  memory->set_zx_name("starnix-anon");

  // profile.pivot("ReplaceAnonVmoAsExecutable");
  // TODO(https://fxbug.dev/42056890): Audit replace_as_executable usage
  auto result = memory->replace_as_executable().map_error(
      [](zx_status_t s) -> Errno { return impossible_error(s); });
  return fit::ok(ktl::move(*result));
}

}  // namespace starnix
