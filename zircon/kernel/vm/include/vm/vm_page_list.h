// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_VM_PAGE_LIST_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_VM_PAGE_LIST_H_

#include <align.h>
#include <bits.h>
#include <lib/fit/function.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <fbl/canary.h>
#include <fbl/intrusive_wavl_tree.h>
#include <fbl/macros.h>
#include <ktl/algorithm.h>
#include <ktl/unique_ptr.h>
#include <vm/page.h>
#include <vm/pmm.h>
#include <vm/vm.h>

// RAII helper for representing content in a page list node. This supports being in one of five
// states
//  * Empty       - Contains nothing
//  * Page p      - Contains a vm_page 'p'. This 'p' is considered owned by this wrapper and
//                  `ReleasePage` must be called to give up ownership.
//  * Reference r - Contains a reference 'r' to some content. This 'r' is considered owned by this
//                  wrapper and `ReleaseReference` must be called to give up ownership.
//  * Marker      - Indicates that whilst not a page, it is also not empty. Markers can be used to
//                  separate the distinction between "there's no page because we've deduped to the
//                  zero page" and "there's no page because our parent contains the content".
//  * Interval    - Indicates that this page is part of a sparse page interval. An interval will
//                  have a Start sentinel, and an End sentinel, and all offsets that lie between the
//                  two will be empty. If the interval spans a single page, it will be represented
//                  as a Slot sentinel, which is conceptually the same as both a Start and an End
//                  sentinel.
class VmPageOrMarker {
 public:
  // A PageType that otherwise holds a null pointer is considered to be Empty.
  VmPageOrMarker() : raw_(kPageType) {}
  ~VmPageOrMarker() { DEBUG_ASSERT(!IsPageOrRef()); }
  VmPageOrMarker(VmPageOrMarker&& other) noexcept : raw_(other.Release()) {}
  VmPageOrMarker(const VmPageOrMarker&) = delete;
  VmPageOrMarker& operator=(const VmPageOrMarker&) = delete;

  // Minimal wrapper around a uint64_t to provide stronger typing in code to prevent accidental
  // mixing of references and other uint64_t values.
  // Provides a way to query the required alignment of the references and does debug enforcement of
  // this.
  class ReferenceValue {
   public:
    // kAlignBits represents the number of low bits in a reference that must be zero so they can be
    // used for internal metadata. This is declared here for convenience, and is asserted to be in
    // sync with the private kReferenceBits.
    static constexpr uint64_t kAlignBits = 4;
    explicit constexpr ReferenceValue(uint64_t raw) : value_(raw) {
      DEBUG_ASSERT((value_ & BIT_MASK(kAlignBits)) == 0);
    }
    uint64_t value() const { return value_; }

   private:
    uint64_t value_;
  };

  // Returns a reference to the underlying vm_page*. Is only valid to call if `IsPage` is true.
  vm_page* Page() const {
    DEBUG_ASSERT(IsPage());
    // Do not need to mask any bits out of raw_, since Page has 0's for the type anyway.
    static_assert(kPageType == 0);
    return reinterpret_cast<vm_page*>(raw_);
  }
  ReferenceValue Reference() const {
    DEBUG_ASSERT(IsReference());
    return ReferenceValue(raw_ & ~BIT_MASK(kReferenceBits));
  }

  // If this is a page, moves the underlying vm_page* out and returns it. After this IsPage will
  // be false and IsEmpty will be true.
  [[nodiscard]] vm_page* ReleasePage() {
    DEBUG_ASSERT(IsPage());
    // Do not need to mask any bits out of the Release since Page has 0's for the type
    // anyway.
    static_assert(kPageType == 0);
    return reinterpret_cast<vm_page*>(Release());
  }

  [[nodiscard]] ReferenceValue ReleaseReference() {
    DEBUG_ASSERT(IsReference());
    return ReferenceValue(Release() & ~BIT_MASK(kReferenceBits));
  }

  // Convenience wrappers for getting and setting split bits on both pages and references.
  bool PageOrRefLeftSplit() const {
    DEBUG_ASSERT(IsPageOrRef());
    if (IsPage()) {
      return Page()->object.cow_left_split;
    }
    return raw_ & kReferenceLeftSplit;
  }
  bool PageOrRefRightSplit() const {
    DEBUG_ASSERT(IsPageOrRef());
    if (IsPage()) {
      return Page()->object.cow_right_split;
    }
    return raw_ & kReferenceRightSplit;
  }
  void SetPageOrRefLeftSplit(bool value) {
    DEBUG_ASSERT(IsPageOrRef());
    if (IsPage()) {
      Page()->object.cow_left_split = value;
    } else {
      if (value) {
        raw_ |= kReferenceLeftSplit;
      } else {
        raw_ &= ~kReferenceLeftSplit;
      }
    }
  }
  void SetPageOrRefRightSplit(bool value) {
    DEBUG_ASSERT(IsPageOrRef());
    if (IsPage()) {
      Page()->object.cow_right_split = value;
    } else {
      if (value) {
        raw_ |= kReferenceRightSplit;
      } else {
        raw_ &= ~kReferenceRightSplit;
      }
    }
  }

  // Changes the content from a reference to a page, preserving the split bits and returning the
  // original reference.
  [[nodiscard]] VmPageOrMarker::ReferenceValue SwapReferenceForPage(vm_page_t* p) {
    DEBUG_ASSERT(p);
    // Ensure the caller has correctly set the split bits in the page as this swap is not supposed
    // to change any other information.
    DEBUG_ASSERT(p->object.cow_left_split == PageOrRefLeftSplit());
    DEBUG_ASSERT(p->object.cow_right_split == PageOrRefRightSplit());
    VmPageOrMarker::ReferenceValue ref = ReleaseReference();
    *this = VmPageOrMarker::Page(p);
    return ref;
  }
  // Changes the content from a page to a reference, preserving the split bits and returning the
  // original page.
  [[nodiscard]] vm_page_t* SwapPageForReference(VmPageOrMarker::ReferenceValue ref) {
    const bool left_split = PageOrRefLeftSplit();
    const bool right_split = PageOrRefRightSplit();
    vm_page_t* page = ReleasePage();
    *this = VmPageOrMarker::Reference(ref, left_split, right_split);
    return page;
  }
  // Changes the content from one reference to a different one, preserving the split bits an
  // returning the original reference.
  [[nodiscard]] VmPageOrMarker::ReferenceValue ChangeReferenceValue(
      VmPageOrMarker::ReferenceValue ref) {
    const bool left_split = PageOrRefLeftSplit();
    const bool right_split = PageOrRefRightSplit();
    const VmPageOrMarker::ReferenceValue old = ReleaseReference();
    *this = VmPageOrMarker::Reference(ref, left_split, right_split);
    return old;
  }

