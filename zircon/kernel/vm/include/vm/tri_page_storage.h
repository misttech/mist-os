// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_TRI_PAGE_STORAGE_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_TRI_PAGE_STORAGE_H_

#include <fbl/canary.h>
#include <vm/compression.h>

// Tri-page storage is an allocator optimized around the expectations of compressed pages. At the
// high level it works by attempting to pack up to 3 compressed pages into a single physical page.
//
// The storage works by taking a physical page, and giving it three notional slots; a left, a right
// and a middle. At most one piece of data can be stored at each slot, and data stored in one slot
// limits, or completely prevents, data stored in another slot. For example, if something of
// PAGE_SIZE/2 or larger is stored in either the left or right slot, then nothing can now be stored
// in the middle slot as it would necessarily overlap.
//
// Contents of each slot is recorded in the vm_page_t::zram.*_compress_size. For any non-zero size
// The contents of the page then are
//  * [0, left_compress_size)
//  * [PAGE_SIZE/2 - (mid_compress_size+1)/2, PAGE_SIZE/2 + (mid_compress_size+1)/2)
//  * [PAGE_SIZE - right_compress_size, PAGE_SIZE)
// As per previous paragraph, these regions may not overlap, and so only one or two slots being used
// could cause the page to be full. Note that for the middle slot the range considered used in the
// page might be 1 byte larger as it is rounded up to the next even size for implementation
// simplicity.
//
// Pages are either empty, full, or have partial space. A full page is one where either all three
// slots are in use, or no slot could be used without overlapping with existing data. Empty pages
// have no slots in use. Partial pages are stored by taking the log2 of the largest item they could
// store in any of the remaining slots and using that as the index into bucket lists.
class VmTriPageStorage final : public VmCompressedStorage {
 public:
  VmTriPageStorage();
  ~VmTriPageStorage() override;

  void Free(CompressedRef ref) override;
  std::pair<ktl::optional<CompressedRef>, vm_page_t*> Store(vm_page_t* page, size_t len) override;
  std::pair<const void*, size_t> CompressedData(CompressedRef ref) const override;
  void Dump() const override;
  MemoryUsage GetMemoryUsage() const override;

 private:
  // Attempts to find an existing partially used page that can satisfy an allocation of the
  // specified length. Returns nullptr if none is found. Len must be in range (0,PAGE_SIZE].
  vm_page_t* AllocateBucketLocked(size_t len) TA_REQ(lock_);

  // Adds a page to the partially used page buckets. The page must be neither empty nor full. The
  // bucket it is added to is determined based on its largest free slot.
  void InsertBucketLocked(vm_page_t* page) TA_REQ(lock_);

  // Removes a specific page from the buckets. It knows which bucket to remove from by the largest
  // free slot, and so the slot information should not have been modified between this and
  // InsertBucketLocked.
  void RemovePageFromBucketLocked(vm_page_t* page) TA_REQ(lock_);

  // Represents which of the three potential slots in a page that an allocation resides. Although
  // can have multiple slots in use, a given allocation exists at a unique slot.
  enum class PageSlot : uint64_t {
    // None is explicitly defined as a 0 encoding to help detect usage errors of CompressedRefs.
    None = 0,
    Left = 1,
    Mid = 2,
    Right = 3,
    // Number of slots. Used by static assertions to ensure we can bit pack everything.
    NumSlots,
  };

  // Returns the bucket a page would be in based on its free space. Requires the page be neither
  // empty nor full.
  static uint64_t bucket_for_page(vm_page_t* page);

  // Splits a reference into the specific page and slot. The returned slot will never be None.
  static std::pair<vm_page_t*, PageSlot> DecodeRef(CompressedRef ref);

  // Encodes a specific page and slot into a reference. Slot must not be None.
  static CompressedRef EncodeRef(vm_page_t* page, PageSlot slot);

  // Returns whether the page should be considered full and cannot store anything more.
  static bool page_is_full(vm_page_t* page);

