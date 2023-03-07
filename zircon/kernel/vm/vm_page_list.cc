// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "vm/vm_page_list.h"

#include <align.h>
#include <inttypes.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>
#include <ktl/move.h>
#include <vm/compression.h>
#include <vm/pmm.h>
#include <vm/vm.h>
#include <vm/vm_object_paged.h>

#include "vm_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE VM_GLOBAL_TRACE(0)

namespace {

inline uint64_t offset_to_node_offset(uint64_t offset, uint64_t skew) {
  return ROUNDDOWN(offset + skew, PAGE_SIZE * VmPageListNode::kPageFanOut);
}

inline uint64_t offset_to_node_index(uint64_t offset, uint64_t skew) {
  return ((offset + skew) >> PAGE_SIZE_SHIFT) % VmPageListNode::kPageFanOut;
}

inline void move_vm_page_list_node(VmPageListNode* dest, VmPageListNode* src) {
  // Called by move ctor/assignment. Move assignment clears the dest node first.
  ASSERT(dest->IsEmpty());

  for (unsigned i = 0; i < VmPageListNode::kPageFanOut; i++) {
    dest->Lookup(i) = ktl::move(src->Lookup(i));
  }
}

}  // namespace

VmPageListNode::VmPageListNode(uint64_t offset) : obj_offset_(offset) {
  LTRACEF("%p offset %#" PRIx64 "\n", this, obj_offset_);
}

VmPageListNode::~VmPageListNode() {
  LTRACEF("%p offset %#" PRIx64 "\n", this, obj_offset_);
  canary_.Assert();

  DEBUG_ASSERT(HasNoPageOrRef());
}

VmPageList::VmPageList() { LTRACEF("%p\n", this); }

VmPageList::VmPageList(VmPageList&& other) : list_(ktl::move(other.list_)) {
  LTRACEF("%p\n", this);
  list_skew_ = other.list_skew_;
}

VmPageList::~VmPageList() {
  LTRACEF("%p\n", this);
  DEBUG_ASSERT(HasNoPageOrRef());
}

VmPageList& VmPageList::operator=(VmPageList&& other) {
  list_ = ktl::move(other.list_);
  list_skew_ = other.list_skew_;
  return *this;
}

VmPageOrMarker* VmPageList::LookupOrAllocate(uint64_t offset) {
  uint64_t node_offset = offset_to_node_offset(offset, list_skew_);
  size_t index = offset_to_node_index(offset, list_skew_);

  if (node_offset >= VmPageList::MAX_SIZE) {
    return nullptr;
  }

  LTRACEF_LEVEL(2, "%p offset %#" PRIx64 " node_offset %#" PRIx64 " index %zu\n", this, offset,
                node_offset, index);

  // lookup the tree node that holds this page
  auto pln = list_.find(node_offset);
  if (pln.IsValid()) {
    return &pln->Lookup(index);
  }

  fbl::AllocChecker ac;
  ktl::unique_ptr<VmPageListNode> pl =
      ktl::unique_ptr<VmPageListNode>(new (&ac) VmPageListNode(node_offset));
  if (!ac.check()) {
    return nullptr;
  }

  LTRACEF("allocating new inner node %p\n", pl.get());

  VmPageOrMarker& p = pl->Lookup(index);

  list_.insert(ktl::move(pl));
  return &p;
}

void VmPageList::ReturnEmptySlot(uint64_t offset) {
  uint64_t node_offset = offset_to_node_offset(offset, list_skew_);
  size_t index = offset_to_node_index(offset, list_skew_);

  LTRACEF_LEVEL(2, "%p offset %#" PRIx64 " node_offset %#" PRIx64 " index %zu\n", this, offset,
                node_offset, index);

  // lookup the tree node that holds this offset
  auto pln = list_.find(node_offset);
  DEBUG_ASSERT(pln.IsValid());

  // check that the slot was empty
  [[maybe_unused]] VmPageOrMarker page = ktl::move(pln->Lookup(index));
  DEBUG_ASSERT(page.IsEmpty());
  if (pln->IsEmpty()) {
    // node is empty, erase it.
    list_.erase(*pln);
  }
}

