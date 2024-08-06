// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_MAPPING_CURSOR_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_MAPPING_CURSOR_H_

#include <assert.h>
#include <sys/types.h>

// Helper class for MMU implementations to track virtual and physical address ranges when installing
// and removing mappings.
class MappingCursor {
 public:
  MappingCursor() = default;
  MappingCursor(vaddr_t vaddr, size_t size) : start_vaddr_(vaddr), vaddr_(vaddr), size_(size) {}
  MappingCursor(const paddr_t* paddrs, size_t paddr_count, size_t page_size, vaddr_t vaddr)
      : paddrs_(paddrs),
        page_size_(page_size),
        start_vaddr_(vaddr),
        vaddr_(vaddr),
        size_(page_size * paddr_count) {
#ifdef DEBUG_ASSERT_IMPLEMENTED
    paddr_count_ = paddr_count;
#endif
  }

  // Sets offset used for |vaddr_rel| and returns true if the cursor lies within that offset and
  // some specified maximum. Should only be called before the cursor has started to be used.
  bool SetVaddrRelativeOffset(vaddr_t vaddr_rel_offset, size_t vaddr_rel_max) {
    DEBUG_ASSERT(start_vaddr_ == vaddr_);
    vaddr_t vaddr_rel = start_vaddr_ - vaddr_rel_offset;

    if (vaddr_rel > vaddr_rel_max - size_ || size_ > vaddr_rel_max) {
      return false;
    }
    vaddr_rel_offset_ = vaddr_rel_offset;
    return true;
  }

  // Update the cursor to skip over a not-present page table entry.
  void SkipEntry(size_t ps) {
    // Cannot just increase by given size as the both the current or final vaddr may not be aligned
    // to this given size. These cases only happen as the very first or very last entry we will
    // examine respectively, but still must be handled here.

    // Calculate the amount the cursor should skip to get to the next entry at
    // this page table level.
    const size_t next_entry_offset = ps - (vaddr_ & (ps - 1));
    // If our endpoint was in the middle of this range, clamp the
    // amount we remove from the cursor
    const size_t consume = (size_ > next_entry_offset) ? next_entry_offset : size_;

    DEBUG_ASSERT(size_ >= consume);
    size_ -= consume;
    vaddr_ += consume;

    // Skipping entries has no meaning if we are attempting to create new mappings as we cannot
    // manipulate the paddr in a sensible way, so we expect this to not get called.
    DEBUG_ASSERT(!paddrs_);
    DEBUG_ASSERT(page_size_ == 0);
  }

  void ConsumePAddr(size_t ps) {
    DEBUG_ASSERT(paddrs_);
    paddr_consumed_ += ps;
    DEBUG_ASSERT(paddr_consumed_ <= page_size_);
    vaddr_ += ps;
    DEBUG_ASSERT(size_ >= ps);
    size_ -= ps;
    if (paddr_consumed_ == page_size_) {
      paddrs_++;
      paddr_consumed_ = 0;
#ifdef DEBUG_ASSERT_IMPLEMENTED
      DEBUG_ASSERT(paddr_count_ > 0);
      paddr_count_--;
#endif
    }
  }

  void ConsumeVAddr(size_t ps) {
    // If physical addresses are being tracked for creating mappings then ConsumePAddr should be
    // called, so validate there are no paddrs we are going to desync with.
    DEBUG_ASSERT(!paddrs_);
    DEBUG_ASSERT(page_size_ == 0);
    vaddr_ += ps;
    DEBUG_ASSERT(size_ >= ps);
    size_ -= ps;
  }

  paddr_t paddr() const {
    DEBUG_ASSERT(paddrs_);
    DEBUG_ASSERT(size_ > 0);
    return (*paddrs_) + paddr_consumed_;
  }

  size_t PageRemaining() const {
    DEBUG_ASSERT(paddrs_);
    return page_size_ - paddr_consumed_;
  }

  // Returns a new cursor to the, possibly empty, virtual range that has already been processed by
  // this cursor. The returned cursor will always be a subset of the original cursors range and
  // does not include the paddrs.
  MappingCursor ProcessedRange() const {
    MappingCursor ret(start_vaddr_, vaddr_ - start_vaddr_);
    // As our new cursor is a subrange we know the relative offset will always be valid.
    ret.vaddr_rel_offset_ = vaddr_rel_offset_;
    return ret;
  }

  vaddr_t vaddr() const { return vaddr_; }

  vaddr_t vaddr_rel() const { return vaddr_ - vaddr_rel_offset_; }

  size_t size() const { return size_; }

 private:
  // Physical address is optional and only applies when mapping in new pages, and not manipulating
  // existing mappings.
  const paddr_t* paddrs_ = nullptr;
#ifdef DEBUG_ASSERT_IMPLEMENTED
  // We have no need to actually track the total number of elements in the paddrs array, as this
  // should be a simple size/paddr_size. To guard against code mistakes though, we separately track
  // this just in debug mode.
  size_t paddr_count_ = 0;
#endif
  size_t paddr_consumed_ = 0;
  size_t page_size_ = 0;

  vaddr_t start_vaddr_;
  vaddr_t vaddr_;
  vaddr_t vaddr_rel_offset_ = 0;
  size_t size_;
};

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_MAPPING_CURSOR_H_
