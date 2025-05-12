// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_PAGE_SLAB_ALLOCATOR_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_PAGE_SLAB_ALLOCATOR_H_

#include <pow2.h>

#include <arch/defines.h>
#include <ktl/algorithm.h>
#include <ktl/iterator.h>
#include <ktl/span.h>
#include <vm/page.h>
#include <vm/physmap.h>
#include <vm/pmm.h>

// Simple slab allocator that uses a vm_page_t as its slab to perform allocations out of. This makes
// the allocator only suitable for small objects, preferably ones that divide evenly into a page.
//
// All per-slab metadata is stored in the vm_page_t itself and, by default, the only dependency of
// the allocator is the PMM to allocate and free pages from. The heap is not needed for any other
// metadata allocations.
//
// Free regions are tracked in a two level list with each slab having an internal list of free
// regions, and the allocator itself having a list of slabs that have at least one free region.
//
// Slabs that become fully empty are able to be returned to the PMM, although allocations cannot be
// moved between slabs, so fragmentation can still occur.
//
// The allocator can be specialized to control the actual allocation and freeing of the slabs
// themselves.
//
// This class it not thread safe.
template <typename T>
class PageSlabAllocator {
 public:
  PageSlabAllocator() = default;
  ~PageSlabAllocator() {
    ASSERT(list_is_empty(&full_slabs_));
    ASSERT(list_is_empty(&available_slabs_));
  }

  template <typename... ConstructorArgs>
  T* New(ConstructorArgs&&... args) {
    Entry* entry = Allocate();
    if (!entry) {
      return nullptr;
    }
    return new (&entry->object) T(ktl::forward<ConstructorArgs>(args)...);
  }
  void Delete(T* object) {
    object->~T();
    Free(object);
  }
  size_t AllocatedSlabs() const { return allocated_slabs_; }

  // Helper to return the number of slabs this allocator would allocate to store the specified
  // number of objects.
  static constexpr uint32_t SlabsRequired(uint32_t num_objects) {
    return fbl::round_up(num_objects, kObjectsPerSlab) / kObjectsPerSlab;
  }

 protected:
  union Entry {
    Entry() : next(0) {}
    ~Entry() {}
    uint32_t next;
    T object;
  };
  static constexpr uint32_t kObjectSize = sizeof(Entry);
  // As we allocate objects linearly along the page ensure that this will be valid for the objects
  // alignment.
  static_assert(fbl::round_up(sizeof(Entry), alignof(Entry)) == sizeof(Entry));
  // Object needs to fit in a page.
  static_assert(kObjectSize < PAGE_SIZE);
  static constexpr uint32_t kObjectsPerSlab = PAGE_SIZE / kObjectSize;
  static constexpr uint32_t kEndOfList = UINT32_MAX;
  ktl::pair<vm_page_t*, uint32_t> ObjectToSlab(const void* object) const {
    const uint32_t offset = reinterpret_cast<uintptr_t>(object) % PAGE_SIZE;
    vm_page_t* page = PaddrToPage(physmap_to_paddr(object));
    ASSERT(page && page->state() == vm_page_state::SLAB);
    DEBUG_ASSERT(list_in_list(&page->queue_node));
    DEBUG_ASSERT(offset % kObjectSize == 0);
    return {page, offset / kObjectSize};
  }
  static Entry* GetEntry(vm_page_t* slab, uint32_t index) {
    ASSERT(slab->state() == vm_page_state::SLAB);
    DEBUG_ASSERT(index < kObjectsPerSlab);
    return reinterpret_cast<Entry*>(paddr_to_physmap(slab->paddr())) + index;
  }
  void DebugFreeAllSlabs() {
    Pmm::Node().FreeList(&full_slabs_);
    Pmm::Node().FreeList(&available_slabs_);
  }
  virtual vm_page_t* AllocSlab() {
    vm_page_t* page = Pmm::Node().AllocPage(0).value_or(nullptr);
    if (page) {
      page->set_state(vm_page_state::SLAB);
    }
    return page;
  }
  virtual void FreeSlab(vm_page_t* slab) { Pmm::Node().FreePage(slab); }
  virtual vm_page_t* PaddrToPage(paddr_t paddr) const { return Pmm::Node().PaddrToPage(paddr); }