  bool IsPage() const { return !IsEmpty() && (GetType() == kPageType); }
  bool IsMarker() const { return GetType() == kZeroMarkerType; }
  bool IsEmpty() const {
    // A PageType that otherwise holds a null pointer is considered to be Empty.
    return raw_ == kPageType;
  }
  bool IsReference() const { return GetType() == kReferenceType; }
  bool IsPageOrRef() const { return IsPage() || IsReference(); }
  bool IsInterval() const { return GetType() == kIntervalType; }

  VmPageOrMarker& operator=(VmPageOrMarker&& other) noexcept {
    // Forbid overriding content, as that would leak it.
    DEBUG_ASSERT(!IsPageOrRef());
    raw_ = other.Release();
    return *this;
  }

  bool operator==(const VmPageOrMarker& other) const { return raw_ == other.raw_; }

  bool operator!=(const VmPageOrMarker& other) const { return raw_ != other.raw_; }

  // A PageType that otherwise holds a null pointer is considered to be Empty.
  static VmPageOrMarker Empty() { return VmPageOrMarker{kPageType}; }
  static VmPageOrMarker Marker() { return VmPageOrMarker{kZeroMarkerType}; }

  [[nodiscard]] static VmPageOrMarker Page(vm_page* p) {
    // A null page is incorrect for two reasons
    // 1. It's a violation of the API of this method
    // 2. A null page cannot be represented internally as this is used to represent Empty
    DEBUG_ASSERT(p);
    const uint64_t raw = reinterpret_cast<uint64_t>(p);
    // A pointer should be aligned by definition, and hence the low bits should always be zero, but
    // assert this anyway just in case kTypeBits is increased or someone passed an invalid pointer.
    DEBUG_ASSERT((raw & BIT_MASK(kTypeBits)) == 0);
    return VmPageOrMarker{raw | kPageType};
  }

  [[nodiscard]] static VmPageOrMarker Reference(ReferenceValue ref, bool left_split,
                                                bool right_split) {
    return VmPageOrMarker(ref.value() | (left_split ? kReferenceLeftSplit : 0) |
                          (right_split ? kReferenceRightSplit : 0) | kReferenceType);
  }

  // The types of sparse page interval types that are supported.
  enum class IntervalType : uint64_t {
    // Represents a range of zero pages.
    Zero = 0,
    NumTypes,
  };

  // Sentinel types that are used to represent a sparse page interval.
  enum class IntervalSentinel : uint64_t {
    // Represents a single page interval.
    Slot = 0,
    // The first page of a multi-page interval.
    Start,
    // The last page of a multi-page interval.
    End,
    NumSentinels,
  };

  // The remaining bits of an interval type store any information specific to the type of interval
  // being tracked. The ZeroRange class is defined here to group together the encoding of these bits
  // specific to IntervalType::Zero.
  class ZeroRange {
   public:
    // This is the same as kIntervalBits. Equality is asserted later where kIntervalBits is defined.
    static constexpr uint64_t kAlignBits = 6;
    explicit constexpr ZeroRange(uint64_t val) : value_(val) {
      DEBUG_ASSERT((value_ & BIT_MASK(kAlignBits)) == 0);
    }
    // The various dirty states that a zero interval can be in. Refer to VmCowPages::DirtyState for
    // an explanation of the states. Note that an AwaitingClean state is not encoded in the interval
    // state bits. This information is instead stored using the AwaitingCleanLength for convenience,
    // where a non-zero length indicates that the interval is AwaitingClean. Doing this affords
    // more convenient splitting and merging of intervals.
    enum class DirtyState : uint64_t {
      Untracked = 0,
      Clean,
      Dirty,
      NumStates,
    };
    ZeroRange(uint64_t val, DirtyState state) : value_(val) {
      DEBUG_ASSERT((value_ & BIT_MASK(kAlignBits)) == 0);
      DEBUG_ASSERT(GetDirtyState() == DirtyState::Untracked);
      SetDirtyState(state);
    }
    uint64_t value() const { return value_; }

    // For zero range tracking, we also need to track dirty state information, and if the interval
    // is AwaitingClean, the length that is AwaitingClean.
    static constexpr uint64_t kDirtyStateBits = VM_PAGE_OBJECT_DIRTY_STATE_BITS;
    static_assert(static_cast<uint64_t>(DirtyState::NumStates) <= (1 << kDirtyStateBits));
    static constexpr uint64_t kDirtyStateShift = kAlignBits;
    DirtyState GetDirtyState() const {
      return static_cast<DirtyState>((value_ & (BIT_MASK(kDirtyStateBits) << kDirtyStateShift)) >>
                                     kDirtyStateShift);
    }
    void SetDirtyState(DirtyState state) {
      // Only allow dirty zero ranges for now.
      DEBUG_ASSERT(state == DirtyState::Dirty);
      // Clear the old state.
      value_ &= ~(BIT_MASK(kDirtyStateBits) << kDirtyStateShift);
      // Set the new state.
      value_ |= static_cast<uint64_t>(state) << kDirtyStateShift;
    }

    // The AwaitingCleanLength will always be a page-aligned length, so we can mask out the low
    // PAGE_SIZE_SHIFT bits and store only the upper bits.
    static constexpr uint64_t kAwaitingCleanLengthShift = PAGE_SIZE_SHIFT;
    // Assert that we are not overlapping with the dirty state bits.
    static_assert(kAwaitingCleanLengthShift >= kDirtyStateShift + kDirtyStateBits);
    void SetAwaitingCleanLength(uint64_t len) {
      DEBUG_ASSERT(GetDirtyState() == DirtyState::Dirty);
      DEBUG_ASSERT(IS_ALIGNED(len, (1 << kAwaitingCleanLengthShift)));
      value_ |= (len & ~BIT_MASK(kAwaitingCleanLengthShift));
    }
    uint64_t GetAwaitingCleanLength() const {
      return value_ & ~BIT_MASK(kAwaitingCleanLengthShift);
    }

   private:
    uint64_t value_;
  };
  using IntervalDirtyState = ZeroRange::DirtyState;

  // Only support creation of zero interval type for now.
  [[nodiscard]] static VmPageOrMarker ZeroInterval(IntervalSentinel sentinel,
                                                   IntervalDirtyState state) {
    uint64_t sentinel_bits = static_cast<uint64_t>(sentinel) << kIntervalSentinelShift;
    uint64_t type_bits = static_cast<uint64_t>(IntervalType::Zero) << kIntervalTypeShift;
    return VmPageOrMarker(ZeroRange(0, state).value() | type_bits | sentinel_bits | kIntervalType);
  }