const VmPageOrMarker* VmPageList::Lookup(uint64_t offset) const {
  uint64_t node_offset = offset_to_node_offset(offset, list_skew_);
  size_t index = offset_to_node_index(offset, list_skew_);

  LTRACEF_LEVEL(2, "%p offset %#" PRIx64 " node_offset %#" PRIx64 " index %zu\n", this, offset,
                node_offset, index);

  // lookup the tree node that holds this page
  auto pln = list_.find(node_offset);
  if (!pln.IsValid()) {
    return nullptr;
  }

  return &pln->Lookup(index);
}

VmPageOrMarkerRef VmPageList::LookupMutable(uint64_t offset) {
  uint64_t node_offset = offset_to_node_offset(offset, list_skew_);
  size_t index = offset_to_node_index(offset, list_skew_);

  LTRACEF_LEVEL(2, "%p offset %#" PRIx64 " node_offset %#" PRIx64 " index %zu\n", this, offset,
                node_offset, index);

  // lookup the tree node that holds this page
  auto pln = list_.find(node_offset);
  if (!pln.IsValid()) {
    return VmPageOrMarkerRef(nullptr);
  }

  return VmPageOrMarkerRef(&pln->Lookup(index));
}

VmPageOrMarker VmPageList::RemoveContent(uint64_t offset) {
  uint64_t node_offset = offset_to_node_offset(offset, list_skew_);
  size_t index = offset_to_node_index(offset, list_skew_);

  LTRACEF_LEVEL(2, "%p offset %#" PRIx64 " node_offset %#" PRIx64 " index %zu\n", this, offset,
                node_offset, index);

  // lookup the tree node that holds this page
  auto pln = list_.find(node_offset);
  if (!pln.IsValid()) {
    return VmPageOrMarker::Empty();
  }

  // free this page
  VmPageOrMarker page = ktl::move(pln->Lookup(index));
  if (!page.IsEmpty() && pln->IsEmpty()) {
    // if it was the last item in the node, remove the node from the tree
    LTRACEF_LEVEL(2, "%p freeing the list node\n", this);
    list_.erase(*pln);
  }
  return page;
}

bool VmPageList::IsEmpty() const { return list_.is_empty(); }

bool VmPageList::HasNoPageOrRef() const {
  bool no_pages = true;
  ForEveryPage([&no_pages](auto* p, uint64_t) {
    if (p->IsPageOrRef()) {
      no_pages = false;
      return ZX_ERR_STOP;
    }
    return ZX_ERR_NEXT;
  });
  return no_pages;
}

bool VmPageList::IsOffsetInInterval(uint64_t offset) const {
  // Find the node that would contain this offset.
  const uint64_t node_offset = offset_to_node_offset(offset, list_skew_);
  // If the node containing offset is populated, the lower bound will return that node. If not
  // populated, it will return the next populated node.
  auto pln = list_.lower_bound(node_offset);

  // Could not find a valid node >= node_offset. So offset cannot be part of an interval, an
  // interval would have an end slot.
  if (!pln.IsValid()) {
    return false;
  }
  // The page list shouldn't have any empty nodes.
  DEBUG_ASSERT(!pln->IsEmpty());
  return IfOffsetInIntervalHelper(offset, *pln);
}

bool VmPageList::IfOffsetInIntervalHelper(uint64_t offset, const VmPageListNode& lower_bound,
                                          const VmPageOrMarker** interval_out) const {
  DEBUG_ASSERT(!lower_bound.IsEmpty());
  if (interval_out) {
    *interval_out = nullptr;
  }

  // Helper to return success if an interval sentinel is found.
  auto found_interval = [&interval_out](const VmPageOrMarker* interval) {
    if (interval_out) {
      *interval_out = interval;
    }
    return true;
  };

  const uint64_t node_offset = offset_to_node_offset(offset, list_skew_);
  const size_t node_index = offset_to_node_index(offset, list_skew_);

  // For the offset to lie in an interval, it is going to have an interval end so we should have
  // found some valid node if the offset falls in an interval. See if offset falls in an interval.

  // We found the node containing offset.
  if (lower_bound.offset() == node_offset) {
    auto& slot = lower_bound.Lookup(node_index);
    // Check if the slot is an interval sentinel.
    if (slot.IsInterval()) {
      return found_interval(&slot);
    }
    if (!slot.IsEmpty()) {
      // For any other non-empty slot, we know this is not an interval.
      return false;
    }
    // The slot is empty. Walk to the left and right and lookup the closest populated
    // slots. We should find either an interval start to the left or an end to the right. Try to
    // find a start to the left first.
    DEBUG_ASSERT(slot.IsEmpty());
    for (size_t i = node_index; i > 0; i--) {
      auto& p = lower_bound.Lookup(i - 1);
      // Found the first populated slot to the left.
      if (!p.IsEmpty()) {
        if (p.IsIntervalStart()) {
          return found_interval(&p);
        }
        // Found a non-empty slot to the left that wasn't a start. We cannot be in an interval.
        return false;
      }
    }
  }

  // We could end up here under two conditions.
  //  1. The lower_bound node contained offset, offset was empty, and we could not find a non-empty
  //  slot to the left. So we have to walk right to try to find an interval end.
  //  2. The lower_bound node contained offsets larger than offset. For this case too we have to
  //  walk right and see if the first populated slot is an interval end.
  // Only the start index for the traversal is different for the two cases.
  size_t index;
  if (lower_bound.offset() == node_offset) {
    index = node_index + 1;
  } else {
    DEBUG_ASSERT(lower_bound.offset() > node_offset);
    index = 0;
  }

  for (; index < VmPageListNode::kPageFanOut; index++) {
    auto& p = lower_bound.Lookup(index);
    // Found the first populated slot to the right.
    if (!p.IsEmpty()) {
      if (p.IsIntervalEnd()) {
        return found_interval(&p);
      }
      // Found a non-empty slot to the right that wasn't an end. We cannot be in an interval.
      return false;
    }
  }

  return false;
}