  // Given a page and size, returns the 'best' slot to be used for storage. Assumes that the page is
  // not empty and that there is at least one slot available of the requested size.
  static PageSlot select_slot(vm_page_t* page, size_t len);

  // Returns a kernel usable address for a slot of the given size in the specified page. This does
  // not assume that the given slot is allocated in the page, just returns the address as if it
  // were.
  static void* addr_for_slot(vm_page_t* page, PageSlot slot, size_t len);

  // Calculate the offset for a slot from the start of a page.
  static size_t offset_for_slot(PageSlot slot, size_t len);

  // Updates the book keeping the given page to set the specific slot as having an allocation of
  // len.
  static void set_slot_length(vm_page_t* page, PageSlot slot, uint16_t len);

  // Retrieves the length of a particular slot in a page.
  static size_t get_slot_length(vm_page_t* page, PageSlot slot);

  // The CompressedRef's we generate need to encode a pointer to a vm_page_t, and indicate which of
  // the |Slot| this reference is for. They have the following bit layout:
  // 63       0
  // P..PTTA..A
  // This layout is variable based on the number of bits reserved by the CompressedRef, indicated
  // by its kAlignBits. After that alignment bits there are 2 bits for storing the slot tag, and
  // then all remaining bits store the pointer to the vm_page_t.
  // To pack a 64-bit vm_page_t pointer into <64 bits, the assumption of kernel pointer having spare
  // high bits always being set is leveraged.
  static constexpr uint64_t kRefTagShift = CompressedRef::kAlignBits;
  static constexpr uint64_t kRefTagBits = 2;
  static constexpr uint64_t kRefPageShift = kRefTagShift + kRefTagBits;
  static constexpr uint64_t kRefHighBits = BIT_MASK(kRefPageShift)
                                           << ((sizeof(uint64_t) * 8) - kRefPageShift);
  // Check that all pointers in the kernel region, that is any value >KERNEL_ASPACE_BASE, will have
  // the high bits always set.
  static_assert((KERNEL_ASPACE_BASE & kRefHighBits) == kRefHighBits);
  // Check that there are enough tag bits to encode all the possible PageSlots
  static_assert(1ul << kRefTagBits >= static_cast<uint64_t>(PageSlot::NumSlots));

  // For simplicity select our number of buckets such that we can use a single uint64_t bitmap to
  // track availability.
  static constexpr size_t kNumBuckets = sizeof(uint64_t) * 8;
  // Buckets are of uniform size, with bucket N supporting an allocation of at most
  // (N+1)*kBucketSize.
  static constexpr uint64_t kBucketSize = PAGE_SIZE / kNumBuckets;

  fbl::Canary<fbl::magic("3PS_")> canary_;

  mutable DECLARE_MUTEX(VmTriPageStorage) lock_;

  // Informational counter of how many items are stored, i.e. how many CompressedRef's we have
  // vended.
  uint64_t stored_items_ TA_GUARDED(lock_) = 0;

  // The total compressed size of all the items being stored.
  uint64_t total_compressed_item_size_ TA_GUARDED(lock_) = 0;

  // Pages that we consider full and cannot have additional items stored in them.
  list_node_t full_pages_ TA_GUARDED(lock_);
  uint64_t full_pages_count_ TA_GUARDED(lock_) = 0;

  // The buckets are used to track available ranges in partially used pages and are a simple
  // linearly increasing size. The smallest amount of free space that is tracked before a page is
  // considered full is kBucketSize and so bucket 0 tracks pages that can store at least
  // kBucketSize.
  // More generally any bucket K can store at least (K+1)*kBucketSize, and a page is placed in the
  // largest bucket possible for its free space.
  // The non_empty_buckets_ is a bitmap optimization around what buckets_ are non-empty to avoid
  // linear searches.
  uint64_t non_empty_buckets_ TA_GUARDED(lock_) = 0;
  list_node_t buckets_[kNumBuckets] TA_GUARDED(lock_);
  uint64_t bucket_pages_counts_[kNumBuckets] TA_GUARDED(lock_) = {};
};

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_TRI_PAGE_STORAGE_H_