  // Getters and setters for the interval type.
  bool IsIntervalStart() const {
    return IsInterval() && GetIntervalSentinel() == IntervalSentinel::Start;
  }
  bool IsIntervalEnd() const {
    return IsInterval() && GetIntervalSentinel() == IntervalSentinel::End;
  }
  bool IsIntervalSlot() const {
    return IsInterval() && GetIntervalSentinel() == IntervalSentinel::Slot;
  }
  bool IsIntervalZero() const { return IsInterval() && GetIntervalType() == IntervalType::Zero; }

  // Change the interval sentinel type for an existing interval, while preserving the rest of the
  // original state. Only valid to call on an existing interval type. The only permissible
  // transitions are from Slot to Start/End and vice versa, as these are the only valid transitions
  // when extending or clipping intervals.
  void ChangeIntervalSentinel(IntervalSentinel new_sentinel) {
#if ZX_DEBUG_ASSERT_IMPLEMENTED
    DEBUG_ASSERT(IsInterval());
    auto old_sentinel = GetIntervalSentinel();
    DEBUG_ASSERT(old_sentinel != new_sentinel);
    if (old_sentinel == IntervalSentinel::Start || old_sentinel == IntervalSentinel::End) {
      DEBUG_ASSERT(new_sentinel == IntervalSentinel::Slot);
    } else {
      DEBUG_ASSERT(old_sentinel == IntervalSentinel::Slot);
      DEBUG_ASSERT(new_sentinel == IntervalSentinel::Start ||
                   new_sentinel == IntervalSentinel::End);
    }
#endif
    SetIntervalSentinel(new_sentinel);
  }

  // Getters and setter for the zero interval type.
  bool IsZeroIntervalClean() const {
    DEBUG_ASSERT(IsIntervalZero());
    return ZeroRange(raw_ & ~BIT_MASK(kIntervalBits)).GetDirtyState() ==
           ZeroRange::DirtyState::Clean;
  }
  bool IsZeroIntervalDirty() const {
    DEBUG_ASSERT(IsIntervalZero());
    return ZeroRange(raw_ & ~BIT_MASK(kIntervalBits)).GetDirtyState() ==
           ZeroRange::DirtyState::Dirty;
  }
  ZeroRange::DirtyState GetZeroIntervalDirtyState() const {
    DEBUG_ASSERT(IsIntervalZero());
    return ZeroRange(raw_ & ~BIT_MASK(kIntervalBits)).GetDirtyState();
  }
  void SetZeroIntervalAwaitingCleanLength(uint64_t len) {
    DEBUG_ASSERT(IsIntervalZero());
    DEBUG_ASSERT(IsIntervalStart() || IsIntervalSlot());
    DEBUG_ASSERT(IsZeroIntervalDirty());
    auto interval = ZeroRange(raw_ & ~BIT_MASK(kIntervalBits));
    interval.SetAwaitingCleanLength(len);
    raw_ = (raw_ & BIT_MASK(kIntervalBits)) | interval.value();
  }
  uint64_t GetZeroIntervalAwaitingCleanLength() const {
    DEBUG_ASSERT(IsIntervalZero());
    DEBUG_ASSERT(IsIntervalStart() || IsIntervalSlot());
    return ZeroRange(raw_ & ~BIT_MASK(kIntervalBits)).GetAwaitingCleanLength();
  }

 private:
  explicit VmPageOrMarker(uint64_t raw) : raw_(raw) {}

  // The low 2 bits of raw_ are reserved to select the type, any other data has to fit into the
  // remaining high bits. Note that there is no explicit Empty type, rather a PageType with a zero
  // pointer is used to represent Empty.
  static constexpr uint64_t kTypeBits = 2;
  static constexpr uint64_t kPageType = 0b00;
  static constexpr uint64_t kZeroMarkerType = 0b01;
  static constexpr uint64_t kReferenceType = 0b10;
  static constexpr uint64_t kIntervalType = 0b11;

  // In addition to storing the type, a reference needs to track two additional pieces of data,
  // these being the left and right split bits. The split bits are normally stored in the vm_page_t
  // and are used for copy-on-write tracking in hidden VMOs. Having the ability to store the split
  // bits here allows these pages to be candidates for compression. The remaining bits are then
  // available for the actual reference value being stored. Unlike the page type, which does not
  // allow the 0 value to be stored, a reference has no restrictions and a ref value of 0 is valid
  // and may be stored.
  static constexpr uint64_t kReferenceBits = kTypeBits + 2;
  // Due to ordering and public/private visibility ReferenceValue::kAlignBits is declared
  // separately, but it should match kReferenceBits.
  static_assert(ReferenceValue::kAlignBits == kReferenceBits);
  static constexpr uint64_t kReferenceLeftSplit = 0b10 << kTypeBits;
  static constexpr uint64_t kReferenceRightSplit = 0b01 << kTypeBits;

  // In addition to storing the type for an interval, we also need to track the type of interval
  // sentinel: the start, the end, or a single slot marker.
  static constexpr uint64_t kIntervalSentinelBits = 2;
  static_assert(static_cast<uint64_t>(IntervalSentinel::NumSentinels) <=
                (1 << kIntervalSentinelBits));
  static constexpr uint64_t kIntervalSentinelShift = kTypeBits;
  IntervalSentinel GetIntervalSentinel() const {
    return static_cast<IntervalSentinel>(
        (raw_ & (BIT_MASK(kIntervalSentinelBits) << kIntervalSentinelShift)) >>
        kIntervalSentinelShift);
  }
  void SetIntervalSentinel(IntervalSentinel sentinel) {
    // Clear the old sentinel type.
    raw_ &= ~(BIT_MASK(kIntervalSentinelBits) << kIntervalSentinelShift);
    // Set the new sentinel type.
    raw_ |= static_cast<uint64_t>(sentinel) << kIntervalSentinelShift;
  }
  // Next we also need to store the type of interval being represented; reserve a couple of bits for
  // this. Currently we only support one type of interval: a range of zero pages, but reserving 2
  // bits allows for more types in the future.
  static constexpr uint64_t kIntervalTypeBits = 2;
  static_assert(static_cast<uint64_t>(IntervalType::NumTypes) <= (1 << kIntervalTypeBits));
  static constexpr uint64_t kIntervalTypeShift = kIntervalSentinelShift + kIntervalSentinelBits;
  IntervalType GetIntervalType() const {
    return static_cast<IntervalType>((raw_ & (BIT_MASK(kIntervalTypeBits) << kIntervalTypeShift)) >>
                                     kIntervalTypeShift);
  }
  static constexpr uint64_t kIntervalBits = kTypeBits + kIntervalSentinelBits + kIntervalTypeBits;
  static_assert(ZeroRange::kAlignBits == kIntervalBits);