 private:
  bool AddSlab() {
    // Allocate a new slab.
    vm_page_t* slab = AllocSlab();
    if (!slab) {
      return false;
    }
    slab->slab.free_slot = kEndOfList;
    slab->slab.peak_allocated = 0;
    slab->slab.allocated = 0;
    // Insert it into the available slabs.
    list_add_head(&available_slabs_, &slab->queue_node);
    allocated_slabs_++;
    return true;
  }
  Entry* Allocate() {
    // See if there are any slabs available.
    if (list_is_empty(&available_slabs_)) {
      if (!AddSlab()) {
        return nullptr;
      }
    }
    vm_page_t* page = list_peek_head_type(&available_slabs_, vm_page_t, queue_node);
    Entry* entry;
    if (page->slab.free_slot == kEndOfList) {
      DEBUG_ASSERT(page->slab.peak_allocated < kObjectsPerSlab);
      entry = GetEntry(page, page->slab.peak_allocated);
      page->slab.peak_allocated++;
    } else {
      entry = GetEntry(page, page->slab.free_slot);
      page->slab.free_slot = entry->next;
    }
    page->slab.allocated++;
    if (page->slab.free_slot == kEndOfList && page->slab.peak_allocated == kObjectsPerSlab) {
      DEBUG_ASSERT(page->slab.allocated == kObjectsPerSlab);
      list_delete(&page->queue_node);
      list_add_head(&full_slabs_, &page->queue_node);
    } else {
      DEBUG_ASSERT(page->slab.allocated < kObjectsPerSlab);
    }
    return entry;
  }
  void Free(void* object) {
    // Lookup the slab this object was allocated in.
    auto [slab, index] = ObjectToSlab(object);

    // This will only catch the most egregious kinds of double-frees, but is better than nothing.
    DEBUG_ASSERT(slab->slab.free_slot != index);

    DEBUG_ASSERT(slab->slab.allocated > 0);
    if (slab->slab.allocated == 1) {
      // Slab has become empty, can free it.
      list_delete(&slab->queue_node);
      allocated_slabs_--;
      FreeSlab(slab);
      return;
    }

    if (slab->slab.allocated == kObjectsPerSlab) {
      // Slab is going from full to having space available, move to the correct list. We place at
      // the back of the list to encourage allocations, which happen on the head, to fill up a page
      // instead of constantly bouncing allocations into different, partially full, pages.
      list_delete(&slab->queue_node);
      list_add_tail(&available_slabs_, &slab->queue_node);
    }
    slab->slab.allocated--;

    // Update the free list for this slab.
    Entry* entry = GetEntry(slab, index);
    entry->next = slab->slab.free_slot;
    slab->slab.free_slot = index;
  }

  list_node_t full_slabs_ = LIST_INITIAL_VALUE(full_slabs_);
  list_node_t available_slabs_ = LIST_INITIAL_VALUE(available_slabs_);

  // Track the total number of allocated slabs (i.e. pages), both full and available.
  size_t allocated_slabs_ = 0;
};

// Common base for a specialized PageSlabAllocator that can assign a numerical ID to each allocated
// object, in lieu of the pointer, and convert between. This base implements the conversion
// functions under the assumption that the derived type is assigning each allocated slab a unique
// ID, such that id*kObjectsPerSlab fits within a uint32_t.
//
// The size of the ID is fixed at 32-bits, and not variable, since a 16-bit limit is unlikely to be
// useful in practice, and a 64-bit limit has no value since the pointer is already a 64-bit ID.
template <typename T>
class BaseIdSlabAllocator : public PageSlabAllocator<T> {
 public:
  static constexpr uint32_t kObjectsPerSlab = PageSlabAllocator<T>::kObjectsPerSlab;
  BaseIdSlabAllocator() = default;
  ~BaseIdSlabAllocator() = default;

