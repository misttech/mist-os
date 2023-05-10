// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <vm/physmap.h>
#include <vm/tri_page_storage.h>

namespace {

constexpr size_t mid_offset_for_len(size_t len) {
  DEBUG_ASSERT(len != 0);
  // len is rounded up so that the offset returned is valid from both the left and right. For an odd
  // length we could provide an extra byte to one side, but this does not seem worth the complexity.
  return (PAGE_SIZE / 2) - ((len + 1) / 2);
}

// The offset of the middle storage is the same as how much space is available on a side. This
// wrapper provides clarity in call sites.
constexpr size_t side_space_avail_with_mid(size_t mid_len) { return mid_offset_for_len(mid_len); }

// Calculates how much space would be available in the middle slot given a hypothetical left and
// right slot usage.
constexpr size_t mid_space_avail_with(size_t left, size_t right) {
  const size_t side_used = ktl::max(left, right);
  if (side_used >= PAGE_SIZE / 2) {
    return 0;
  }
  return ((PAGE_SIZE / 2) - side_used) * 2;
}

size_t left_space_avail(vm_page_t* page) {
  if (page->zram.left_compress_size != 0) {
    return 0;
  }
  // If the middle slot is used it will be what constrains us, otherwise the right slot.
  if (page->zram.mid_compress_size != 0) {
    return side_space_avail_with_mid(page->zram.mid_compress_size);
  }
  return PAGE_SIZE - page->zram.right_compress_size;
}

size_t mid_space_avail(vm_page_t* page) {
  if (page->zram.mid_compress_size != 0) {
    return 0;
  }
  return mid_space_avail_with(page->zram.left_compress_size, page->zram.right_compress_size);
}

size_t right_space_avail(vm_page_t* page) {
  if (page->zram.right_compress_size != 0) {
    return 0;
  }
  // If the middle slot is used it will be what constrains us, otherwise the left slot.
  if (page->zram.mid_compress_size != 0) {
    return side_space_avail_with_mid(page->zram.mid_compress_size);
  }
  return PAGE_SIZE - page->zram.left_compress_size;
}

bool page_is_empty(vm_page_t* page) {
  return page->zram.left_compress_size == 0 && page->zram.mid_compress_size == 0 &&
         page->zram.right_compress_size == 0;
}

void initialize_page(vm_page_t* page) {
  DEBUG_ASSERT(page);
  DEBUG_ASSERT(!list_in_list(&page->queue_node));
  // Page should be in the alloc state so we can transition it to the ZRAM state.
  DEBUG_ASSERT(page->state() == vm_page_state::ALLOC);
  page->set_state(vm_page_state::ZRAM);
  page->zram.left_compress_size = 0;
  page->zram.mid_compress_size = 0;
  page->zram.right_compress_size = 0;
}

}  // namespace

VmTriPageStorage::VmTriPageStorage() {
  list_initialize(&full_pages_);
  for (auto& bucket : buckets_) {
    list_initialize(&bucket);
  }
}

VmTriPageStorage::~VmTriPageStorage() {
  ASSERT(list_is_empty(&full_pages_));
  for (auto& bucket : buckets_) {
    ASSERT(list_is_empty(&bucket));
  }
  ASSERT(stored_items_ == 0);
}

// static
bool VmTriPageStorage::page_is_full(vm_page_t* page) {
  return left_space_avail(page) < kBucketSize && mid_space_avail(page) < kBucketSize &&
         right_space_avail(page) < kBucketSize;
}

// static
VmTriPageStorage::PageSlot VmTriPageStorage::select_slot(vm_page_t* page, size_t len) {
  DEBUG_ASSERT(!page_is_empty(page));
  DEBUG_ASSERT(page->state() == vm_page_state::ZRAM);
  // There are either 1 or 2 slots available based on our size. Three cannot be available, since
  // this is partially in use. If two are available then pick the choice that leaves the largest
  // remaining.
  PageSlot choice = PageSlot::None;
  if (left_space_avail(page) >= len) {
    choice = PageSlot::Left;
  }
  if (mid_space_avail(page) >= len) {
    if (choice == PageSlot::None) {
      choice = PageSlot::Mid;
    } else {
      size_t left_space_after = side_space_avail_with_mid(len);
      size_t mid_space_after = mid_space_avail_with(len, page->zram.right_compress_size);
      if (left_space_after > mid_space_after) {
        choice = PageSlot::Mid;
      }
    }
  }
  if (right_space_avail(page) >= len) {
    if (choice == PageSlot::None) {
      choice = PageSlot::Right;
    } else if (choice == PageSlot::Mid) {
      size_t right_space_after = side_space_avail_with_mid(len);
      size_t mid_space_after = mid_space_avail_with(page->zram.left_compress_size, len);
      if (mid_space_after > right_space_after) {
        choice = PageSlot::Right;
      }
    } else {
      // Left and right are equivalent choices, so leave it at left.
    }
  }
  return choice;
}