zx_status_t VmPageList::AddZeroInterval(uint64_t start_offset, uint64_t end_offset,
                                        VmPageOrMarker::IntervalDirtyState dirty_state) {
  DEBUG_ASSERT(IS_PAGE_ALIGNED(start_offset));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(end_offset));
  DEBUG_ASSERT(start_offset < end_offset);
  DEBUG_ASSERT(!AnyPagesOrIntervalsInRange(start_offset, end_offset));

  const uint64_t interval_start = start_offset;
  const uint64_t interval_end = end_offset - PAGE_SIZE;
  const uint64_t prev_offset = interval_start - PAGE_SIZE;
  const uint64_t next_offset = interval_end + PAGE_SIZE;

  // Helper to look up a slot at an offset and return a mutable VmPageOrMarker*. Only finds an
  // existing slot and does not perform any allocations.
  auto lookup_slot = [this](uint64_t offset) -> VmPageOrMarker* {
    const uint64_t node_offset = offset_to_node_offset(offset, list_skew_);
    const size_t index = offset_to_node_index(offset, list_skew_);
    auto pln = list_.find(node_offset);
    if (!pln.IsValid()) {
      return nullptr;
    }
    return &pln->Lookup(index);
  };

  // Check if we can merge this zero interval with a preceding one.
  bool merge_with_prev = false;
  VmPageOrMarker* prev_slot = nullptr;
  if (interval_start > 0) {
    prev_slot = lookup_slot(prev_offset);
    // We can merge to the left if we find a zero interval end or slot, and the dirty state matches.
    if (prev_slot && prev_slot->IsIntervalZero() &&
        (prev_slot->IsIntervalEnd() || prev_slot->IsIntervalSlot()) &&
        prev_slot->GetZeroIntervalDirtyState() == dirty_state) {
      merge_with_prev = true;
    }
  }

  // Check if we can merge this zero interval with a following one.
  bool merge_with_next = false;
  VmPageOrMarker* next_slot = nullptr;
  next_slot = lookup_slot(next_offset);
  // We can merge to the right if we find a zero interval start or slot, and the dirty state
  // matches.
  if (next_slot && next_slot->IsIntervalZero() &&
      (next_slot->IsIntervalStart() || next_slot->IsIntervalSlot()) &&
      next_slot->GetZeroIntervalDirtyState() == dirty_state) {
    merge_with_next = true;
  }

  // First allocate any slots that might be needed to insert the interval.
  VmPageOrMarker* new_start = nullptr;
  VmPageOrMarker* new_end = nullptr;
  // If we could not merge with an interval to the left, we're going to need a new start sentinel.
  if (!merge_with_prev) {
    new_start = LookupOrAllocate(interval_start);
    if (!new_start) {
      return ZX_ERR_NO_MEMORY;
    }
    DEBUG_ASSERT(new_start->IsEmpty());
  }
  // If we could not merge with an interval to the right, we're going to need a new end sentinel.
  if (!merge_with_next) {
    new_end = LookupOrAllocate(interval_end);
    if (!new_end) {
      // Clean up any slot we allocated for new_start before returning.
      if (new_start) {
        DEBUG_ASSERT(new_start->IsEmpty());
        ReturnEmptySlot(interval_start);
      }
      return ZX_ERR_NO_MEMORY;
    }
    DEBUG_ASSERT(new_end->IsEmpty());
  }

  // Now that we've checked for all error conditions perform the actual update.
  if (merge_with_prev) {
    DEBUG_ASSERT(prev_slot);
    if (prev_slot->IsIntervalEnd()) {
      // If the prev_slot was an interval end, we can simply extend that interval to include the new
      // interval. Free up the old interval end.
      *prev_slot = VmPageOrMarker::Empty();
    } else {
      // If the prev_slot was interval slot, we can extend the interval in that case too. Change the
      // old interval slot into an interval start.
      DEBUG_ASSERT(prev_slot->IsIntervalSlot());
      DEBUG_ASSERT(prev_slot->GetZeroIntervalDirtyState() == dirty_state);
      prev_slot->ChangeIntervalSentinel(VmPageOrMarker::IntervalSentinel::Start);
    }
  } else {
    // We could not merge with an interval to the left. Start a new interval.
    DEBUG_ASSERT(new_start);
    DEBUG_ASSERT(new_start->IsEmpty());
    *new_start = VmPageOrMarker::ZeroInterval(VmPageOrMarker::IntervalSentinel::Start, dirty_state);
  }

  if (merge_with_next) {
    DEBUG_ASSERT(next_slot);
    if (next_slot->IsIntervalStart()) {
      // If the next_slot was an interval start, we can move back the start to include the new
      // interval. Free up the old start.
      *next_slot = VmPageOrMarker::Empty();
    } else {
      // If the next_slot was an interval slot, we can move back the start in that case too. Change
      // the old interval slot into an interval end.
      DEBUG_ASSERT(next_slot->IsIntervalSlot());
      DEBUG_ASSERT(next_slot->GetZeroIntervalDirtyState() == dirty_state);
      next_slot->ChangeIntervalSentinel(VmPageOrMarker::IntervalSentinel::End);
    }
  } else {
    // We could not merge with an interval to the right. Install an interval end sentinel.
    DEBUG_ASSERT(new_end);
    // If the new zero interval spans a single page, we will already have installed a start above,
    // so change it to a slot sentinel.
    if (new_end->IsIntervalStart()) {
      DEBUG_ASSERT(new_end->GetZeroIntervalDirtyState() == dirty_state);
      new_end->ChangeIntervalSentinel(VmPageOrMarker::IntervalSentinel::Slot);
    } else {
      DEBUG_ASSERT(new_end->IsEmpty());
      *new_end = VmPageOrMarker::ZeroInterval(VmPageOrMarker::IntervalSentinel::End, dirty_state);
    }
  }

  // If we ended up removing the prev_slot or next_slot, return the now empty slots.
  bool returned_prev_slot = false;
  if (merge_with_prev && prev_slot->IsEmpty()) {
    ReturnEmptySlot(prev_offset);
    returned_prev_slot = true;
  }
  if (merge_with_next && next_slot->IsEmpty()) {
    // next_slot and prev_slot could have come from the same node, in which case we've already
    // freed up the node when returning prev_slot.
    if (!returned_prev_slot || offset_to_node_offset(prev_offset, list_skew_) !=
                                   offset_to_node_offset(next_offset, list_skew_)) {
      ReturnEmptySlot(next_offset);
    }
  }

  return ZX_OK;
}