  uint32_t ObjectToId(const T* object) const {
    auto [slab, slab_index] = PageSlabAllocator<T>::ObjectToSlab(object);
    uint32_t id = (slab->slab.id * kObjectsPerSlab) + slab_index;
    return id;
  }
  T* IdToObject(uint32_t id) const {
    uint32_t slab_id = id / kObjectsPerSlab;
    uint32_t slab_index = id % kObjectsPerSlab;
    vm_page_t* slab = IdToSlab(slab_id);
    return &PageSlabAllocator<T>::GetEntry(slab, 0)->object + slab_index;
  }

 private:
  virtual vm_page_t* IdToSlab(uint32_t slab_id) const = 0;
};

// An implementation of the BaseIdSlabAllocator that supports a fixed amount of slabs, as determined
// by MaxObjects, and statically pre-allocates all the metadata for those slabs. The metadata is 8
// bytes per slab.
//
// This allocator guarantees that all IDs are within [0, MaxObjects). To achieve this it is a
// restriction that the maximum number of requested objects be a multiple of the number of objects
// per slab.
//
// Due to the static overhead per potential slab required this object can become quite large as
// MaxObjects grows. Users should check the resulting size of the object, based on the provided
// allocation type T and MaxObjects, to ensure it is within acceptable limits.
//
// Most likely this allocator is not what you are looking for, and you want the IdSlabAllocator.
template <typename T, size_t MaxObjects>
class FixedIdSlabAllocator final : public BaseIdSlabAllocator<T> {
 public:
  static_assert(MaxObjects % BaseIdSlabAllocator<T>::kObjectsPerSlab == 0);
  FixedIdSlabAllocator() = default;
  ~FixedIdSlabAllocator() {
    ASSERT(ktl::all_of(slabs_.begin(), slabs_.end(), [](vm_page_t* p) { return p == nullptr; }));
  }

 private:
  vm_page_t* AllocSlab() override {
    // Find an unused slot/id.
    auto it = ktl::find(slabs_.begin(), slabs_.end(), nullptr);
    if (it == slabs_.end()) {
      return nullptr;
    }
    vm_page_t* slab = BaseIdSlabAllocator<T>::AllocSlab();
    if (!slab) {
      return nullptr;
    }
    const uint32_t id = static_cast<uint32_t>(ktl::distance(slabs_.begin(), it));
    slab->slab.id = id;
    *it = slab;
    return slab;
  }
  void FreeSlab(vm_page_t* slab) override {
    DEBUG_ASSERT(slab->state() == vm_page_state::SLAB);
    DEBUG_ASSERT(slabs_.at(slab->slab.id) == slab);
    slabs_.at(slab->slab.id) = nullptr;
    BaseIdSlabAllocator<T>::FreeSlab(slab);
  }
  vm_page_t* IdToSlab(uint32_t slab_id) const override {
    vm_page_t* slab = slabs_.at(slab_id);
    DEBUG_ASSERT(slab);
    return slab;
  }
  ktl::array<vm_page_t*, BaseIdSlabAllocator<T>::SlabsRequired(MaxObjects)> slabs_ = {nullptr};
};