// static
size_t VmTriPageStorage::offset_for_slot(PageSlot slot, size_t len) {
  DEBUG_ASSERT(slot != PageSlot::None);
  DEBUG_ASSERT(len > 0);
  switch (slot) {
    case PageSlot::Left:
      return 0;
    case PageSlot::Mid:
      return mid_offset_for_len(len);
    case PageSlot::Right:
      return PAGE_SIZE - len;
    default:
      ASSERT(false);
  }
}

// static
void* VmTriPageStorage::addr_for_slot(vm_page_t* page, PageSlot slot, size_t len) {
  const size_t offset = offset_for_slot(slot, len);
  DEBUG_ASSERT(offset < PAGE_SIZE);
  return reinterpret_cast<void*>(reinterpret_cast<uintptr_t>(paddr_to_physmap(page->paddr())) +
                                 offset);
}

// static
void VmTriPageStorage::set_slot_length(vm_page_t* page, PageSlot slot, uint16_t len) {
  DEBUG_ASSERT(slot != PageSlot::None);
  DEBUG_ASSERT(page->state() == vm_page_state::ZRAM);
  // Must not be modified the page while it's in any list, this aims to catch misuse of modifying
  // before first removing from the buckets.
  DEBUG_ASSERT(!list_in_list(&page->queue_node));
  switch (slot) {
    case PageSlot::Left:
      page->zram.left_compress_size = len;
      break;
    case PageSlot::Mid:
      page->zram.mid_compress_size = len;
      break;
    case PageSlot::Right:
      page->zram.right_compress_size = len;
      break;
    default:
      ASSERT(false);
  }
}

// static
size_t VmTriPageStorage::get_slot_length(vm_page_t* page, PageSlot slot) {
  switch (slot) {
    case PageSlot::Left:
      return page->zram.left_compress_size;
    case PageSlot::Mid:
      return page->zram.mid_compress_size;
    case PageSlot::Right:
      return page->zram.right_compress_size;
    default:
      ASSERT(false);
  }
  return 0;
}

// static
std::pair<vm_page_t*, VmTriPageStorage::PageSlot> VmTriPageStorage::DecodeRef(CompressedRef ref) {
  uint64_t compressed_ref = ref.value();
  ASSERT(compressed_ref != 0);
  vm_page_t* p = reinterpret_cast<vm_page_t*>((compressed_ref >> kRefPageShift) | kRefHighBits);
  PageSlot slot = static_cast<PageSlot>((compressed_ref >> kRefTagShift) & BIT_MASK(kRefTagBits));
  ASSERT(slot != PageSlot::None);
  return {p, slot};
}

// static
VmTriPageStorage::CompressedRef VmTriPageStorage::EncodeRef(vm_page_t* page, PageSlot slot) {
  ASSERT(page);
  ASSERT(slot != PageSlot::None);

  return CompressedRef((reinterpret_cast<uint64_t>(page) << kRefPageShift) |
                       (static_cast<uint64_t>(slot) << kRefTagShift));
}

std::pair<const void*, size_t> VmTriPageStorage::CompressedData(CompressedRef ref) const {
  auto [page, slot] = DecodeRef(ref);

  DEBUG_ASSERT(page->state() == vm_page_state::ZRAM);
  DEBUG_ASSERT(slot != PageSlot::None);
  const size_t src_size = get_slot_length(page, slot);
  return {addr_for_slot(page, slot, src_size), src_size};
}

uint64_t VmTriPageStorage::bucket_for_page(vm_page_t* page) {
  DEBUG_ASSERT(!page_is_empty(page));
  DEBUG_ASSERT(!page_is_full(page));
  // Find the largest available region.
  const uint64_t max_avail =
      ktl::max({left_space_avail(page), mid_space_avail(page), right_space_avail(page)});
  // For this to be false the page_is_full check would have failed.
  DEBUG_ASSERT(max_avail >= kBucketSize);
  // the bucket a page can go into to satisfy an allocation is a rounded down version of its size.
  // e.g:
  // [0,kBucketSize) -> Invalid
  // [kBucketSize, kBucketSize*2) -> 0
  // etc
  // This way everything in bucket 0 is at least large enough to hold something of kBucketSize, and
  // more generally this gives the required property of a given bucket K can hold at least
  // (K+1)*kBucketSize.
  return (max_avail / kBucketSize) - 1;
}