  uint64_t GetType() const { return raw_ & BIT_MASK(kTypeBits); }

  uint64_t Release() {
    const uint64_t p = raw_;
    raw_ = 0;
    return p;
  }

  uint64_t raw_;
};

// Limited reference to a VmPageOrMarker. This reference provides unrestricted const access to the
// underlying VmPageOrMarker, but as it holds a non-const VmPageOrMarker* it has the ability to
// modify the underlying entry. However, the interface for modification is very limited.
//
// This allows for the majority of VmPageList iterations that are not intended to allow for clearing
// entries to the Empty state to allow limited mutation (such as between different content states),
// without being completely mutable.
class VmPageOrMarkerRef {
 public:
  VmPageOrMarkerRef() = default;
  explicit VmPageOrMarkerRef(VmPageOrMarker* page_or_marker) : page_or_marker_(page_or_marker) {}
  ~VmPageOrMarkerRef() = default;

  const VmPageOrMarker& operator*() const {
    DEBUG_ASSERT(page_or_marker_);
    return *page_or_marker_;
  }

  const VmPageOrMarker* operator->() const {
    DEBUG_ASSERT(page_or_marker_);
    return page_or_marker_;
  }

  explicit operator bool() const { return !!page_or_marker_; }

  // Forward split bit modifications as an allowed mutation.
  void SetPageOrRefLeftSplit(bool value) {
    DEBUG_ASSERT(page_or_marker_);
    page_or_marker_->SetPageOrRefLeftSplit(value);
  }
  void SetPageOrRefRightSplit(bool value) {
    DEBUG_ASSERT(page_or_marker_);
    page_or_marker_->SetPageOrRefRightSplit(value);
  }

  // Changing the kind of content is an allowed mutation and this takes ownership of the provided
  // page and returns ownership of the previous reference.
  [[nodiscard]] VmPageOrMarker::ReferenceValue SwapReferenceForPage(vm_page_t* p) {
    DEBUG_ASSERT(page_or_marker_);
    return page_or_marker_->SwapReferenceForPage(p);
  }
  // Similar to SwapReferenceForPage, but takes ownership of the ref and returns ownership of the
  // previous page.
  [[nodiscard]] vm_page_t* SwapPageForReference(VmPageOrMarker::ReferenceValue ref) {
    DEBUG_ASSERT(page_or_marker_);
    return page_or_marker_->SwapPageForReference(ref);
  }
  // Similar to SwapReferenceForPage, but changes one reference for another.
  [[nodiscard]] VmPageOrMarker::ReferenceValue ChangeReferenceValue(
      VmPageOrMarker::ReferenceValue ref) {
    DEBUG_ASSERT(page_or_marker_);
    return page_or_marker_->ChangeReferenceValue(ref);
  }

 private:
  VmPageOrMarker* page_or_marker_ = nullptr;
};

class VmPageListNode final : public fbl::WAVLTreeContainable<ktl::unique_ptr<VmPageListNode>> {
 public:
  explicit VmPageListNode(uint64_t offset);
  ~VmPageListNode();

  DISALLOW_COPY_ASSIGN_AND_MOVE(VmPageListNode);

  static const size_t kPageFanOut = 16;

  // accessors
  uint64_t offset() const { return obj_offset_; }
  uint64_t GetKey() const { return obj_offset_; }

  uint64_t end_offset() const { return offset() + kPageFanOut * PAGE_SIZE; }

  void set_offset(uint64_t offset) {
    DEBUG_ASSERT(!InContainer());
    obj_offset_ = offset;
  }

  // for every page or marker in the node call the passed in function.
  template <typename PTR_TYPE, typename F>
  zx_status_t ForEveryPage(F func, uint64_t skew) {
    return ForEveryPageInRange<PTR_TYPE>(this, func, offset(), end_offset(), skew);
  }

  // for every page or marker in the node call the passed in function.
  template <typename PTR_TYPE, typename F>
  zx_status_t ForEveryPage(F func, uint64_t skew) const {
    return ForEveryPageInRange<PTR_TYPE>(this, func, offset(), end_offset(), skew);
  }

  // for every page or marker in the node in the range call the passed in function. The range is
  // assumed to be within the nodes object range.
  template <typename PTR_TYPE, typename F>
  zx_status_t ForEveryPageInRange(F func, uint64_t start_offset, uint64_t end_offset,
                                  uint64_t skew) {
    return ForEveryPageInRange<PTR_TYPE>(this, func, start_offset, end_offset, skew);
  }

  // for every page or marker in the node in the range call the passed in function. The range is
  // assumed to be within the nodes object range.
  template <typename PTR_TYPE, typename F>
  zx_status_t ForEveryPageInRange(F func, uint64_t start_offset, uint64_t end_offset,
                                  uint64_t skew) const {
    return ForEveryPageInRange<PTR_TYPE>(this, func, start_offset, end_offset, skew);
  }

  const VmPageOrMarker& Lookup(size_t index) const {
    canary_.Assert();
    DEBUG_ASSERT(index < kPageFanOut);
    return pages_[index];
  }

  VmPageOrMarker& Lookup(size_t index) {
    canary_.Assert();
    DEBUG_ASSERT(index < kPageFanOut);
    return pages_[index];
  }

  // A node is empty if it contains no pages, page interval sentinels, references, or markers.
  bool IsEmpty() const {
    for (const auto& p : pages_) {
      if (!p.IsEmpty()) {
        return false;
      }
    }
    return true;
  }

  // Returns true if there are no pages or references owned by this node. Meant to check whether the
  // node has any resource that needs to be returned.
  bool HasNoPageOrRef() const {
    for (const auto& p : pages_) {
      if (p.IsPageOrRef()) {
        return false;
      }
    }
    return true;
  }