// Slab allocator that can convert each allocation to/from a numerical ID, with the added guarantee
// that all IDs fall within [0, MaxObjects). This implies that only up to MaxObjects can be
// allocated at one time. To achieve this it is a restriction that the maximum number of requested
// objects be a common multiple of the number of objects per slab and per ID slab.
//
// The properties of this allocator are otherwise the same as the PageSlabAllocator, with the only
// dependency being direct PMM allocations, and slabs able to be cleaned up if they become fully
// free.
//
// This slab allocator uses the FixedIdSlabAllocator internally to allocate slab IDs. This works as
// follows:
// 1. Whenever this slab allocator allocates a slab, the `vm_page_t*` pointing to that slab is
//    stored in the FixedIdSlabAllocator.
// 2. By construction, each slab in the FixedIdSlabAllocator has an ID that we can convert into a
//    slab. Thus, using the ObjectToId function allows us to convert the `vm_page_t**` into a stable
//    ID that we can assign as the slab's ID in this allocator.
// We do this to minimize the amount of metadata needed to store the IDs. Notice that using a
// FixedIdSlabAllocator directly would increase the object size by 8 bytes for every slab we
// allocate. By introducing this level of indirection, we are able to add a slab to the
// FixedIdSlabAllocator only when we have (8 / PAGE_SIZE) slabs allocated in this allocator. We can
// fit (sizeof(T) / PAGE_SIZE) objects in each slab in this allocator, so this means that the size
// of the underlying FixedIdSlabAllocator will grow at a rate of
// (8 * sizeof(T)) / (PAGE_SIZE * PAGE_SIZE) bytes per object added.
//
// The rate of growth of the static metadata can be further reduced by adding additional levels of
// indirection for the slab id allocation by overriding the SLAB template argument.
template <typename T, size_t MaxObjects,
          typename SLAB =
              FixedIdSlabAllocator<vm_page_t*, BaseIdSlabAllocator<T>::SlabsRequired(MaxObjects)>>
class IdSlabAllocator final : public BaseIdSlabAllocator<T> {
 public:
  static_assert(MaxObjects % BaseIdSlabAllocator<T>::kObjectsPerSlab == 0);
  IdSlabAllocator() = default;
  ~IdSlabAllocator() = default;

  // Total memory usage, in bytes, including any unused portions of slabs, not including the size of
  // |this|.
  size_t MemoryUsage() const {
    return (slab_id_allocator_.AllocatedSlabs() + BaseIdSlabAllocator<T>::AllocatedSlabs()) *
           PAGE_SIZE;
  }

 private:
  vm_page_t* AllocSlab() override {
    // Allocate a new slot to store the slab from the id allocator.
    vm_page_t** slab_ref = slab_id_allocator_.New();
    if (!slab_ref) {
      return nullptr;
    }
    vm_page_t* slab = BaseIdSlabAllocator<T>::AllocSlab();
    if (!slab) {
      slab_id_allocator_.Delete(slab_ref);
      return nullptr;
    }
    *slab_ref = slab;
    // The object id of our slot is our slab id, allowing us to go from id->vm_page_t* later.
    const uint32_t slab_id = slab_id_allocator_.ObjectToId(slab_ref);
    slab->slab.id = slab_id;
    return slab;
  }
  void FreeSlab(vm_page_t* slab) override {
    // Return the slot used to store slab to the slab id allocator.
    vm_page_t** slab_ref = slab_id_allocator_.IdToObject(slab->slab.id);
    DEBUG_ASSERT(*slab_ref == slab);
    *slab_ref = nullptr;
    slab_id_allocator_.Delete(slab_ref);
    BaseIdSlabAllocator<T>::FreeSlab(slab);
  }
  vm_page_t* IdToSlab(uint32_t slab_id) const override {
    return *slab_id_allocator_.IdToObject(slab_id);
  }

  // The slab id allocator both allocates our unique slab ids, and provides the storage / mapping
  // from slab_id->vm_page_t*. Importantly it guarantees that its IDs are from [0..kMaxSlabs), which
  // we need to then guarantee that our final IDs are from [0..MaxObjects).
  SLAB slab_id_allocator_;
};

// An example of overriding the SLAB argument to provide an additional level of indirection for the
// ID allocation, limiting the growth of metadata by another multiple of the PAGE_SIZE.
template <typename T, size_t MaxObjects>
using TwoLevelIdSlabAllocator =
    IdSlabAllocator<T, MaxObjects,
                    IdSlabAllocator<vm_page_t*, BaseIdSlabAllocator<T>::SlabsRequired(MaxObjects)>>;

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_PAGE_SLAB_ALLOCATOR_H_