void VmPageList::MergeFrom(
    VmPageList& other, const uint64_t offset, const uint64_t end_offset,
    fit::inline_function<void(VmPageOrMarker&&, uint64_t offset), 3 * sizeof(void*)> release_fn,
    fit::inline_function<void(VmPageOrMarker*, uint64_t offset)> migrate_fn) {
  constexpr uint64_t kNodeSize = PAGE_SIZE * VmPageListNode::kPageFanOut;
  // The skewed |offset| in |other| must be equal to 0 skewed in |this|. This allows
  // nodes to moved directly between the lists, without having to worry about allocations.
  DEBUG_ASSERT((other.list_skew_ + offset) % kNodeSize == list_skew_);

  auto release_fn_wrapper = [&release_fn](VmPageOrMarker* page_or_marker, uint64_t offset) {
    if (!page_or_marker->IsEmpty()) {
      release_fn(ktl::move(*page_or_marker), offset);
    }
    return ZX_ERR_NEXT;
  };

  // Free pages outside of [|offset|, |end_offset|) so that the later code
  // doesn't have to worry about dealing with this.
  if (offset) {
    other.RemovePages(release_fn_wrapper, 0, offset);
  }
  other.RemovePages(release_fn_wrapper, end_offset, MAX_SIZE);

  // Calculate how much we need to shift nodes so that the node in |other| which contains
  // |offset| gets mapped to offset 0 in |this|.
  const uint64_t node_shift = offset_to_node_offset(offset, other.list_skew_);

  auto other_iter = other.list_.lower_bound(node_shift);
  while (other_iter.IsValid()) {
    uint64_t other_offset = other_iter->GetKey();
    // Any such nodes should have already been freed.
    DEBUG_ASSERT(other_offset < (end_offset + other.list_skew_));

    auto cur = other_iter++;
    auto other_node = other.list_.erase(cur);
    other_node->set_offset(other_offset - node_shift);

    auto target = list_.find(other_offset - node_shift);
    if (target.IsValid()) {
      // If there's already a node at the desired location, then merge the two nodes.
      for (unsigned i = 0; i < VmPageListNode::kPageFanOut; i++) {
        uint64_t src_offset = other_offset - other.list_skew_ + i * PAGE_SIZE;
        VmPageOrMarker page = ktl::move(other_node->Lookup(i));
        VmPageOrMarker& target_page = target->Lookup(i);
        if (target_page.IsEmpty()) {
          if (page.IsPageOrRef()) {
            migrate_fn(&page, src_offset);
          }
          target_page = ktl::move(page);
        } else if (!page.IsEmpty()) {
          release_fn(ktl::move(page), src_offset);
        }

        // In all cases if we still have a page add it to the free list.
        DEBUG_ASSERT(!page.IsPageOrRef());
      }
    } else {
      // If there's no node at the desired location, then directly insert the node.
      list_.insert_or_find(ktl::move(other_node), &target);
      bool has_page = false;
      for (unsigned i = 0; i < VmPageListNode::kPageFanOut; i++) {
        VmPageOrMarker& page = target->Lookup(i);
        if (page.IsPageOrRef()) {
          migrate_fn(&page, other_offset - other.list_skew_ + i * PAGE_SIZE);
          if (page.IsPageOrRef()) {
            has_page = true;
          }
        } else if (page.IsMarker()) {
          has_page = true;
        }
      }
      if (!has_page) {
        list_.erase(target);
      }
    }
  }
}