 private:
  template <typename PTR_TYPE, typename S, typename F>
  static zx_status_t ForEveryPageInRange(S self, F func, uint64_t start_offset, uint64_t end_offset,
                                         uint64_t skew) {
    // Assert that the requested range is sensible and falls within our nodes actual offset range.
    DEBUG_ASSERT(end_offset >= start_offset);
    DEBUG_ASSERT(start_offset >= self->obj_offset_);
    DEBUG_ASSERT(end_offset <= self->end_offset());
    const size_t start = (start_offset - self->obj_offset_) / PAGE_SIZE;
    const size_t end = (end_offset - self->obj_offset_) / PAGE_SIZE;
    for (size_t i = start; i < end; i++) {
      if (!self->pages_[i].IsEmpty()) {
        zx_status_t status =
            func(PTR_TYPE{&self->pages_[i]}, self->obj_offset_ + i * PAGE_SIZE - skew);
        if (unlikely(status != ZX_ERR_NEXT)) {
          return status;
        }
      }
    }
    return ZX_ERR_NEXT;
  }

  fbl::Canary<fbl::magic("PLST")> canary_;

  uint64_t obj_offset_ = 0;
  VmPageOrMarker pages_[kPageFanOut];
};

class VmPageList;

// Class which holds the list of vm_page structs removed from a VmPageList
// by TakePages. The list include information about uncommitted pages and markers.
class VmPageSpliceList final {
 public:
  VmPageSpliceList();
  VmPageSpliceList(VmPageSpliceList&& other);
  VmPageSpliceList& operator=(VmPageSpliceList&& other_tree);
  ~VmPageSpliceList();

  // For use by PhysicalPageProvider.  The user-pager path doesn't use this.
  static VmPageSpliceList CreateFromPageList(uint64_t offset, uint64_t length, list_node* pages);

  // Pops the next page off of the splice.
  VmPageOrMarker Pop();

  // Returns true after the whole collection has been processed by Pop.
  bool IsDone() const { return pos_ >= length_; }

  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(VmPageSpliceList);

 private:
  VmPageSpliceList(uint64_t offset, uint64_t length);
  void FreeAllPages();

  uint64_t offset_;
  uint64_t length_;
  uint64_t pos_ = 0;

  VmPageListNode head_ = VmPageListNode(0);
  fbl::WAVLTree<uint64_t, ktl::unique_ptr<VmPageListNode>> middle_;
  VmPageListNode tail_ = VmPageListNode(0);

  // To avoid the possibility of allocation failure, we don't use head_, middle_, tail_ for
  // CreateFromPageList().  With CreateFromPageList() we know that all the pages are present, so
  // we can just keep a list of pages, and create VmPageListNode on the stack as pages are Pop()ed.
  list_node raw_pages_ = LIST_INITIAL_VALUE(raw_pages_);

  friend VmPageList;
};

class VmPageList final {
 public:
  VmPageList();
  ~VmPageList();

  VmPageList& operator=(VmPageList&& other);
  VmPageList(VmPageList&& other);

  void InitializeSkew(uint64_t parent_skew, uint64_t offset) {
    // Checking list_skew_ doesn't catch all instances of double-initialization, but
    // it should catch some of them.
    DEBUG_ASSERT(list_skew_ == 0);
    DEBUG_ASSERT(list_.is_empty());

    list_skew_ = (parent_skew + offset) % (PAGE_SIZE * VmPageListNode::kPageFanOut);
  }
  uint64_t GetSkew() const { return list_skew_; }

  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(VmPageList);

  // walk the page tree, calling the passed in function on every tree node.
  template <typename F>
  zx_status_t ForEveryPage(F per_page_func) const {
    return ForEveryPage<const VmPageOrMarker*>(this, per_page_func);
  }

  // similar to ForEveryPage, but the per_page_func gets called with a VmPageOrMarkerRef instead of
  // a const VmPageOrMarker*, allowing for limited mutation.
  template <typename F>
  zx_status_t ForEveryPageMutable(F per_page_func) {
    return ForEveryPage<VmPageOrMarkerRef>(this, per_page_func);
  }

  // walk the page tree, calling the passed in function on every tree node.
  template <typename F>
  zx_status_t ForEveryPageInRange(F per_page_func, uint64_t start_offset,
                                  uint64_t end_offset) const {
    return ForEveryPageInRange<const VmPageOrMarker*>(this, per_page_func, start_offset,
                                                      end_offset);
  }

  // similar to ForEveryPageInRange, but the per_page_func gets called with a VmPageOrMarkerRef
  // instead of a const VmPageOrMarker*, allowing for limited mutation.
  template <typename F>
  zx_status_t ForEveryPageInRangeMutable(F per_page_func, uint64_t start_offset,
                                         uint64_t end_offset) {
    return ForEveryPageInRange<VmPageOrMarkerRef>(this, per_page_func, start_offset, end_offset);
  }

  // walk the page tree, calling |per_page_func| on every page/marker and |per_gap_func| on every
  // gap.
  template <typename PAGE_FUNC, typename GAP_FUNC>
  zx_status_t ForEveryPageAndGapInRange(PAGE_FUNC per_page_func, GAP_FUNC per_gap_func,
                                        uint64_t start_offset, uint64_t end_offset) const {
    return ForEveryPageAndGapInRange<const VmPageOrMarker*>(this, per_page_func, per_gap_func,
                                                            start_offset, end_offset);
  }

  // walk the page tree, calling |per_page_func| on every page/marker that fulfills (returns true)
  // the |compare_func|. Also call |contiguous_run_func| on every contiguous range of such
  // pages/markers encountered.
  template <typename COMPARE_FUNC, typename PAGE_FUNC, typename CONTIGUOUS_RUN_FUNC>
  zx_status_t ForEveryPageAndContiguousRunInRange(COMPARE_FUNC compare_func,
                                                  PAGE_FUNC per_page_func,
                                                  CONTIGUOUS_RUN_FUNC contiguous_run_func,
                                                  uint64_t start_offset,
                                                  uint64_t end_offset) const {
    return ForEveryPageAndContiguousRunInRange<const VmPageOrMarker*>(
        this, compare_func, per_page_func, contiguous_run_func, start_offset, end_offset);
  }

  // Returns true if any pages (actual pages, references, or markers) are in the given range, or if
  // the range forms a part of a sparse page interval.
  bool AnyPagesOrIntervalsInRange(uint64_t start_offset, uint64_t end_offset) const {
    bool found_page = false;
    ForEveryPageInRange(
        [&found_page](const VmPageOrMarker* page, uint64_t offset) {
          found_page = true;
          return ZX_ERR_STOP;
        },
        start_offset, end_offset);
    // It is possible that the range forms a part of an interval even if no nodes in the range have
    // populated slots. We can determine that by checking to see if the start offset in the range
    // falls in an interval (we could technically perform this check for any inclusive offset in the
    // range since the range is entirely unpopulated and hence would only fall in the same interval
    // if applicable).
    return found_page ? true : IsOffsetInInterval(start_offset);
  }

