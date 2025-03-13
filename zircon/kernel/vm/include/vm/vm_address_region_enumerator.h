// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_VM_ADDRESS_REGION_ENUMERATOR_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_VM_ADDRESS_REGION_ENUMERATOR_H_

#include <assert.h>
#include <stdint.h>
#include <zircon/types.h>

#include <ktl/optional.h>
#include <ktl/type_traits.h>
#include <vm/vm_address_region.h>

enum class VmAddressRegionEnumeratorType : bool {
  // Both vmars and mappings will be yielded in calls to |next|. Only vmars that are wholly
  // contained within the requested range will be yielded, and it is considered an error to
  // knowingly attempt an enumeration that partially overlaps any sub vmars.
  VmarsAndMappings,
  // Only mappings will be yielded in calls to |next|, and there are no restrictions on the kind of
  // range that can be specified.
  MappingsOnly,
};
// Helper class for performing enumeration of a VMAR. Although this is intended to be
// internal, it is declared here for exposing to unit tests.
//
// The purpose of having a stateful enumerator is to have the option to not need to hold the aspace
// lock over the entire enumeration, whilst guaranteeing forward progress and termination. If the
// vmar is modified whilst enumeration is paused (due to dropping the lock or otherwise) then it is
// not well defined whether the enumerator will return any new mappings. However, the enumerator
// will never return DEAD mappings, and will not return mappings in ranges it has already
// enumerated.
//
// Except between calls to |pause| and |resume|, the vmar should be considered immutable, and
// sub-vmars and mappings should not be modified.
template <VmAddressRegionEnumeratorType Type>
class VmAddressRegionEnumerator {
 public:
  // This requires the vmar lock to be held over the lifetime of the object, except where explicitly
  // stated otherwise.
  VmAddressRegionEnumerator(VmAddressRegion& vmar, vaddr_t min_addr, vaddr_t max_addr)
      TA_REQ(vmar.lock())
      : min_addr_(min_addr),
        max_addr_(max_addr),
        vmar_(vmar),
        itr_(vmar_.subregions_.IncludeOrHigher(min_addr_)) {
    if constexpr (Type == VmAddressRegionEnumeratorType::VmarsAndMappings) {
      // If enumerating vmars and mappings validate that, unless the range is empty, that any
      // vmar is wholly contained within the range. This will not catch all errors in users
      // potentially providing a range that only partially overlaps a vmar, but should catch
      // most accidental misuses.
      // This can only be tested here, and not when walking, since due to how pausing and
      // resuming works we may end up with a min_addr_ that is partially in a vmar, either
      // because the vmars were modified while paused or because we need to resume at a
      // mapping part way into a vmar.
      ASSERT(!itr_.IsValid() || itr_->is_mapping() ||
             (itr_->base_locked() >= min_addr &&
              itr_->base_locked() + itr_->size_locked() <= max_addr));
    }
  }

  struct NextResult {
    VmAddressRegionOrMapping* region_or_mapping;
    uint depth;
  };

  // Yield the next region or mapping, or a nullopt if enumeration has completed. Regions are
  // yielded in depth-first pre-order. The regions are yielded as raw pointers, which are guaranteed
  // to be valid since the lock is held. It is the callers responsibility to keep these pointers
  // alive, by upgrading to RefPtrs or otherwise, if they want to |pause| and drop the lock.
  ktl::optional<NextResult> next() TA_REQ(vmar_.lock()) {
    ASSERT(!state_.paused_);
    ktl::optional<NextResult> ret = ktl::nullopt;
    while (!ret && itr_.IsValid()) {
      AssertHeld(itr_->lock_ref());
      if (itr_->base_locked() >= max_addr_)
        break;

      auto curr = itr_++;
      AssertHeld(curr->lock_ref());
      DEBUG_ASSERT(curr->IsAliveLocked());
      VmAddressRegion* up = curr->parent_;

      if (VmMapping* mapping = curr->as_vm_mapping_ptr(); mapping) {
        DEBUG_ASSERT(mapping != nullptr);
        AssertHeld(mapping->lock_ref());
        // If the mapping is entirely before |min_addr| or entirely after |max_addr| do not run
        // on_mapping. This can happen when a vmar contains min_addr but has mappings entirely
        // below it, for example.
        if ((mapping->base_locked() < min_addr_ &&
             mapping->base_locked() + mapping->size_locked() <= min_addr_) ||
            mapping->base_locked() > max_addr_) {
          continue;
        }
        ret = NextResult{mapping, depth_};
      } else {
        VmAddressRegion* vmar = curr->as_vm_address_region_ptr();
        DEBUG_ASSERT(vmar != nullptr);
        AssertHeld(vmar->lock_ref());
        // Yield the vmar if its base is greater than or equal to our min. As we only yield vmars if
        // requested, and a condition of it being requested is that iteration not be happening part
        // way into a VMAR, we can simply validate that the vmar base is >= our min.
        if constexpr (Type == VmAddressRegionEnumeratorType::VmarsAndMappings) {
          if (vmar->base() >= min_addr_) {
            ret = NextResult{vmar, depth_};
          }
        }
        if (!vmar->subregions_.IsEmpty()) {
          // If the sub-VMAR is not empty, iterate through its children.
          itr_ = vmar->subregions_.begin();
          depth_++;
          continue;
        }
      }
      if (depth_ > kStartDepth && !itr_.IsValid()) {
        AssertHeld(up->lock_ref());
        // If we are at a depth greater than the minimum, and have reached
        // the end of a sub-VMAR range, we ascend and continue iteration.
        do {
          itr_ = up->subregions_.UpperBound(curr->base_locked());
          if (itr_.IsValid()) {
            break;
          }
          up = up->parent_;
        } while (depth_-- != kStartDepth);
        if (!itr_.IsValid()) {
          // If we have reached the end after ascending all the way up,
          // break out of the loop.
          break;
        }
      }
    }
    return ret;
  }