void VmPageList::MergeOnto(VmPageList& other,
                           fit::inline_function<void(VmPageOrMarker&&)> release_fn) {
  DEBUG_ASSERT(other.list_skew_ == list_skew_);

  auto iter = list_.begin();
  while (iter.IsValid()) {
    auto node = list_.erase(iter++);
    auto target = other.list_.find(node->GetKey());
    if (target.IsValid()) {
      // If there's already a node at the desired location, then merge the two nodes.
      for (unsigned i = 0; i < VmPageListNode::kPageFanOut; i++) {
        VmPageOrMarker page = ktl::move(node->Lookup(i));
        if (page.IsEmpty()) {
          continue;
        }
        VmPageOrMarker& old_page = target->Lookup(i);
        VmPageOrMarker removed = ktl::move(old_page);
        old_page = ktl::move(page);
        if (!removed.IsEmpty()) {
          release_fn(ktl::move(removed));
        }
      }
    } else {
      other.list_.insert(ktl::move(node));
    }
  }
}

VmPageSpliceList VmPageList::TakePages(uint64_t offset, uint64_t length) {
  VmPageSpliceList res(offset, length);
  const uint64_t end = offset + length;

  // Taking pages from children isn't supported, so list_skew_ should be 0.
  DEBUG_ASSERT(list_skew_ == 0);

  // If we can't take the whole node at the start of the range,
  // the shove the pages into the splice list head_ node.
  while (offset_to_node_index(offset, 0) != 0 && offset < end) {
    res.head_.Lookup(offset_to_node_index(offset, 0)) = RemoveContent(offset);
    offset += PAGE_SIZE;
  }

  // As long as the current and end node offsets are different, we
  // can just move the whole node into the splice list.
  while (offset_to_node_offset(offset, 0) != offset_to_node_offset(end, 0)) {
    ktl::unique_ptr<VmPageListNode> node = list_.erase(offset_to_node_offset(offset, 0));
    if (node) {
      res.middle_.insert(ktl::move(node));
    }
    offset += (PAGE_SIZE * VmPageListNode::kPageFanOut);
  }

  // Move any remaining pages into the splice list tail_ node.
  while (offset < end) {
    res.tail_.Lookup(offset_to_node_index(offset, 0)) = RemoveContent(offset);
    offset += PAGE_SIZE;
  }

  return res;
}