vm_page_t* VmTriPageStorage::AllocateBucketLocked(size_t len) {
  DEBUG_ASSERT(len > 0 && len <= PAGE_SIZE);
  // Calculate the first bucket that can safely satisfy an allocation of this size. This effectively
  // rounds up our len to the next bucket size when picking an allocation spot. Since we know that
  // bucket K can support an allocation of (K+1)*kBucketSize we want to map allocations like so:
  //   (0, kBucketSize] -> 0
  //   (kBucketSize, 2 * kBucketSize] -> 1
  //   ...
  //   (K*kBucketSize, (K+1) * kBucketSize] -> K
  // Note that the start of the range is exclusive and the end is inclusive.
  // Therefore we need to solve the following inequality:
  //   K*kBucketSize < len <= (K+1) * kBucketSize
  //   (K*kBucketSize) - 1 < len - 1 <= ((K+1) * kBucketSize) - 1  (subtract 1 everywhere)
  //   K - 1/kBucketSize < (len - 1) / kBucketSize <= (K+1) - 1/kBucketSize (divide by kBucketSize)
  //   K - 1/kBucketSize < (len - 1) / kBucketSize < (K+1)  (relax <= to <)
  // As we are only interested in the integer values of K we can see that (len - 1)/kBucketSize is
  // definitely < K+1, and will be the next integer value >K-1/kBucketSize.
  const uint64_t first_bucket = (len - 1) / kBucketSize;
  // Create a mask of all buckets greater than or equal to the first_bucket
  const uint64_t valid_buckets_mask = ~((1ul << first_bucket) - 1);
  // Apply this mask to the available buckets to see what we can actually allocate from.
  const uint64_t available_buckets = non_empty_buckets_ & valid_buckets_mask;
  // See if any are actually available.
  if (available_buckets == 0) {
    return nullptr;
  }
  // Select the smallest bucket available to allocate from. This might be wasteful and overfill a
  // slot, but since we have a strict 3 slots per page strategy if all of our data is small (i.e.
  // compresses well) then to get optimal memory usage we will have to place small items in large
  // slots and so it is not obviously bad.
  // TODO(fxbug.dev/60238): Consider if there are scenarios that we can detect where this
  // fragmentation is undesirable.
  const uint64_t bucket = __builtin_ctzl(available_buckets);
  DEBUG_ASSERT(bucket < kNumBuckets);
  ASSERT(!list_is_empty(&buckets_[bucket]));
  vm_page_t* ret = list_remove_head_type(&buckets_[bucket], vm_page_t, queue_node);
  bucket_pages_counts_[bucket]--;
  // Check if the bucket is now empty.
  if (bucket_pages_counts_[bucket] == 0) {
    DEBUG_ASSERT(list_is_empty(&buckets_[bucket]));
    non_empty_buckets_ &= ~(1ul << bucket);
  }
  return ret;
}

void VmTriPageStorage::InsertBucketLocked(vm_page_t* page) {
  DEBUG_ASSERT(page->state() == vm_page_state::ZRAM);
  DEBUG_ASSERT(!list_in_list(&page->queue_node));
  const uint64_t bucket = bucket_for_page(page);
  DEBUG_ASSERT(bucket < kNumBuckets);
  if (bucket_pages_counts_[bucket] == 0) {
    DEBUG_ASSERT(list_is_empty(&buckets_[bucket]));
    non_empty_buckets_ |= (1ul << bucket);
  }
  list_add_head(&buckets_[bucket], &page->queue_node);
  bucket_pages_counts_[bucket]++;
}

void VmTriPageStorage::RemovePageFromBucketLocked(vm_page_t* page) {
  DEBUG_ASSERT(page->state() == vm_page_state::ZRAM);
  DEBUG_ASSERT(list_in_list(&page->queue_node));
  const uint64_t bucket = bucket_for_page(page);
  DEBUG_ASSERT(bucket < kNumBuckets);
  list_delete(&page->queue_node);
  bucket_pages_counts_[bucket]--;
  if (bucket_pages_counts_[bucket] == 0) {
    DEBUG_ASSERT(list_is_empty(&buckets_[bucket]));
    non_empty_buckets_ &= ~(1ul << bucket);
  }
}