  // Attempts to return a reference to the VmPageOrMarker at the specified offset. The returned
  // pointer is valid until the VmPageList is destroyed or any of the Remove*/Take/Merge etc
  // functions are called.
  //
  // Lookup may return 'nullptr' if there is no slot allocated for the given offset. If non-null
  // is returned it may still be the case that IsEmpty() on the returned PageOrMarker is true.
  const VmPageOrMarker* Lookup(uint64_t offset) const;

  // Similar to `Lookup` but returns a VmPageOrMarkerRef that allows for limited mutation of the
  // slot. General mutation requires calling `LookupOrAllocate`.
  VmPageOrMarkerRef LookupMutable(uint64_t offset);

  // Similar to `Lookup` but only returns `nullptr` if a slot cannot be allocated either due to out
  // of memory or due to offset being invalid.
  //
  // The returned slot, if not a `nullptr`, may generally be freely manipulated with the exception
  // that if it started !Empty, then it is an error to set it to Empty. In this case the
  // `RemovePage` method must be used.
  //
  // If the returned slot started Empty, as it not made !Empty, then the slot must be returned with
  // ReturnEmptySlot, to ensure no empty nodes are retained.
  VmPageOrMarker* LookupOrAllocate(uint64_t offset);

  // Similar to LookupOrAllocate but also checks if offset falls in a sparse page interval,
  // returning true via the bool in ktl::pair if it does, along with the slot. Also splits the
  // interval around offset if split_interval is set to true. This allows the caller to freely
  // manipulate the slot at offset similar to LookupOrAllocate. If offset is found in an interval,
  // but split_interval was false, no VmPageOrMarker* is returned, as it is not safe to manipulate
  // any slot in an interval without also splitting the interval around it.
  //
  // In other words, the return values fall into three categories.
  //  1. {page, false} : offset does not lie in an interval. |slot| is the required slot.
  //  2. {page, true} : offset lies in an interval and split_interval was true. |page| is the
  //  required slot. The interval has been correctly split around the slot, so |page| can be treated
  //  similar to any non-interval type.
  //  3. {nullptr, true} : offset lies in an interval but split_interval was false. No slot is
  //  returned.
  //
  // Splitting the interval would look as follows. If the interval previously was:
  //  [start, end) where start < offset < end.
  // After the split we would have three intervals:
  //  [start, offset) [offset, offset + PAGE_SIZE) [offset + PAGE_SIZE, end)
  // The middle interval containing offset spans only a single page, i.e. offset is an
  // IntervalSentinel::Slot, which can now be manipulated independently.
  ktl::pair<VmPageOrMarker*, bool> LookupOrAllocateCheckForInterval(uint64_t offset,
                                                                    bool split_interval);

  // Returns a slot that was empty after LookupOrAllocate, and that the caller did not end up
  // filling.
  // This ensures that if LookupOrAllocate allocated a new underlying list node, then that list node
  // needs to be free'd otherwise it might not get cleaned up for the lifetime of the page list.
  //
  // This is only correct to call on an offset for which LookupOrAllocate had just returned a non
  // null slot, and that slot was Empty and is still Empty.
  void ReturnEmptySlot(uint64_t offset);

  // Removes any item at |offset| from the list and returns it, or VmPageOrMarker::Empty() if none.
  VmPageOrMarker RemoveContent(uint64_t offset);

  // Release every item in the page list and calls free_content_fn on any content, giving it
  // ownership. Any markers are cleared.
  template <typename T>
  void RemoveAllContent(T free_content_fn) {
    // per page get a reference to the page pointer inside the page list node
    auto per_page_func = [&free_content_fn](VmPageOrMarker* p, uint64_t offset) {
      if (p->IsPageOrRef()) {
        free_content_fn(ktl::move(*p));
      }
      *p = VmPageOrMarker::Empty();
      return ZX_ERR_NEXT;
    };

    // walk the tree in order, freeing all the pages on every node
    ForEveryPage<VmPageOrMarker*>(this, per_page_func);

    // empty the tree
    list_.clear();
  }

  // Calls the provided callback for every page or marker in the range [start_offset, end_offset).
  // The callback can modify the VmPageOrMarker and take ownership of any pages, or leave them in
  // place. The difference between this and ForEveryPage is as this allows for modifying the
  // underlying pages any intermediate data structures can be checked and potentially freed if no
  // longer needed.
  template <typename T>
  void RemovePages(T per_page_fn, uint64_t start_offset, uint64_t end_offset) {
    ForEveryPageInRange<VmPageOrMarker*, NodeCheck::CleanupEmpty>(this, per_page_fn, start_offset,
                                                                  end_offset);
  }

  // Similar to RemovePages but also takes a |per_gap_fn| callback to allow for iterating over any
  // gaps encountered as well. This can be used when the intent is to modify the underlying pages
  // and/or gaps, while checking any intermediate data structures to potentially free ones that are
  // no longer needed.
  template <typename P, typename G>
  zx_status_t RemovePagesAndIterateGaps(P per_page_fn, G per_gap_fn, uint64_t start_offset,
                                        uint64_t end_offset) {
    return ForEveryPageAndGapInRange<VmPageOrMarker*, NodeCheck::CleanupEmpty>(
        this, per_page_fn, per_gap_fn, start_offset, end_offset);
  }

  // Returns true if there are no pages, references, markers, or intervals in the page list.
  bool IsEmpty() const;

  // Returns true if the page list does not own any pages or references. Meant to check whether the
  // page list has any resource that needs to be returned.
  bool HasNoPageOrRef() const;

  // Merges the pages in |other| in the range [|offset|, |end_offset|) into |this|
  // page list, starting at offset 0 in this list.
  //
  // For every page in |other| in the given range, if there is no corresponding page or marker
  // in |this|, then they will be passed to |migrate_fn|. If |migrate_fn| leaves the page in the
  // VmPageOrMarker it will be migrated into |this|, otherwise the migrate_fn is assumed to now own
  // the page. For any pages or markers in |other| outside the given range or which conflict with a
  // page in |this|, they will be released given ownership to |release_fn|.
  //
  // The |offset| values passed to |release_fn| and |migrate_fn| are the original offsets
  // in |other|, not the adapted offsets in |this|.
  //
  // **NOTE** unlike MergeOnto, |other| will be empty at the end of this method.
  void MergeFrom(
      VmPageList& other, uint64_t offset, uint64_t end_offset,
      fit::inline_function<void(VmPageOrMarker&&, uint64_t offset), 3 * sizeof(void*)> release_fn,
      fit::inline_function<void(VmPageOrMarker*, uint64_t offset)> migrate_fn);