VmPageSpliceList::VmPageSpliceList() : VmPageSpliceList(0, 0) {}

VmPageSpliceList::VmPageSpliceList(uint64_t offset, uint64_t length)
    : offset_(offset), length_(length) {}

VmPageSpliceList::VmPageSpliceList(VmPageSpliceList&& other)
    : offset_(other.offset_),
      length_(other.length_),
      pos_(other.pos_),
      middle_(ktl::move(other.middle_)) {
  move_vm_page_list_node(&head_, &other.head_);
  move_vm_page_list_node(&tail_, &other.tail_);

  other.offset_ = other.length_ = other.pos_ = 0;
}

VmPageSpliceList& VmPageSpliceList::operator=(VmPageSpliceList&& other) {
  FreeAllPages();

  offset_ = other.offset_;
  length_ = other.length_;
  pos_ = other.pos_;
  move_vm_page_list_node(&head_, &other.head_);
  move_vm_page_list_node(&tail_, &other.tail_);
  middle_ = ktl::move(other.middle_);

  other.offset_ = other.length_ = other.pos_ = 0;

  return *this;
}

VmPageSpliceList::~VmPageSpliceList() { FreeAllPages(); }

// static
VmPageSpliceList VmPageSpliceList::CreateFromPageList(uint64_t offset, uint64_t length,
                                                      list_node* pages) {
  // TODO(fxbug.dev/88859): This method needs coverage in vmpl_unittests.
  DEBUG_ASSERT(pages);
  DEBUG_ASSERT(list_length(pages) == length / PAGE_SIZE);
  VmPageSpliceList res(offset, length);
  DEBUG_ASSERT(list_is_empty(&res.raw_pages_));
  list_move(pages, &res.raw_pages_);
  return res;
}

void VmPageSpliceList::FreeAllPages() {
  // Free any pages owned by the splice list.
  while (!IsDone()) {
    VmPageOrMarker page = Pop();
    if (page.IsPage()) {
      pmm_free_page(page.ReleasePage());
    } else if (page.IsReference()) {
      auto compression = pmm_page_compression();
      DEBUG_ASSERT(compression);
      compression->Free(page.ReleaseReference());
    }
  }
}

VmPageOrMarker VmPageSpliceList::Pop() {
  if (IsDone()) {
    DEBUG_ASSERT_MSG(false, "Popped from empty splice list");
    return VmPageOrMarker::Empty();
  }

  VmPageOrMarker res;
  if (!list_is_empty(&raw_pages_)) {
    // TODO(fxbug.dev/88859): This path and CreateFromPageList() need coverage in vmpl_unittests.
    vm_page_t* head = list_remove_head_type(&raw_pages_, vm_page, queue_node);
    res = VmPageOrMarker::Page(head);
  } else {
    const uint64_t cur_offset = offset_ + pos_;
    const auto cur_node_idx = offset_to_node_index(cur_offset, 0);
    const auto cur_node_offset = offset_to_node_offset(cur_offset, 0);

    if (offset_to_node_index(offset_, 0) != 0 &&
        offset_to_node_offset(offset_, 0) == cur_node_offset) {
      // If the original offset means that pages were placed in head_
      // and the current offset points to the same node, look there.
      res = ktl::move(head_.Lookup(cur_node_idx));
    } else if (cur_node_offset != offset_to_node_offset(offset_ + length_, 0)) {
      // If the current offset isn't pointing to the tail node,
      // look in the middle tree.
      auto middle_node = middle_.find(cur_node_offset);
      if (middle_node.IsValid()) {
        res = ktl::move(middle_node->Lookup(cur_node_idx));
      }
    } else {
      // If none of the other cases, we're in the tail_.
      res = ktl::move(tail_.Lookup(cur_node_idx));
    }
  }

  pos_ += PAGE_SIZE;
  return res;
}