std::pair<ktl::optional<VmCompressedStorage::CompressedRef>, vm_page_t*> VmTriPageStorage::Store(
    vm_page_t* buffer_page, size_t len) {
  canary_.Assert();
  DEBUG_ASSERT(len > 0 && len <= PAGE_SIZE);
  DEBUG_ASSERT(buffer_page);
  DEBUG_ASSERT(!list_in_list(&buffer_page->queue_node));
  // Track the memcpy we need to do after the lock is dropped.
  void* dest_addr = nullptr;
  CompressedRef return_ref(0);
  {
    // Search for an existing location.
    Guard<Mutex> guard{&lock_};
    // Remove a page from an appropriate bucket.
    vm_page_t* page = AllocateBucketLocked(len);
    if (page) {
      const PageSlot slot = select_slot(page, len);
      ASSERT(slot != PageSlot::None);
      // Based on the slot, select the correct destination address, and write the length in.
      dest_addr = addr_for_slot(page, slot, len);
      set_slot_length(page, slot, static_cast<uint16_t>(len));
      return_ref = EncodeRef(page, slot);
    } else {
      // Take the passed in buffer_page instead, no need to memcpy.
      page = buffer_page;
      buffer_page = nullptr;
      initialize_page(page);
      set_slot_length(page, PageSlot::Left, static_cast<uint16_t>(len));
      return_ref = EncodeRef(page, PageSlot::Left);
    }

    // Return the page to the buckets if it has space.
    if (!page_is_full(page)) {
      InsertBucketLocked(page);
    } else {
      list_add_tail(&full_pages_, &page->queue_node);
      full_pages_count_++;
    }
    stored_items_++;
    total_compressed_item_size_ += len;
  }
  // Perform any remaining mempcy
  if (dest_addr) {
    ASSERT(buffer_page);
    memcpy(dest_addr, paddr_to_physmap(buffer_page->paddr()), len);
  }
  // Make sure we got a proper ref by the end.
  ASSERT(return_ref.value() != 0);
  // Return the reference, and the buffer_page if we didn't consume it.
  return {return_ref, buffer_page};
}

void VmTriPageStorage::Free(CompressedRef ref) {
  canary_.Assert();
  auto [page, slot] = DecodeRef(ref);
  DEBUG_ASSERT(page->state() == vm_page_state::ZRAM);
  DEBUG_ASSERT(get_slot_length(page, slot) != 0);

  {
    Guard<Mutex> pages_guard{&lock_};
    DEBUG_ASSERT(!page_is_empty(page));
    if (page_is_full(page)) {
      list_delete(&page->queue_node);
      full_pages_count_--;
    } else {
      // Although list_delete would remove the page from the bucket tracking list, we need to update
      // the correct bucket counts.
      RemovePageFromBucketLocked(page);
    }
    const uint64_t len = get_slot_length(page, slot);
    set_slot_length(page, slot, 0);
    if (page_is_full(page)) {
      // The page could still be full after freeing a slot if a very small allocation was placed in
      // a side slot, and then a large allocation in the middle slot. The middle allocation can
      // cause the free space after free of either end slot to be less than the threshold needed to
      // be added to the bucket list.
      list_add_tail(&full_pages_, &page->queue_node);
      full_pages_count_++;
      page = nullptr;
    } else if (!page_is_empty(page)) {
      InsertBucketLocked(page);
      page = nullptr;
    }
    stored_items_--;
    DEBUG_ASSERT(len <= total_compressed_item_size_);
    total_compressed_item_size_ -= len;
  }
  if (page) {
    DEBUG_ASSERT(page->state() == vm_page_state::ZRAM && page_is_empty(page));
    pmm_free_page(page);
  }
}

VmTriPageStorage::MemoryUsage VmTriPageStorage::GetMemoryUsage() const {
  canary_.Assert();
  Guard<Mutex> pages_guard{&lock_};
  uint64_t total = full_pages_count_;
  for (uint64_t bucket_pages_count : bucket_pages_counts_) {
    total += bucket_pages_count;
  }
  return MemoryUsage{.uncompressed_content_bytes = stored_items_ * PAGE_SIZE,
                     .compressed_storage_bytes = total * PAGE_SIZE,
                     .compressed_storage_used_bytes = total_compressed_item_size_};
}

void VmTriPageStorage::Dump() const {
  canary_.Assert();
  Guard<Mutex> pages_guard{&lock_};
  uint64_t bucket_totals = 0;
  for (uint64_t bucket_pages_count : bucket_pages_counts_) {
    bucket_totals += bucket_pages_count;
  }
  printf("[ZRAM]: Storing %" PRIu64 " items with compressed size %" PRIu64 " using %" PRIu64
         " pages\n",
         stored_items_, total_compressed_item_size_, (full_pages_count_ + bucket_totals));
  printf("[ZRAM]: %" PRIu64 " pages considered full, non-empty buckets:\n", full_pages_count_);
  for (size_t i = 0; i < kNumBuckets; i++) {
    if (bucket_pages_counts_[i] > 0) {
      printf("[ZRAM]: Bucket %zu: %" PRIu64 " pages\n", i, bucket_pages_counts_[i]);
    }
  }
}