  // Merges this pages in |this| onto |other|.
  //
  // For every page (or marker) in |this|, checks the same offset in |other|. If there is no
  // page or marker, then it inserts the page into |other|. Otherwise, it releases the page (or
  // marker) and gives ownership to |release_fn|.
  //
  // **NOTE** unlike MergeFrom, |this| will be empty at the end of this method.
  void MergeOnto(VmPageList& other, fit::inline_function<void(VmPageOrMarker&&)> release_fn);

  // Takes the pages, references and markers in the range [offset, length) out of this page list.
  VmPageSpliceList TakePages(uint64_t offset, uint64_t length);

  uint64_t HeapAllocationBytes() const { return list_.size() * sizeof(VmPageListNode); }

  // Allow the implementation to use a one-past-the-end for VmPageListNode offsets,
  // plus to account for skew_.
  static constexpr uint64_t MAX_SIZE =
      ROUNDDOWN(UINT64_MAX, 2 * VmPageListNode::kPageFanOut * PAGE_SIZE);

  // Add a sparse zero interval spanning the range [start_offset, end_offset) with the specified
  // dirty_state. The specified range must be previously unpopulated. This will try to merge the new
  // zero interval with existing intervals to the left and/or right, if the dirty_state allows it.
  zx_status_t AddZeroInterval(uint64_t start_offset, uint64_t end_offset,
                              VmPageOrMarker::IntervalDirtyState dirty_state);

 private:
  // Returns true if the specified offset falls in a sparse page interval.
  bool IsOffsetInInterval(uint64_t offset) const;

  // Internal helper used when checking whether the offset falls in an interval.
  // lower_bound is the node that was queried with a lower_bound() lookup on the list using the
  // offset. This node is passed in here so that we can reuse the node the callsite has looked up
  // and avoid an extra lookup. The interval sentinel found in lower_bound which is used to
  // conclude that offset lies is an interval is optionally returned. If this function returns true,
  // interval_out (if not null) returns the start/end/slot sentinel of the interval that the offset
  // lies in.
  bool IfOffsetInIntervalHelper(uint64_t offset, const VmPageListNode& lower_bound,
                                const VmPageOrMarker** interval_out = nullptr) const;

  template <typename PTR_TYPE, typename S, typename F>
  static zx_status_t ForEveryPage(S self, F per_page_func) {
    for (auto& pl : self->list_) {
      zx_status_t status = pl.template ForEveryPage<PTR_TYPE, F>(per_page_func, self->list_skew_);
      if (unlikely(status != ZX_ERR_NEXT)) {
        if (status == ZX_ERR_STOP) {
          break;
        }
        return status;
      }
    }
    return ZX_OK;
  }

  // Calls the provided callback for every page in the given range. If the CleanupNodes template
  // argument is true then it is assumed the per_page_func may remove pages and page nodes will be
  // checked to see if they are empty and can be cleaned up.
  enum class NodeCheck : bool {
    Skip = false,
    CleanupEmpty = true,
  };
  template <typename PTR_TYPE, NodeCheck NODE_CHECK = NodeCheck::Skip, typename S, typename F>
  static zx_status_t ForEveryPageInRange(S self, F per_page_func, uint64_t start_offset,
                                         uint64_t end_offset) {
    start_offset += self->list_skew_;
    end_offset += self->list_skew_;

    // Find the first node (if any) that will contain our starting offset.
    auto cur =
        self->list_.lower_bound(ROUNDDOWN(start_offset, VmPageListNode::kPageFanOut * PAGE_SIZE));
    if (!cur) {
      return ZX_OK;
    }

    // Handle scenario where start_offset begins not aligned to a node.
    if (cur->offset() < start_offset) {
      zx_status_t status = cur->template ForEveryPageInRange<PTR_TYPE, F>(
          per_page_func, start_offset, ktl::min(end_offset, cur->end_offset()), self->list_skew_);
      auto prev = cur++;
      if constexpr (NODE_CHECK == NodeCheck::CleanupEmpty) {
        if (prev->IsEmpty()) {
          self->list_.erase(prev);
        }
      }
      if (unlikely(status != ZX_ERR_NEXT)) {
        if (status == ZX_ERR_STOP) {
          return ZX_OK;
        }
        return status;
      }
    }
    // Iterate through all full nodes contained in the range.
    while (cur && cur->end_offset() < end_offset) {
      DEBUG_ASSERT(start_offset <= cur->offset());
      zx_status_t status = cur->template ForEveryPage<PTR_TYPE, F>(per_page_func, self->list_skew_);
      auto prev = cur++;
      if constexpr (NODE_CHECK == NodeCheck::CleanupEmpty) {
        if (prev->IsEmpty()) {
          self->list_.erase(prev);
        }
      }
      if (unlikely(status != ZX_ERR_NEXT)) {
        if (status == ZX_ERR_STOP) {
          return ZX_OK;
        }
        return status;
      }
    }
    // Handle scenario where the end_offset is not aligned to the end of a node.
    if (cur && cur->offset() < end_offset) {
      DEBUG_ASSERT(cur->end_offset() >= end_offset);
      zx_status_t status = cur->template ForEveryPageInRange<PTR_TYPE, F>(
          per_page_func, cur->offset(), end_offset, self->list_skew_);
      if constexpr (NODE_CHECK == NodeCheck::CleanupEmpty) {
        if (cur->IsEmpty()) {
          self->list_.erase(cur);
        }
      }
      if (unlikely(status != ZX_ERR_NEXT)) {
        if (status == ZX_ERR_STOP) {
          return ZX_OK;
        }
        return status;
      }
    }
    return ZX_OK;
  }