  // Pause enumeration. Until |resume| is called |next| may not be called, but the vmar lock is
  // permitted to be dropped, and the vmar is permitted to be modified.
  void pause() TA_REQ(vmar_.lock()) {
    ASSERT(!state_.paused_);
    // Save information of the next iteration we should return.
    if (itr_.IsValid()) {
      AssertHeld(itr_->lock_ref());
      // itr_ represents the next item that needs to be checked / yielded after we |resume|. We
      // know that everything up to itr_ has already been yielded, so if |itr_| becomes invalid
      // between now and |resume| we know the following:
      //  1. If itr_ was a mapping then this mapping was deleted and, even if a new mapping has
      //     since replaced it, we are not obligated to return it. As such we can skip it.
      //  2. If itr_ was a mapping then since a mapping cannot cross a vmar boundary, resuming at
      //     the end of the mapping will not result in missing a vmar.
      //  3. If itr_ was a vmar then it being deleted can only happen if the entire subtree was
      //     deleted. Now, similar to a mapping, even if a new subtree was created we are not
      //     obligated to return it and can skip.
      // For these reasons we prepare our |next_offset_| to be the end of the current |itr_|,
      // meaning that if itr_ is deleted the next object to be yielded is whatever starts after it.
      state_.next_offset_ = itr_->base_locked() + itr_->size_locked();
      state_.region_or_mapping_ = itr_.CopyPointer();
    } else {
      state_.next_offset_ = max_addr_;
      state_.region_or_mapping_ = nullptr;
    }
    state_.paused_ = true;
  }

  // Resume enumeration allowing |next| to be called again.
  void resume() TA_REQ(vmar_.lock()) {
    ASSERT(state_.paused_);
    if (state_.region_or_mapping_) {
      AssertHeld(itr_->lock_ref());
      if (!itr_->IsAliveLocked()) {
        // Generate a new iterator that starts at the right offset, but back at the top. The next
        // call to next() will walk back down if necessary to find the next mapping / VMAR.
        min_addr_ = state_.next_offset_;
        itr_ = vmar_.subregions_.IncludeOrHigher(min_addr_);
        depth_ = kStartDepth;
      } else {
        DEBUG_ASSERT(&*itr_ == &*state_.region_or_mapping_);
      }
      // Free the refptr. Note that the actual destructors of VmAddressRegionOrMapping objects
      // themselves do very little, so we are safe to potential invoke the destructor here.
      state_.region_or_mapping_.reset();
    } else {
      // There was no refptr, meaning itr_ was already not valid, and should still not be valid.
      ASSERT(!itr_.IsValid());
    }
    state_.paused_ = false;
  }

  // Expose our backing lock for annotation purposes.
  Lock<CriticalMutex>& lock_ref() const TA_RET_CAP(vmar_.lock_ref()) { return vmar_.lock_ref(); }

 private:
  struct PauseState {
    bool paused_ = false;
    vaddr_t next_offset_ = 0;
    fbl::RefPtr<VmAddressRegionOrMapping> region_or_mapping_;
  };

  PauseState state_;
  vaddr_t min_addr_;
  const vaddr_t max_addr_;
  static constexpr uint kStartDepth = 1;
  uint depth_ = kStartDepth;
  // Root vmar being enumerated.
  VmAddressRegion& vmar_;
  // This iterator represents the object at which |next| should use to find the next item to return.
  // An invalid itr_ therefore represents no next object, and means enumeration has finished.
  RegionList<VmAddressRegionOrMapping>::ChildList::iterator itr_;
};

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_VM_ADDRESS_REGION_ENUMERATOR_H_
