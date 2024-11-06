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

class VmPageList;
class VMPLCursor;

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
//
// There are certain invariants that the page list tries to maintain at all times. It might not
// always be possible to enforce these as the checks involved might be expensive, however it is
// important that any code that manipulates the page list abide by them, primarily to keep the
// memory occupied by the page list nodes in check.
// 1. Page list nodes cannot be completely empty i.e. they must contain at least one non-empty slot.
// 2. Any intervals in the page list should span a maximal range. In other words, there should not
// be consecutive intervals in the page list which it would have been possible to represent with a
// single interval instead.
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
    return ReferenceValue(raw_ & ~BIT_MASK(ReferenceValue::kAlignBits));
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
    return ReferenceValue(Release() & ~BIT_MASK(ReferenceValue::kAlignBits));
  }

  // Changes the content from a reference to a page and returns the original reference.
  [[nodiscard]] VmPageOrMarker::ReferenceValue SwapReferenceForPage(vm_page_t* p) {
    DEBUG_ASSERT(p);

    VmPageOrMarker::ReferenceValue ref = ReleaseReference();
    *this = VmPageOrMarker::Page(p);

    return ref;
  }

  // Changes the content from a page to a reference and returns the original page.
  [[nodiscard]] vm_page_t* SwapPageForReference(VmPageOrMarker::ReferenceValue ref) {
    vm_page_t* page = ReleasePage();
    *this = VmPageOrMarker::Reference(ref);

    return page;
  }

  // Changes the content from one reference to a different one and returns the original reference.
  [[nodiscard]] VmPageOrMarker::ReferenceValue SwapReferenceForReference(
      VmPageOrMarker::ReferenceValue ref) {
    const VmPageOrMarker::ReferenceValue old = ReleaseReference();
    *this = VmPageOrMarker::Reference(ref);

    return old;
  }

  [[nodiscard]] VmPageOrMarker Swap(VmPageOrMarker&& other) {
    uint64_t ret = raw_;
    raw_ = other.Release();
    return VmPageOrMarker(ret);
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

  [[nodiscard]] static VmPageOrMarker Reference(ReferenceValue ref) {
    return VmPageOrMarker(ref.value() | kReferenceType);
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
      // Clear the old value.
      value_ &= BIT_MASK(kAwaitingCleanLengthShift);
      // Set the new value.
      value_ |= (len & ~BIT_MASK(kAwaitingCleanLengthShift));
    }
    uint64_t GetAwaitingCleanLength() const {
      return value_ & ~BIT_MASK(kAwaitingCleanLengthShift);
    }

   private:
    uint64_t value_;
  };
  using IntervalDirtyState = ZeroRange::DirtyState;

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

  // Ensure two additional bits are available for storing metadata inside of references. The
  // remaining bits are available for the actual ref value being stored. Unlike the page type, which
  // does not allow the 0 value to be stored, a ref value of 0 is valid and may be stored.
  static_assert(ReferenceValue::kAlignBits == (kTypeBits + 2));

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

  // Only support creation of zero interval type for now.
  // Private and only friended with VmPageList so that an external caller cannot arbitrarily create
  // interval sentinels.
  [[nodiscard]] static VmPageOrMarker ZeroInterval(IntervalSentinel sentinel,
                                                   IntervalDirtyState state) {
    uint64_t sentinel_bits = static_cast<uint64_t>(sentinel) << kIntervalSentinelShift;
    uint64_t type_bits = static_cast<uint64_t>(IntervalType::Zero) << kIntervalTypeShift;
    return VmPageOrMarker(ZeroRange(0, state).value() | type_bits | sentinel_bits | kIntervalType);
  }

  // Change the interval sentinel type for an existing interval, while preserving the rest of the
  // original state. Only valid to call on an existing interval type. The only permissible
  // transitions are from Slot to Start/End and vice versa, as these are the only valid transitions
  // when extending or clipping intervals.
  // Private and only friended with VmPageList so that an external caller cannot arbitrarily
  // manipulate interval sentinels.
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

  uint64_t GetType() const { return raw_ & BIT_MASK(kTypeBits); }

  uint64_t Release() {
    const uint64_t p = raw_;
    raw_ = 0;
    return p;
  }

  uint64_t raw_;

  friend VmPageList;
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
  [[nodiscard]] VmPageOrMarker::ReferenceValue SwapReferenceForReference(
      VmPageOrMarker::ReferenceValue ref) {
    DEBUG_ASSERT(page_or_marker_);
    return page_or_marker_->SwapReferenceForReference(ref);
  }

  // Replaces the contents of this VmPageOrMarker with some non-empty contents, and returns what was
  // previously present. The previous content is allowed to be empty, but the provided content must
  // be non-empty.
  [[nodiscard]] VmPageOrMarker SwapContent(VmPageOrMarker&& content) {
    DEBUG_ASSERT(!content.IsEmpty());
    DEBUG_ASSERT(page_or_marker_);
    return page_or_marker_->Swap(ktl::move(content));
  }

  // Forward dirty state updates as an allowed mutation.
  void SetZeroIntervalAwaitingCleanLength(uint64_t len) {
    DEBUG_ASSERT(page_or_marker_);
    page_or_marker_->SetZeroIntervalAwaitingCleanLength(len);
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

  uint64_t end_offset() const {
    const uint64_t end = obj_offset_ + kPageFanOut * PAGE_SIZE;
    // By construction the node cannot overflow, but the compiler does not know this. By explicitly
    // telling it some checks can be avoided as the compiler does not have to consider the case
    // where end wrapped.
    if (end <= obj_offset_) {
      __builtin_unreachable();
    }
    return end;
  }

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

  // Returns true if there are no interval sentinels owned by this node.
  bool HasNoIntervalSentinel() const {
    for (const auto& p : pages_) {
      if (p.IsInterval()) {
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

  friend VMPLCursor;
};

// Cursor that can be used for iterating over contiguous blocks of entries in a page list. The
// underlying page list must not have any entries removed while using this cursor, as the cursor
// retains iterators into the page list. It is, however, safe to insert new entries.
// The cursor can be used to iterate over empty contiguous slots, however iteration will always
// cease if entries are not contiguous.
class VMPLCursor {
 public:
  VMPLCursor() : index_(kPageFanOut) {}

  // Retrieve the current VmPageOrMarker pointed at by the cursor. This will be a nullptr if the
  // cursor is no longer valid. The slot pointed at may itself be empty.
  // Note that it is up to the caller to know the offset, which it can track by remembering how
  // many |step|s it has done.
  VmPageOrMarkerRef current() const {
    return VmPageOrMarkerRef(valid() ? &(node_->pages_[index_]) : nullptr);
  }

  // Move the cursor to the next entry. The next entry can then be retrieved by calling |current|,
  // and if there is no next entry then current will return a nullptr.
  void step() {
    if (valid()) {
      index_++;
      if (index_ == kPageFanOut) {
        inc_node();
      }
    }
  }

  // Calls the provided callback of type [](VmPageOrMarkerRef)->zx_status_t on every entry as long
  // as they are contiguous. This is equivalent a loop calling |step| and |current|, but can
  // produce more optimal code gen with the internal loop.
  // The callback can return ZX_ERR_NEXT to continue, ZX_ERR_STOP to cease iteration gracefully, or
  // any other status to terminate with that status code.
  template <typename F>
  zx_status_t ForEveryContiguous(F func) {
    while (valid()) {
      while (index_ < kPageFanOut) {
        zx_status_t status = func(VmPageOrMarkerRef(&node_->pages_[index_]));
        if (status != ZX_ERR_NEXT) {
          return status == ZX_ERR_STOP ? ZX_OK : status;
        }
        index_++;
      }
      if (!inc_node()) {
        return ZX_OK;
      }
    }
    return ZX_OK;
  }

 private:
  static constexpr size_t kPageFanOut = VmPageListNode::kPageFanOut;

  VMPLCursor(fbl::WAVLTree<uint64_t, ktl::unique_ptr<VmPageListNode>>::iterator&& node, uint index)
      : node_(node), index_(index) {}

  // Helper to increment the underlying node_, testing for contiguity.
  bool inc_node() {
    // Should only be incrementing if index is at the end, as otherwise we're not being contiguous.
    DEBUG_ASSERT(index_ == kPageFanOut);
    const uint64_t prev = node_->obj_offset_;
    node_++;
    if (node_.IsValid() && node_->obj_offset_ == prev + PAGE_SIZE * kPageFanOut) {
      // node is valid and contiguous, reset the index_ to both remove the terminal sentinel, and
      // resume iteration from the beginning.
      index_ = 0;
      // TODO: Once cursor is in use benchmark the impact of validating that the node is not empty.
      return true;
    }
    return false;
  }

  // Helper to check if the node is valid or not by checking index for its sentinel value.
  bool valid() const { return index_ < kPageFanOut; }

  // Current node_ in the underlying page list currently being iterated. If this is invalid then
  // index_ will be kPageFanOut
  fbl::WAVLTree<uint64_t, ktl::unique_ptr<VmPageListNode>>::iterator node_;

  // The index into node_ that is currently being pointed at to be returned by |current|. The
  // sentinel value of kPageFanOut is used to indicate that node_ is no longer valid.
  uint index_;

  friend VmPageList;
};

// Class which holds the list of vm_page structs removed from a VmPageList
// by TakePages. The list include information about uncommitted pages and markers.
// Every splice list is expected to go through the following series of states:
// 1. The splice list is created.
// 2. Pages are added to the splice list.
// 3. The list is `Finalize`d, meaning that it can no longer be modified by `Append`.
// 4. Pages are then `Pop`d from the list. Once all the pages are popped, the list is considered
//    "processed".
// 5. The list is then considered `Processed` and can be destroyed.
class VmPageSpliceList final {
 public:
  VmPageSpliceList();
  VmPageSpliceList(uint64_t offset, uint64_t length, uint64_t list_skew);
  VmPageSpliceList(VmPageSpliceList&& other);
  VmPageSpliceList& operator=(VmPageSpliceList&& other_tree);
  ~VmPageSpliceList();

  // For use by PhysicalPageProvider.  The user-pager path doesn't use this. This returns a
  // finalized list.
  static VmPageSpliceList CreateFromPageList(uint64_t offset, uint64_t length, list_node* pages);

  // Pops the next page off of the splice list. It is invalid to pop a page from a non-finalized
  // splice list.
  VmPageOrMarker Pop();

  // Peeks at the head of the splice list and returns a non-null VmPageOrMarkerRef pointing to it
  // if and only if it is a reference. It is invalid to peek at a non-finalized splice list.
  VmPageOrMarkerRef PeekReference();

  // Appends `content` to the end of the splice list.
  // The splice list takes ownership of `content` after this call.
  // Note that this method does not work when raw_pages_ is in use.
  // It is invalid to append to a finalized splice list.
  zx_status_t Append(VmPageOrMarker content);

  // Returns true after the whole collection has been processed by Pop.
  bool IsProcessed() const { return pos_ >= length_; }

  // Returns true if this list is empty.
  bool IsEmpty() const;

  // Marks the list as finalized.
  // See the comment at `VmPageSpliceList`'s declaration for more info on what this means and when
  // to call it. Note that it is invalid to call `Finalize` twice on the same list.
  void Finalize();

  // Returns true if the splice list is finalized.
  // See the comment at `VmPageSpliceList`'s declaration for more info on what this means.
  bool IsFinalized() const { return finalized_; }

  // Returns the current position in the list.
  uint64_t Position() const { return pos_; }

  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(VmPageSpliceList);

 private:
  void FreeAllPages();

  uint64_t offset_;
  uint64_t length_;
  uint64_t pos_ = 0;
  uint64_t list_skew_ = 0;
  bool finalized_ = false;

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

  // walk the page tree, calling |per_page_func| on every page/marker/interval that fulfills
  // (returns true) the |compare_func|. Also call |contiguous_run_func| on every contiguous range of
  // such pages/markers/intervals encountered, whose signature is:
  // zx_status_t contiguous_run_func(uint64_t start, uint64_t end, bool is_interval)
  //
  // Intervals are treated as distinct contiguous runs, i.e. they won't be merged into a contiguous
  // run of pages/markers for invocation of |contiguous_run_func|. For intervals,
  // |contiguous_run_func| will be called with |is_interval| set to true; for other page types it
  // will be false. Additionally, the entire interval should fulfill |compare_func| for
  // |contiguous_run_func| to be called on the portion that falls in [start_offset, end_offset).
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

  // Similar to `LookupMutable` but returns a VMPLCursor that allows for iterating over any
  // contiguous slots from the provided offset.
  VMPLCursor LookupMutableCursor(uint64_t offset);

  // The interval handling flag to be used by LookupOrAllocate. See comments near LookupOrAllocate.
  enum class IntervalHandling : uint8_t {
    NoIntervals,
    CheckForInterval,
    SplitInterval,
  };
  // Similar to `Lookup` but only returns `nullptr` if a slot cannot be allocated either due to out
  // of memory, due to offset being invalid, or |interval_handling| not allowing for a slot to be
  // safely returned.
  //
  // The returned slot, if not a `nullptr`, may generally be freely manipulated with the exception
  // that if it started !Empty, then it is an error to set it to Empty. In this case the
  // `RemovePage` method must be used.
  //
  // If the returned slot started Empty, as it not made !Empty, then the slot must be returned with
  // ReturnEmptySlot, to ensure no empty nodes are retained.
  //
  // The bool in the ktl::pair returns whether the offset falls inside a sparse interval. And
  // whether a valid VmPageOrMarker* is returned in the ktl::pair depends on the specified
  // |interval_handling|.
  //  - NoIntervals: The page list does not contain any intervals, so there is no special handling
  //  to check for or split intervals. In other words, each slot in the page list can be manipulated
  //  independently.
  //  - CheckForIntervals: The page list can contain intervals, and the bool in the returned
  //  ktl::pair indicates whether the offset fell inside an interval. Note that this only checks for
  //  intervals but does not allow manipulating them, so a valid VmPageOrMarker* will be returned
  //  only if the offset can safely be manipulated independently.
  //  - SplitInterval: The page list can contain intervals and we are allowed to split intervals to
  //  return the required slot. The returned VmPageOrMarker* can be manipulated freely. (See
  //  comments near LookupOrAllocateCheckForInterval for an explanation of how splitting works.)
  ktl::pair<VmPageOrMarker*, bool> LookupOrAllocate(uint64_t offset,
                                                    IntervalHandling interval_handling) {
    switch (interval_handling) {
      case IntervalHandling::NoIntervals:
        // The page list does not expect any intervals. Short circuit any checks for intervals.
        return {LookupOrAllocateInternal(offset), false};
      case IntervalHandling::CheckForInterval:
        // Check for intervals but do not allow splitting them.
        return LookupOrAllocateCheckForInterval(offset, false);
      case IntervalHandling::SplitInterval:
        // Check for intervals and also split them.
        return LookupOrAllocateCheckForInterval(offset, true);
    }
    return {nullptr, false};
  }

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
  zx_status_t RemovePages(T per_page_fn, uint64_t start_offset, uint64_t end_offset) {
    return ForEveryPageInRange<VmPageOrMarker*, NodeCheck::CleanupEmpty>(this, per_page_fn,
                                                                         start_offset, end_offset);
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
      fit::inline_function<void(VmPageOrMarker&& p, uint64_t offset), 3 * sizeof(void*)> release_fn,
      fit::inline_function<void(VmPageOrMarker* p, uint64_t offset)> migrate_fn);

  // Merges this pages in |this| onto |other|.
  //
  // For every page (or marker) in |this|, checks the same offset in |other|. If there is no
  // page or marker, then it inserts the page into |other|. Otherwise, it releases the page (or
  // marker) and gives ownership to |release_fn|.
  //
  // **NOTE** unlike MergeFrom, |this| will be empty at the end of this method.
  void MergeOnto(VmPageList& other,
                 fit::inline_function<void(VmPageOrMarker&& p, uint64_t offset)> release_fn);

  // Takes the pages, references and markers in the range [offset, length) out of this page list.
  // This method calls `Finalize` on the splice list prior to returning it, meaning that no more
  // pages, references, or markers can be added to it.
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
                              VmPageOrMarker::IntervalDirtyState dirty_state) {
    return AddZeroIntervalInternal(start_offset, end_offset, dirty_state, 0);
  }

  // Populates individual interval slots in the range [start_offset, end_offset) that falls inside a
  // sparse interval. The intent of this function is to allow the caller to prepare the range for
  // overwriting (replacing with pages) by populating the required slots upfront, so that slot
  // lookup does not fail after this call. Essentially simulates interval splits
  // (LookupOrAllocateCheckForInterval) for every offset in the specified range, but does so more
  // efficiently, instead of having to search the tree repeatedly for every single offset.
  zx_status_t PopulateSlotsInInterval(uint64_t start_offset, uint64_t end_offset);

  // Helper to return an unused interval slot so that it can be merged back into the interval it was
  // populated/split from.
  void ReturnIntervalSlot(uint64_t offset);

  // Clips an interval from the start by len, i.e. moves the start from interval_start to
  // interval_start + len. The total length of the interval must be larger than len.
  zx_status_t ClipIntervalStart(uint64_t interval_start, uint64_t len);

  // Clips an interval from the end by len, i.e. moves the end from interval_end to
  // interval_end - len. The total length of the interval must be larger than len.
  zx_status_t ClipIntervalEnd(uint64_t interval_end, uint64_t len);

  // Returns true if the specified offset falls in a sparse zero interval.
  bool IsOffsetInZeroInterval(uint64_t offset) const;

  // Replace an existing page at offset with a zero interval, and return the released page. The
  // caller takes ownership of the released page and is responsible for freeing it.
  vm_page_t* ReplacePageWithZeroInterval(uint64_t offset,
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

  // Internal helper for AddZeroInterval.
  // |replace_existing_slot| can optionally be set to true if a zero interval spanning a single page
  // is being added, and the slot at that offset is already populated (but Empty) and can be reused.
  zx_status_t AddZeroIntervalInternal(uint64_t start_offset, uint64_t end_offset,
                                      VmPageOrMarker::IntervalDirtyState dirty_state,
                                      uint64_t awaiting_clean_len,
                                      bool replace_existing_slot = false);

  // Internal helper for LookupOrAllocate.
  VmPageOrMarker* LookupOrAllocateInternal(uint64_t offset);

  // Similar to LookupOrAllocateInternal but also checks if offset falls in a sparse page interval,
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
    DEBUG_ASSERT(IS_PAGE_ALIGNED(start_offset));
    DEBUG_ASSERT(IS_PAGE_ALIGNED(end_offset));
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
    auto per_page_wrapper_fn = [&](auto* p, uint64_t off) {
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
          // Start and end sentinel interval types should match. Since we only support zero
          // intervals currently, we can simply check for that.
          DEBUG_ASSERT(p->IsIntervalZero());
          in_interval = true;
        } else if (p->IsIntervalEnd()) {
          // If this is not the first populated slot we encountered, we should have been tracking a
          // valid interval.
          DEBUG_ASSERT(in_interval || expected_next_off == start_offset);
          // Start and end sentinel interval types should match. Since we only support zero
          // intervals currently, we can simply check for that.
          DEBUG_ASSERT(p->IsIntervalZero());
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

  // Internal helpers to return the start (or end) of an interval given the end (or start) along
  // with the corresponding offset.
  ktl::pair<const VmPageOrMarker*, uint64_t> FindIntervalStartForEnd(uint64_t end_offset) const;
  ktl::pair<const VmPageOrMarker*, uint64_t> FindIntervalEndForStart(uint64_t start_offset) const;

  template <typename PTR_TYPE, typename S, typename COMPARE_FUNC, typename PAGE_FUNC,
            typename CONTIGUOUS_RUN_FUNC>
  static zx_status_t ForEveryPageAndContiguousRunInRange(S self, COMPARE_FUNC compare_func,
                                                         PAGE_FUNC per_page_func,
                                                         CONTIGUOUS_RUN_FUNC contiguous_run_func,
                                                         uint64_t start_offset,
                                                         uint64_t end_offset) {
    if (start_offset == end_offset) {
      return ZX_OK;
    }

    // Track contiguous range of pages fulfilling compare_func.
    uint64_t contiguous_run_start = start_offset;
    uint64_t contiguous_run_len = 0;

    // Tracks whether we enter the ForEveryPageAndGap traversal at all.
    bool found_page_or_gap = false;
    // Tracks information if we encounter an interval start, to be used when we encounter the
    // corresponding end.
    struct {
      uint64_t interval_start_offset;
      bool start_compare_status;
      bool started_interval;
    } interval_tracker = {.started_interval = false};

    zx_status_t status = ForEveryPageAndGapInRange<PTR_TYPE>(
        self,
        [&](auto* p, uint64_t off) {
          found_page_or_gap = true;
          zx_status_t st = ZX_ERR_NEXT;
          const bool compare_result = compare_func(p, off);

          // Handle interval types first.
          if (p->IsInterval()) {
            // If we are going to start an interval, end any contiguous run being tracked, and call
            // contiguous_run_func on it. This is because intervals are treated as contiguous ranges
            // distinct from pages or markers. Do this before per_page_func because we don't want to
            // have processed extra pages if contiguous_run_func on the range prior would have
            // failed. Also do this irrespective of whether this interval passes the compare_func or
            // not, since we are processing pages prior to the interval.
            if (p->IsIntervalStart() || p->IsIntervalSlot()) {
              if (contiguous_run_len > 0) {
                st = contiguous_run_func(contiguous_run_start,
                                         contiguous_run_start + contiguous_run_len,
                                         /*is_interval=*/false);
                // Reset contiguous range tracking.
                contiguous_run_len = 0;
                if (st != ZX_ERR_NEXT) {
                  return st;
                }
              }
            }
            DEBUG_ASSERT(contiguous_run_len == 0);

            // Run the per-page function on the interval sentinel first. Then proceed to the more
            // complicated logic for the contiguous function.
            if (compare_result) {
              st = per_page_func(p, off);
              if (st != ZX_ERR_NEXT && st != ZX_ERR_STOP) {
                return st;
              }
            }

            // A slot is a contiguous run of a single page.
            if (p->IsIntervalSlot()) {
              // We should not have been already tracking an interval.
              DEBUG_ASSERT(!interval_tracker.started_interval);
              if (compare_result) {
                return contiguous_run_func(off, off + PAGE_SIZE, /*is_interval=*/true);
              }
              return ZX_ERR_NEXT;
            }

            if (p->IsIntervalStart()) {
              // Start tracking a new run. We should not have been already tracking an interval.
              DEBUG_ASSERT(!interval_tracker.started_interval);
              interval_tracker.started_interval = true;
              interval_tracker.interval_start_offset = off;
              // Stash the comparison result for the interval start.
              interval_tracker.start_compare_status = compare_result;
              return ZX_ERR_NEXT;
            }

            DEBUG_ASSERT(p->IsIntervalEnd());
            // If the interval end does not pass the check, there is nothing more to be done.
            if (!compare_result) {
              interval_tracker.started_interval = false;
              return ZX_ERR_NEXT;
            }

            // If this is the end of an interval, call contiguous_run_func on the interval if the
            // compare_func passes for *both* the start and the end, and proceed.
            // It is possible that we don't have the interval start if we started the traversal
            // partway inside an interval. Find the start and evaluate compare_func on it.
            if (!interval_tracker.started_interval) {
              auto [start, interval_start_offset] = self->FindIntervalStartForEnd(off);
              DEBUG_ASSERT(start);
              DEBUG_ASSERT(start->IsIntervalStart());
              DEBUG_ASSERT(interval_start_offset < start_offset);
              interval_tracker.started_interval = true;
              interval_tracker.start_compare_status = compare_func(start, interval_start_offset);
              // Pretend that the interval begins at start_offset since we're not considering the
              // range before it.
              interval_tracker.interval_start_offset = start_offset;
            }
            DEBUG_ASSERT(interval_tracker.started_interval);
            interval_tracker.started_interval = false;
            if (interval_tracker.start_compare_status) {
              return contiguous_run_func(interval_tracker.interval_start_offset, off + PAGE_SIZE,
                                         /*is_interval=*/true);
            }
            return ZX_ERR_NEXT;
          }

          // Handle any non-interval types.
          DEBUG_ASSERT(!p->IsInterval());
          DEBUG_ASSERT(!interval_tracker.started_interval);

          if (compare_result) {
            st = per_page_func(p, off);
            // Return any errors early before considering this page for contiguous_run_func.
            if (st != ZX_ERR_NEXT && st != ZX_ERR_STOP) {
              // If there was an outstanding contiguous run, process it since it had to have ended
              // before the failing offset.
              if (contiguous_run_len > 0) {
                zx_status_t prev_range_status = contiguous_run_func(
                    contiguous_run_start, contiguous_run_start + contiguous_run_len,
                    /*is_interval=*/false);
                contiguous_run_len = 0;
                // If there was an error encountered, surface that instead of st, as it occurred on
                // a range prior to this offset.
                if (prev_range_status != ZX_ERR_NEXT && prev_range_status != ZX_ERR_STOP) {
                  return prev_range_status;
                }
              }
              return st;
            }

            // Start tracking a contiguous run if none was being tracked.
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
            st =
                contiguous_run_func(contiguous_run_start, contiguous_run_start + contiguous_run_len,
                                    /*is_interval=*/false);
            // Reset contiguous_run_len to zero to track a new range later if required.
            // Do this irrespective of the return status to ensure we don't erroneously have a
            // remaining range to process below after exiting the traversal.
            contiguous_run_len = 0;
          }
          return st;
        },
        [&](uint64_t start, uint64_t end) {
          found_page_or_gap = true;
          // We should not encounter any gaps in the midst of an interval we were tracking.
          DEBUG_ASSERT(!interval_tracker.started_interval);
          zx_status_t st = ZX_ERR_NEXT;
          // We were already tracking a contiguous range when we encountered this gap. Invoke
          // contiguous_run_func on the range so far and start tracking a new one skipping over this
          // gap.
          if (contiguous_run_len > 0) {
            st =
                contiguous_run_func(contiguous_run_start, contiguous_run_start + contiguous_run_len,
                                    /*is_interval=*/false);
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

    // If we did not execute either the per-page or per-gap function, we could only have been inside
    // an interval. In that case, we need to find both the start and the end of this interval and
    // evaluate compare_func on them.
    if (!found_page_or_gap) {
      DEBUG_ASSERT(self->IsOffsetInInterval(start_offset));
      DEBUG_ASSERT(self->IsOffsetInInterval(end_offset - PAGE_SIZE));

      uint64_t interval_end_offset = UINT64_MAX;
      bool end_compare_status = false;
      status = ForEveryPageInRange<PTR_TYPE>(
          self,
          [&](auto* p, uint64_t off) {
            // The first populated slot should be an interval end.
            DEBUG_ASSERT(p->IsIntervalEnd());
            interval_end_offset = off;
            end_compare_status = compare_func(p, off);
            return ZX_ERR_STOP;
          },
          end_offset, VmPageList::MAX_SIZE);
      DEBUG_ASSERT(status == ZX_OK);

      if (end_compare_status) {
        auto [start, interval_start_offset] = self->FindIntervalStartForEnd(interval_end_offset);
        DEBUG_ASSERT(start);
        DEBUG_ASSERT(start->IsIntervalStart());
        DEBUG_ASSERT(interval_start_offset < start_offset);
        if (compare_func(start, interval_start_offset)) {
          status = contiguous_run_func(start_offset, end_offset, /*is_interval=*/true);
          if (status != ZX_ERR_NEXT && status != ZX_ERR_STOP) {
            return status;
          }
        }
      }
      return ZX_OK;
    }

    // Process the last contiguous range if there is one, or an interval that we started tracking
    // but did not end.
    if (contiguous_run_len > 0) {
      status = contiguous_run_func(contiguous_run_start, contiguous_run_start + contiguous_run_len,
                                   /*is_interval=*/false);
      if (status != ZX_ERR_NEXT && status != ZX_ERR_STOP) {
        return status;
      }
    } else if (interval_tracker.started_interval && interval_tracker.start_compare_status) {
      auto [end, interval_end_offset] =
          self->FindIntervalEndForStart(interval_tracker.interval_start_offset);
      DEBUG_ASSERT(end);
      DEBUG_ASSERT(end->IsIntervalEnd());
      if (compare_func(end, interval_end_offset)) {
        status = contiguous_run_func(interval_tracker.interval_start_offset, end_offset,
                                     /*is_interval=*/true);
        if (status != ZX_ERR_NEXT && status != ZX_ERR_STOP) {
          return status;
        }
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