  template <typename PTR_TYPE, NodeCheck NODE_CHECK = NodeCheck::Skip, typename S,
            typename PAGE_FUNC, typename GAP_FUNC>
  static zx_status_t ForEveryPageAndGapInRange(S self, PAGE_FUNC per_page_func,
                                               GAP_FUNC per_gap_func, uint64_t start_offset,
                                               uint64_t end_offset) {
    uint64_t expected_next_off = start_offset;
    // Set to true when we encounter an interval start but haven't yet encountered the end.
    bool in_interval = false;
    auto per_page_wrapper_fn = [&expected_next_off, &in_interval, start_offset, end_offset,
                                per_page_func, &per_gap_func](auto* p, uint64_t off) {
      zx_status_t status = ZX_ERR_NEXT;
      // We can move ahead of expected_next_off in the case of an interval too, which represents a
      // run of pages. Make sure this is not an interval before calling the per_gap_func.
      if (expected_next_off != off && !p->IsIntervalEnd()) {
        status = per_gap_func(expected_next_off, off);
      }
      if (status == ZX_ERR_NEXT) {
        if (p->IsIntervalStart()) {
          // We should not already have been tracking an interval.
          DEBUG_ASSERT(!in_interval);
          in_interval = true;
        } else if (p->IsIntervalEnd()) {
          // If this is not the first populated slot we encountered, we should have been tracking a
          // valid interval.
          DEBUG_ASSERT(in_interval || expected_next_off == start_offset);
          // Reset interval tracking.
          in_interval = false;
        }
        status = per_page_func(p, off);
      }
      expected_next_off = off + PAGE_SIZE;
      // Prevent the last call to per_gap_func
      if (status == ZX_ERR_STOP) {
        expected_next_off = end_offset;
      }
      return status;
    };

    zx_status_t status = ForEveryPageInRange<PTR_TYPE, NODE_CHECK>(self, per_page_wrapper_fn,
                                                                   start_offset, end_offset);
    if (status != ZX_OK) {
      return status;
    }

    // Handle the last gap after checking that we are not in an interval. Note that simply checking
    // for in_interval is not sufficient, as it is possible to have started the traversal partway
    // into an interval, in which case we would not have seen the interval start and in_interval
    // would be false. So we perform a quick check for in_interval first and if that fails perform
    // the more expensive IsOffsetInInterval() check. The IsOffsetInInterval() call is further gated
    // by whether we encountered any page at all in the traversal above. If we saw at least one
    // page in the traversal, we know that we could not be in an interval without in_interval being
    // true because we would have seen the interval start.
    if (expected_next_off != end_offset) {
      // Traversal ended in an interval if in_interval was true, OR if the traversal did not see any
      // page at all and the start_offset is in an interval (Note that in this latter case all
      // offsets in the range [start_offset, end_offset) would lie in the same interval, so we can
      // just check one of them).
      bool ended_in_interval = in_interval || (expected_next_off == start_offset &&
                                               self->IsOffsetInInterval(start_offset));
      if (!ended_in_interval) {
        status = per_gap_func(expected_next_off, end_offset);
        if (status != ZX_ERR_NEXT && status != ZX_ERR_STOP) {
          return status;
        }
      }
    }

    return ZX_OK;
  }

  template <typename PTR_TYPE, typename S, typename COMPARE_FUNC, typename PAGE_FUNC,
            typename CONTIGUOUS_RUN_FUNC>
  static zx_status_t ForEveryPageAndContiguousRunInRange(S self, COMPARE_FUNC compare_func,
                                                         PAGE_FUNC per_page_func,
                                                         CONTIGUOUS_RUN_FUNC contiguous_run_func,
                                                         uint64_t start_offset,
                                                         uint64_t end_offset) {
    // Track contiguous range of pages fulfilling compare_func.
    uint64_t contiguous_run_start = start_offset;
    uint64_t contiguous_run_len = 0;

    zx_status_t status = ForEveryPageAndGapInRange<PTR_TYPE>(
        self,
        [&](const VmPageOrMarker* p, uint64_t off) {
          zx_status_t st = ZX_ERR_NEXT;
          if (compare_func(p, off)) {
            st = per_page_func(p, off);
            // Return any errors early before considering this page for contiguous_run_func.
            if (st != ZX_ERR_NEXT && st != ZX_ERR_STOP) {
              // If there was an outstanding contiguous run, process it since it had to have ended
              // before the failing offset.
              if (contiguous_run_len > 0) {
                zx_status_t prev_range_status = contiguous_run_func(
                    contiguous_run_start, contiguous_run_start + contiguous_run_len);
                contiguous_run_len = 0;
                // If there was an error encountered, surface that instead of st, as it occurred on
                // a range prior to this offset.
                if (prev_range_status != ZX_ERR_NEXT && prev_range_status != ZX_ERR_STOP) {
                  return prev_range_status;
                }
              }
              return st;
            }
            // Start tracking a new range first if no range is being tracked yet.
            if (contiguous_run_len == 0) {
              contiguous_run_start = off;
            }
            // Append this page to the contiguous range being tracked.
            contiguous_run_len += PAGE_SIZE;
            // In the case that st is ZX_ERR_STOP, we will include this page in the contiguous run
            // and stop traversal *after* this page.
            return st;
          }
          // We were already tracking a contiguous range when we encountered this page that does not
          // fulfill compare_func. Invoke contiguous_run_func on the range so far and start tracking
          // a new one skipping over this page.
          if (contiguous_run_len > 0) {
            st = contiguous_run_func(contiguous_run_start,
                                     contiguous_run_start + contiguous_run_len);
            // Reset contiguous_run_len to zero to track a new range later if required.
            // Do this irrespective of the return status to ensure we don't erroneously have a
            // remaining range to process below after exiting the traversal.
            contiguous_run_len = 0;
          }
          return st;
        },
        [&](uint64_t start, uint64_t end) {
          zx_status_t st = ZX_ERR_NEXT;
          // We were already tracking a contiguous range when we encountered this gap. Invoke
          // contiguous_run_func on the range so far and start tracking a new one skipping over this
          // gap.
          if (contiguous_run_len > 0) {
            st = contiguous_run_func(contiguous_run_start,
                                     contiguous_run_start + contiguous_run_len);
            // Reset contiguous_run_len to zero to track a new range later if required.
            // Do this irrespective of the return status to ensure we don't erroneously have a
            // remaining range to process below after exiting the traversal.
            contiguous_run_len = 0;
          }
          return st;
        },
        start_offset, end_offset);

    if (status != ZX_OK) {
      return status;
    }

    // Process the last contiguous range if there is one.
    if (contiguous_run_len > 0) {
      status = contiguous_run_func(contiguous_run_start, contiguous_run_start + contiguous_run_len);
      if (status != ZX_ERR_NEXT && status != ZX_ERR_STOP) {
        return status;
      }
    }

    return ZX_OK;
  }

  fbl::WAVLTree<uint64_t, ktl::unique_ptr<VmPageListNode>> list_;
  // A skew added to offsets provided as arguments to VmPageList functions before
  // interfacing with list_. This allows all VmPageLists within a clone tree
  // to place individual vm_page_t entries at the same offsets within their nodes, so
  // that the nodes can be moved between different lists without having to worry
  // about needing to split up a node.
  uint64_t list_skew_ = 0;
};

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_VM_PAGE_LIST_H_
