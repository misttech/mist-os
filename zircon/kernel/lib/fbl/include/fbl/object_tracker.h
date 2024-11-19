// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_FBL_INCLUDE_FBL_OBJECT_TRACKER_H_
#define ZIRCON_KERNEL_LIB_FBL_INCLUDE_FBL_OBJECT_TRACKER_H_

#include <align.h>
#include <lib/page_cache.h>
#include <lib/zx/result.h>
#include <pow2.h>
#include <sys/types.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstdio>

#include <arch/defines.h>
#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <fbl/inline_array.h>
#include <fbl/int_set.h>
#include <fbl/intrusive_wavl_tree.h>
#include <fbl/mutex.h>
#include <fbl/vector.h>
#include <kernel/auto_preempt_disabler.h>
#include <kernel/mutex.h>
#include <kernel/spinlock.h>
#include <ktl/algorithm.h>
#include <ktl/array.h>
#include <ktl/limits.h>
#include <ktl/span.h>
#include <ktl/string_view.h>
#include <ktl/type_traits.h>
#include <lockdep/guard.h>

namespace fbl {
// ObjectTracker is a a kernel version of a multithreaded map container + allocator
// specialized for uint32 <--> object. It provides the standard map services
// add, remove, get & enumerate but also the storage services of a pool allocator.
//
// ObjectTracker fills the gap between the existing helpers:
// - allocators cannot enumerate their live objects, the current solution is
//   to use a intrusive linked list to enumerate them which comes with 16 bytes
//   overhead and lookups are O(n).
// - The WAVL tree supports fast lookups and iteration but each object must
//   pay 48+ bytes overhead, so it is only practical if the object weights upwards of
//   200 bytes.
//
//  The API is move-oriented, the typical lifecycle goes:
//  1 - Create object |o| to be tracked
//  2 - Move |o| into the tracker via add(o) which returns its |id|.
//  3 - Retrieve the object reference (via the holder) from get(id)
//  4 - call |o| methods via the holder
//  5 - Drop the holder
//  6 - Either remove the object from tracking via remove(id) or
//      goto step 3.
//
// Using it
//   ObjectTracker takes two class template parameters:
//   A) TrackedType: the class to be stored and tracked, it must implement three
//      methods:
//       1- zx_status on_holder(uint32_t user_tag). Called when a Holder needs to
//          be created. If this method returns an error, the holder will not be
//          created and the error will be returned instead.
//
//       2- uint32_t on_tracked(). Called once the object is being tracked. The
//          returned value is the "user tag" included in the tracking id. See remarks
//          below.
//       3- zx_status_t on_remove(uint32_t user_tag). Called before is removed from
//          tracking. If this method returns an error, the object will not be removed
//          from the container and the error will be returned instead.
//
//   If the user_tag feature is not needed, on_tracked() and on_remove() methods can
//   have a no-op implementations as follows:
//
//     uint32_t on_tracked() {
//       return 0u;
//     }
//
//     zx_status_t on_remove(uint32_t) {
//       return ZX_OK;
//     }
//
//   All the above callbacks are issued with a spinlock acquired that protects the page
//   where the tracked object lives. Therefore, no calls to the tracker should be issued
//   from the callback or calls that acquire regular mutexes. Calls that acquire
//   spinlocks are ok.
//
//   B) HolderType: an object of this class is returned when ObjectTracker::get()
//     is successful. The Holder is created via the HolderType(TrackedType&) or the
//     HolderType(const TrackedType&) ctor. If HolderType keeps a weak reference to the
//     TrackedType then it must collaborate with it to prevent ObjectTracker::remove()
//     from succeeding by returning an error from TrackedType::on_remove().
//
//   The TrackedType has one more requirement: it must have a boring destructor
//   for the particular case when it is called for a moved-from state. In other words
//   assume a TrackedType = TFoo, then
//
//  TFoo destination;
//
//  {
//    // stack allocated storage for |source|.
//    alignas(Foo) char data[sizeof(Foo)];
//    TFoo* source = new (data) TFoo();
//    ......
//    ......
//    destination = std::move(*source);
//    avoid_dtor = true;
//
//    if (!avoid_dtor) {
//      source->~TFoo();
//    }
//  }
//
//  The program above is correct for the TFoo type. Note that if TFoo is not in the
//  moved-from state, the destructor can do all sorts of critical operations, this means
//  the object tracker that fail the ktl::is_trivially_destructible<TrackedType> test.
//
//  Using the user_tag
//
//
//  A user tag is a small value, cookie-like, with as many bits as the template parameter
//  user_tag_bits. It is provided by the tracked object during ObjectTracker::add() via the
//  callback on_tracked() and returned as mixed in the returned id. This tag is given back
//  to the tracked object during the ObjectTracker::get() in the on_holder()
//  callback or during ObjectTracker::remove() in the on_remove() callback.
//
//  The tracked object has full freedom how to interpret the tag, it might require to be
//  provided back the same tag or a different value. The ObjectTracker::encode() and
//  decode() static functions can be used by clients to extract or replace the tag returned
//  by the tracked object, except in one case, during the ObjectTracker destruction
//  or if triggered manually via destroy_all() the on_remove() callback is issued with
//  the special value kTagDestroy, which is hardcoded to be the highest value possible
//  for an unsigned number of size user_tag_bits.

template <typename TrackedType, typename HolderType, size_t user_tag_bits = 4u>
class ObjectTracker {
 public:
  ObjectTracker() = delete;
  ObjectTracker(const ObjectTracker&) = delete;
  ObjectTracker(ObjectTracker&&) = delete;

  explicit ObjectTracker(page_cache::PageCache* page_cache) : page_cache_(page_cache) {}
  ~ObjectTracker() { destroy_all(); }

  // Reserved value for the user_tag. More details above.
  static constexpr uint32_t kTagDestroy = (1u << user_tag_bits) - 1u;

  // Make a holder from a tracked object pointed by |id|. The only error possible is
  // ZX_ERR_NOT_FOUND. Otherwise an HolderType is returned.
  zx::result<HolderType> get(uint32_t id);

  // Batch-get. Same considerations as the single-issue get apply.
  zx_status_t get(ktl::span<const uint32_t> ids, ktl::span<HolderType> holders);

  // Move the |object| into the tracker. The only error results are the ones returned
  // by PageCache, which are memory related: ZX_ERR_OUT_OF_MEMORY or ZX_ERR_OUT_OF_RANGE
  // if we have maxed out the number of pages that can be allocated. Otherwise the result is
  // the id of the object.
  //
  // Because of the locking scheme, It is possible for another thread guessing ids to issue a
  // successful get() and operate on the |object| before this method returns. In other words
  // the object moved (or copied) from |object| can start receiving method calls before this
  // method returns but after it has been fully created.
  zx::result<uint32_t> add(TrackedType object);

  // Move the |objects| into the tracker returning their assigned ids in |ids|.
  // The possible errors are the same as the single item Add(). In the case of an error
  // some elements might have been inserted already. If the user wants to recover from this
  // it is recommended that the output |ids| vector should contain zeros so if desired the
  // newly inserted objects might be removed.
  zx_status_t add(ktl::span<TrackedType> objects, ktl::span<uint32_t> ids);

  // Remove the (previously added) object from tracking. There are two
  // main error conditions:
  // - ZX_ERR_NOT_FOUND: the |id| does not correspond to an tracked object.
  // - The object does not want to be removed, that is, on_remove() returned an
  //   error which is returned here. It might be that an alive HolderType object
  //   is preventing the removal.
  zx::result<TrackedType> remove(uint32_t id);

  // Remove the previously added objects in |ids| and returns them in |objects|.
  // The input array is mutated.
  //
  // In the case of failure, for example one of the ids is invalid, the rest are still
  // processed. This means that there are two outcomes:
  // - success: all objects are in the output vector
  // - failure: all objects that could be removed are destroyed.
  zx_status_t remove(ktl::span<const uint32_t> ids, ktl::span<TrackedType> objects);

  // Destroy all the tracked objects.
  // This method is to be called when by construction you know that there are no outstanding
  // HolderType objects. It will remove (or assert if it can't) all the tracked objects and
  // call the destructors to each one.
  //
  // Also beware that it will hold internal spinlocks so you want to make sure other threads
  // are not going to be adding or removing objects to the tracker concurrently.
  size_t destroy_all();

  // Returns how many objects are being tracked. Only use this method
  // for testing.
  size_t count_slow();

  // Returns how many pages are being used. You can use this method for
  // testing or for in-the-field memory diagnostics.
  size_t page_count();

  // Decodes an |id| returned by add() so the client can store the
  // id differently. For example if the number of objects is limited
  // by other means, an id can be compressed into 16 bits.
  static auto decode(uint32_t id);

  // Encodes the tuple returned by decode() back into a tracker id. Only
  // needed if you use decode() and store the id in an interesting way.
  static uint32_t encode(uint32_t page_id, uint32_t index, uint32_t user_tag);

  // Like encode but for a pre-combined index plus tag.
  static uint32_t encode(uint32_t page_id, uint32_t index_and_tag);

  // How many objects fit in a page. Only interesting if you are also using
  // encode() or decode(), or deeply care about efficient storage.
  static constexpr uint32_t obs_per_page() { return objects_per_page; }

  // Ids are uint32_t and must fit the index, tag and, page id which limits
  // the number of pages.
  static constexpr uint32_t max_pages() { return (1u << (32u - page_id_shift)) - 1; }

 private:
  // Capacity calculations.
  //
  // The calculation of how many objects to fit snugly on a page is not trivial. The theory
  // is that there is a fixed overhead of 56 bytes (see the static_assert in PageObject ctor)
  // and a variable overhead from the |node_tracker_| that results on the following equation:
  //
  //   let |count| be the number of objects, |fo| the fixed overhead per page,
  //   |ts| the tracked-type object size and |al| the alignment overhead.
  //
  // Then
  //   PAGE_SIZE = (count * ts) + (count/8) + fo + al
  //
  // Note that
  //   1- |al| depends on the alignment of the tracked-type
  //   2- the bitmap storage (count/8) has a minimum size of 8 bytes.
  //
  // We can approximate a solution if we assume that |al| is approximately zero and that we
  // constrain the solution to have count > 64:
  //
  //   PAGE_SIZE - fo = count (ts + 1/8)  thus
  //
  //   count = (PAGE_SIZE - fo)/(ts + 1/8)
  //
  // With |fo| = 44 bytes to 52 bytes (depends of the build type) we get
  // count = (8 * (PAGE_SIZE - 52))/(8 * ts + 1) but only valid over the reals.
  //
  // Over the integers there are no exact solutions but the following seems to work well.
  //
  //   count = 8 * (PAGE_SIZE - 52))/(8 * ts + 1) - 1
  //
  // There are two further static_assert(s) in PageObject ctor to ensure this approximation
  // does not overshoot or undershoot. If the asserts fire when a better approximation
  // needs to be found.

  static constexpr uint32_t numerator = 8u * (PAGE_SIZE - 52u);
  static constexpr size_t objects_per_page = (numerator / ((8u * sizeof(TrackedType)) + 1u)) - 1;

  // Locking scheme.
  //
  // The locking for this data structure follows the hand-over-hand pattern with two locks; the
  // top lock and the bottom lock.
  // The top lock is the |tracker_lock_| and protects all the members of the ObjectTracker object
  // and the bottom lock is the PageObject's |page_lock_|. There is a single top lock and there are
  // as many bottom locks as there are pages. Any thread doing any operation is is either:
  //
  //   1- Holding the top lock
  //   2- Holding the top lock and a single bottom lock
  //   3- Holding a single bottom lock
  //
  //  Moreover, steps #2 and #3 are always issued against the same bottom lock (the same
  //  PageObject). Lets review each operation and how the 3 states map to them
  //
  //  a) get(id) --> HolderType  (#1>#2>#3)
  //     Acquire the top lock, find the PageObject (which has the bottom lock)
  //       Acquire the bottom lock + Release the top lock
  //         Lookup (work)
  //       Release the bottom lock
  //
  //  b) add(TrackedType) --> id
  //     case 1: has |top_page| and there is space there. (#1>#2>#3)
  //       Acquire the top lock
  //         Acquire the bottom lock of |top_page| + Release the top lock
  //           Add item (work)
  //         Release the bottom lock
  //     case 2: no |top_page| or no space in there. (#1)
  //       Allocate page
  //       Acquire the top lock
  //         Add page to tree (work)
  //       Release top lock
  //       Go to case 1
  //
  //  c) remove(id) --> TrackedType
  //     phase 1: remove item  (#1, #2, #3)
  //       Acquire the top lock, find the PageObject (which has the bottom lock)
  //         Acquire the bottom lock + Release the top lock
  //           remove item (work)
  //         Release the bottom lock
  //     phase 2: maybe free mem or replace |top_page| (#1>#2)
  //       Acquire the top lock
  //         Acquire the bottom lock (of page of phase 1)
  //           read element count
  //         Release bottom lock
  //         maybe replace |top_page| with this page or, maybe remove page from tree (work)
  //       Release top lock.
  //

  // Object ids are packed as follows:
  // msb |---------------------------------| lsb
  //     | page-id | user_tag | page-index |
  //     |                   *-------------- user_tag_shift
  //     |        *------------------------- page_id_shift
  //
  static constexpr uint32_t user_tag_shift = log2_ceil(objects_per_page);
  static constexpr uint32_t index_mask = (1 << user_tag_shift) - 1;
  static constexpr uint32_t page_id_shift = user_tag_shift + user_tag_bits;
  static constexpr uint32_t user_tag_mask = (1u << user_tag_bits) - 1;

  // The PageObject represents a memory page which holds at most |num_objects|
  // of TrackedType, along with the control structures and the access spinlock.
  // In the code below we use the following terms:
  // - index : points to an entry on the array of TrackedTypes.
  // - id or ids : a full object tracker id or ids, the bottom part is an index
  //     and the top part is a page id.
  // - smap: a sorted map, which maps ids to ids that belong to this page.
  //     for example ids[0] might not belong to this page but ids[smap[0]] does.
  template <size_t num_objects>
  struct PageObject {
    using Tracker = ObjectTracker<TrackedType, HolderType, user_tag_bits>;

    explicit PageObject(uint32_t page_id) : page_id_(page_id) {
      // This ctor must be kept cheap because we call it under the tracker lock.
      static_assert(sizeof(*this) <= PAGE_SIZE);
      // Assert that the wasted bytes are no more than two tracked objects wort
      // of bytes. Notionally it should be strictly less than two but differences
      // between build types affect this calculation.
      static_assert(sizeof(*this) >= (PAGE_SIZE - (2 * sizeof(TrackedType))));
      // The fixed overhead varies per build type, 44 bytes to 52.
    }
    // Note: this destructor is not called during destroy_all() only
    // during remove() so the tracker returns memory to the page-cache
    // which in turn might return it to the system.
    ~PageObject() {
      ASSERT(node_tracker_.free_count() == num_objects);
      ASSERT(!wavl_node_state_.InContainer());
    }
    // The page id is the WAVL key and it is never zero.
    uint32_t page_id() const { return page_id_; }

    Lock<SpinLock>* lock() const TA_RET_CAP(page_lock_) { return &page_lock_; }

    // Make a holder from a valid |index| that points to a tracked object.
    zx::result<HolderType> get_locked(uint32_t index, uint32_t user_tag) TA_REQ(page_lock_) {
      if (!node_tracker_.exists(index)) {
        return zx::error(ZX_ERR_NOT_FOUND);
      }
      auto tracked = object_at(index);
      if (auto st = tracked->on_holder(user_tag); st != ZX_OK) {
        return zx::error(st);
      }
      return zx::ok(HolderType{*tracked});
    }

    // Batch get. Note that the input |ids| contain the page, which we must strip.
    zx_status_t get_locked(ktl::span<const uint32_t> indirect_map, ktl::span<const uint32_t> ids,
                           ktl::span<HolderType> holders) TA_REQ(page_lock_) {
      // Loop over the indirect map if exists or over the ids directly if not.

      if (indirect_map.empty()) {
        size_t ix = 0;
        for (auto id : ids) {
          auto dec = Tracker::decode(id);
          if (!node_tracker_.exists(dec.index)) {
            return ZX_ERR_NOT_FOUND;
          }
          auto tracked = object_at(dec.index);
          if (auto st = tracked->on_holder(dec.user_tag); st != ZX_OK) {
            return st;
          }
          holders[ix] = ktl::move(HolderType{*tracked});
          ix += 1;
        }
      } else {
        for (auto si : indirect_map) {
          auto dec = Tracker::decode(ids[si]);
          if (!node_tracker_.exists(dec.index)) {
            return ZX_ERR_NOT_FOUND;
          }
          auto tracked = object_at(dec.index);
          if (auto st = tracked->on_holder(dec.user_tag); st != ZX_OK) {
            return st;
          }
          holders[si] = ktl::move(HolderType{*tracked});
        }
      }
      return ZX_OK;
    }

    // Batch-add. Stops at the first error.
    zx_status_t add_locked(ktl::span<TrackedType> objects, ktl::span<uint32_t> ids)
        TA_REQ(page_lock_) {
      zx_status_t status = ZX_OK;
      for (size_t ix = 0; ix != objects.size(); ix++) {
        auto res = add_locked(ktl::move(objects[ix]));
        if (res.is_error()) {
          return res.error_value();
        }
        ids[ix] = Tracker::encode(page_id_, res.value());
      }
      return status;
    }

    zx::result<uint32_t> add_locked(TrackedType&& object) TA_REQ(page_lock_) {
      auto index = node_tracker_.allocate_new();
      if (index.is_error()) {
        return index.take_error();
      }
      auto oit = new (object_at(index.value())) TrackedType(ktl::move(object));
      uint32_t user_tag = oit->on_tracked();
      if (user_tag != 0u) {
        index.value() |= (user_tag << user_tag_shift);
      }
      return zx::ok(index.value());
    }

    struct RemoveResult {
      TrackedType object;
      uint32_t object_count;
    };

    // Batch remove. Note |ids| contain the page, not just the index so we need
    // to strip it.
    zx_status_t remove_locked(ktl::span<const uint32_t> indirect_map, ktl::span<const uint32_t> ids,
                              ktl::span<TrackedType> objects) TA_REQ(page_lock_) {
      const auto write = !objects.empty();

      // Loop over all input indexes via the indirect map if present or directly if not.
      // Ignore errors, but return the last one.

      zx_status_t status = ZX_OK;
      if (indirect_map.empty()) {
        size_t ix = 0;
        for (auto id : ids) {
          auto dec = Tracker::decode(id);
          auto res = remove_locked(dec.index, dec.user_tag);
          if (res.is_error()) {
            status = res.error_value();
          } else if (write) {
            objects[ix] = ktl::move(res.value().object);
          }
          ix += 1;
        }
      } else {
        for (auto si : indirect_map) {
          auto dec = Tracker::decode(ids[si]);
          auto res = remove_locked(dec.index, dec.user_tag);
          if (res.is_error()) {
            status = res.error_value();
          } else if (write) {
            objects[si] = ktl::move(res.value().object);
          }
        }
      }
      return status;
    }

    zx::result<RemoveResult> remove_locked(uint32_t index, uint32_t user_tag) TA_REQ(page_lock_) {
      if (!node_tracker_.exists(index)) {
        return zx::error(ZX_ERR_NOT_FOUND);
      }
      auto object = object_at(index);
      if (auto status = object->on_remove(user_tag); status != ZX_OK) {
        return zx::error(status);
      }
      TrackedType result{ktl::move(*object)};
      node_tracker_.remove(index);
      uint32_t count = num_objects - node_tracker_.free_count();
      return zx::ok(RemoveResult{.object = ktl::move(result), .object_count = count});
    }

    uint32_t destroy_all() {
      uint32_t count = 0;
      Guard<SpinLock, IrqSave> guard{&page_lock_};
      node_tracker_.remove_all_fn([this, &count, &guard](size_t index) {
        auto object = object_at(index);
        ASSERT(object->on_remove(kTagDestroy) == ZX_OK);
        guard.CallUnlocked([object] { object->~TrackedType(); });
        count++;
      });
      // TODO(cpu): this can be optimized by a local id array so we don't
      // have to reacquire the spinlock n times but instead n/16.
      return count;
    }

    uint32_t free_count_locking() {
      Guard<SpinLock, IrqSave> guard{&page_lock_};
      return node_tracker_.free_count();
    }

    uint32_t object_count_locking() { return num_objects - free_count_locking(); }

    TrackedType* object_at(size_t index) {
      return reinterpret_cast<TrackedType*>(&storage_[index * sizeof(TrackedType)]);
    }

    WAVLTreeNodeState<PageObject*, NodeOptions::AllowClearUnsafe> wavl_node_state_;
    const uint32_t page_id_;
    mutable DECLARE_SPINLOCK(PageObject) page_lock_;
    // The |node_tracker_| must be at this spot for the size calculations to work.
    IntSet<num_objects> node_tracker_ TA_GUARDED(page_lock_);
    alignas(TrackedType) uint8_t storage_[sizeof(TrackedType) * num_objects];
  };

  // Get or Remove items depending on the output type |T|:
  // - output type TrackedType : remove() -> span<TrackedType>
  // - output type HolderType : get() -> span<HolderType>
  template <typename Tout>
  zx_status_t get_or_remove_batch(ktl::span<const uint32_t> ids, ktl::span<Tout> objects);

  // Adds a new top_page, maybe. More than one thread could be attempting the same.
  // |raw_page| can be nullptr which means the function will take care of allocation.
  zx_status_t add_top_page(void* raw_page = nullptr);

  // Called when we discover a page which is empty or mostly empty, we either free it
  // or we swap it for the top page or we free the top page.
  void optimize_pages(uint32_t page_id_hint, size_t page_count);

  // Takes a PageList head node and pops it from the list and initializes it,
  // returning its physmap address.
  void* pop_and_init_phys_page(page_cache::PageCache::PageList* page_list);

  // Takes a previously created page object |po| destroys it and returns the memory
  // to the page cache.
  void free_phys_page(void* po);

  using PageObj = PageObject<objects_per_page>;

  struct WAVLTraits {
    static uint32_t GetKey(const PageObj& po) { return po.page_id(); }
    static bool LessThan(uint32_t a, uint32_t b) { return a < b; }
    static bool EqualTo(uint32_t a, uint32_t b) { return a == b; }
  };

  page_cache::PageCache* const page_cache_;
  DECLARE_MUTEX(ObjectTracker) tracker_lock_;
  uint32_t next_key_ TA_GUARDED(tracker_lock_) = 0;
  PageObj* top_page_ TA_GUARDED(tracker_lock_) = nullptr;
  WAVLTree<uint32_t, PageObj*, WAVLTraits> page_map_ TA_GUARDED(tracker_lock_);
};

template <typename TrackedType, typename HolderType, size_t utb>
zx::result<HolderType> ObjectTracker<TrackedType, HolderType, utb>::get(uint32_t id) {
  auto dec = decode(id);
  AutoPreemptDisabler apd;
  Guard<Mutex> tracker_guard{&tracker_lock_};
  auto page = page_map_.find(dec.page_id);
  if (page == page_map_.end()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  Guard<SpinLock, IrqSave> page_guard{page->lock()};
  tracker_guard.Release();
  return page->get_locked(dec.index, dec.user_tag);
}

template <typename TrackedType, typename HolderType, size_t utb>
zx_status_t ObjectTracker<TrackedType, HolderType, utb>::get(ktl::span<const uint32_t> ids,
                                                             ktl::span<HolderType> holders) {
  if (ids.size() != holders.size()) {
    return ZX_ERR_INVALID_ARGS;
  }
  // Handle cases of size 0 and 1.
  if (holders.size() == 0u) {
    return ZX_OK;
  }
  if (ids.size() == 1u) {
    auto res = get(ids[0]);
    if (res.is_error()) {
      return res.error_value();
    };
    holders[0] = ktl::move(res.value());
    return ZX_OK;
  }
  return get_or_remove_batch(ids, holders);
}

template <typename TrackedType, typename HolderType, size_t utb>
zx::result<uint32_t> ObjectTracker<TrackedType, HolderType, utb>::add(TrackedType object) {
  {
    while (true) {
      {
        AutoPreemptDisabler apd;
        Guard<Mutex> tracker_guard{&tracker_lock_};
        if ((top_page_ != nullptr) && (top_page_->free_count_locking() > 0u)) {
          auto page_id = top_page_->page_id();
          PageObj* top_page = top_page_;
          Guard<SpinLock, IrqSave> page_guard{top_page->lock()};
          tracker_guard.Release();
          auto result = top_page->add_locked(ktl::move(object));
          if (result.is_error()) {
            return result.take_error();
          }
          return zx::ok(encode(page_id, result.value()));
        }
      }
      // No top_page or top_page full, request one from the cache.
      auto st = add_top_page();
      if (st != ZX_OK) {
        return zx::error(st);
      }
      // It is possible to loop more than once since page
      // allocation is not done under the lock. Other threads
      // can take the fresh top_page and fill it completely.
    }
  }
}

template <typename TrackedType, typename HolderType, size_t utb>
zx_status_t ObjectTracker<TrackedType, HolderType, utb>::add(ktl::span<TrackedType> objects,
                                                             ktl::span<uint32_t> ids) {
  if (objects.size() != ids.size()) {
    return ZX_ERR_INVALID_ARGS;
  }
  // Handle cases of size 0 and 1.
  if (objects.size() == 0) {
    return ZX_OK;
  }
  if (objects.size() == 1u) {
    auto res = add(ktl::move(objects[0]));
    if (res.is_error()) {
      return res.error_value();
    }
    ids[0] = res.value();
    return ZX_OK;
  }
  // Note to future enhancers of this code. It is possible to make this function
  // fully transactional if PageCache::Allocate() is issued eagerly here. There
  // are (workable) complexities since objects.size() / objects_per_page can be 0
  // and we haven't checked the state of the top_page yet.
  page_cache::PageCache::PageList page_list;

  size_t start = 0;
  size_t left = objects.size();

  while (true) {
    {
      AutoPreemptDisabler apd;
      Guard<Mutex> tracker_guard{&tracker_lock_};
      if (top_page_ != nullptr) {
        auto free = top_page_->free_count_locking();
        // Subtle code warning! since we have |tracker_lock_| and we must have obtained (and
        // released) the top page lock via free_count(), we know we are the only thread operating
        // om the top page even if we are not holding its lock.
        if (free > 0u) {
          auto count = ktl::min(static_cast<size_t>(free), left);
          PageObj* top_page = top_page_;
          Guard<SpinLock, IrqSave> page_guard{top_page->lock()};
          tracker_guard.Release();
          auto st = top_page->add_locked(objects.subspan(start, count), ids.subspan(start, count));
          // Outside any locks here.
          if (st != ZX_OK) {
            return st;
          }
          left -= count;
          start += count;
          if (left == 0u) {
            return ZX_OK;
          }
          // More objects to add. It also means the top page is likely full.
        }
        // No more space in the top page.
      }
    }

    if (page_list.is_empty()) {
      // Time to allocate all the pages we are going to need. Note that we do this outside any
      // locks so it is possible (but rare) for other threads to use up the pages we make and
      // thus might have to enter here more than once.
      auto pages_needed = ktl::max(left / objects_per_page, 1ul);
      auto raw_pages = page_cache_->Allocate(pages_needed);
      if (raw_pages.is_error()) {
        return raw_pages.error_value();
      }
      page_list = ktl::move(raw_pages.value().page_list);
    }
    // Load up one page and try again.
    auto st = add_top_page(pop_and_init_phys_page(&page_list));
    if (st != ZX_OK) {
      return st;
    }
  }  // while (true).
}

template <typename TrackedType, typename HolderType, size_t utb>
zx::result<TrackedType> ObjectTracker<TrackedType, HolderType, utb>::remove(uint32_t id) {
  auto dec = decode(id);
  AutoPreemptDisabler apd;
  Guard<Mutex> tracker_guard{&tracker_lock_};
  auto page = page_map_.find(dec.page_id);
  if (page == page_map_.end()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  size_t page_count = page_map_.size();
  Guard<SpinLock, IrqSave> page_guard{page->lock()};
  tracker_guard.Release();
  auto result = page->remove_locked(dec.index, dec.user_tag);
  if (result.is_error()) {
    return result.take_error();
  }

  if (result.value().object_count < objects_per_page / 3) {
    optimize_pages(dec.page_id, page_count);
  }

  return zx::ok(ktl::move(result.value().object));
}

template <typename TrackedType, typename HolderType, size_t utb>
zx_status_t ObjectTracker<TrackedType, HolderType, utb>::remove(ktl::span<const uint32_t> ids,
                                                                ktl::span<TrackedType> objects) {
  if (ids.size() != objects.size()) {
    return ZX_ERR_INVALID_ARGS;
  }
  // Handle cases of size 0 and 1.
  if (objects.size() == 0u) {
    return ZX_OK;
  }
  if (ids.size() == 1u) {
    auto res = remove(ids[0]);
    if (res.is_error()) {
      return res.error_value();
    }
    objects[0] = ktl::move(res.value());
    return ZX_OK;
  }
  return get_or_remove_batch(ids, objects);
}

template <typename TrackedType, typename HolderType, size_t utb>
size_t ObjectTracker<TrackedType, HolderType, utb>::destroy_all() {
  size_t count = 0;
  page_cache::PageCache::PageList list;

  Guard<Mutex> tracker_guard{&tracker_lock_};

  for (auto& page : page_map_) {
    count += page.destroy_all();
    auto paddr = physmap_to_paddr(&page);
    auto pa = paddr_to_vm_page(paddr);
    list_add_tail(&list, &pa->queue_node);
  }
  // We can do the "fast" clear because the pages are stored unmanaged
  // in the WAVL tree and since the page destructors are trivial, it's ok
  // not to call their destructors.

  page_map_.clear_unsafe();
  page_cache_->Free(ktl::move(list));

  top_page_ = nullptr;
  next_key_ = 0;
  return count;
}

template <typename TrackedType, typename HolderType, size_t utb>
size_t ObjectTracker<TrackedType, HolderType, utb>::count_slow() {
  size_t count = 0;
  Guard<Mutex> tracker_guard{&tracker_lock_};
  for (auto& page : page_map_) {
    count += page.object_count_locking();
  }
  return count;
}

template <typename TrackedType, typename HolderType, size_t utb>
size_t ObjectTracker<TrackedType, HolderType, utb>::page_count() {
  Guard<Mutex> tracker_guard{&tracker_lock_};
  return page_map_.size();
}

template <typename TrackedType, typename HolderType, size_t utb>
template <typename Tout>
zx_status_t ObjectTracker<TrackedType, HolderType, utb>::get_or_remove_batch(
    ktl::span<const uint32_t> ids, ktl::span<Tout> objects) {
  // Single page case. This is an optimization.
  {
    AutoPreemptDisabler apd;
    Guard<Mutex> tracker_guard{&tracker_lock_};
    if (page_map_.size() == 1u) {
      PageObj* top_page = top_page_;
      Guard<SpinLock, IrqSave> page_guard{top_page->lock()};
      tracker_guard.Release();
      if constexpr (ktl::is_same_v<TrackedType, Tout>) {
        return top_page->remove_locked(ktl::span<uint32_t>(), ids, objects);
      } else if constexpr (ktl::is_same_v<HolderType, Tout>) {
        return top_page->get_locked(ktl::span<uint32_t>(), ids, objects);
      }
    }
  }

  // Multi page case.

  // We need to sort |ids| to operate batch-per-age but we need to remember the original
  // order, so we sort an array of indexes to |ids| instead. It is used to map the unordered
  // input ids to sorted ids:
  // ids[0] = first element, unsorted.
  // ids[smap[0]] = first element in sort order.
  fbl::AllocChecker ac;
  fbl::InlineArray<uint32_t, 16> sorted_map(&ac, ids.size());
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  auto smap = ktl::span(sorted_map.get(), sorted_map.size());
  uint32_t ix = 0u;
  for (auto& c : smap) {
    c = ix++;
  }
  ktl::sort(smap.begin(), smap.end(), [&ids](uint32_t a, uint32_t b) { return ids[a] < ids[b]; });

  size_t start = 0;
  size_t end = 0;
  zx_status_t st = ZX_OK;

  do {
    auto dec_s = decode(ids[smap[start]]);
    while (true) {
      auto dec_e = decode(ids[smap[end]]);
      if (dec_s.page_id != dec_e.page_id) {
        break;
      }
      end += 1u;
      if (end == smap.size()) {
        break;
      }
    }
    // Collected a batch of ids from the same page, attempt to fetch or remove them.
    AutoPreemptDisabler apd;
    Guard<Mutex> tracker_guard{&tracker_lock_};
    auto page = page_map_.find(dec_s.page_id);
    if (page == page_map_.end()) {
      st = ZX_ERR_NOT_FOUND;
    } else {
      auto count = end - start;
      Guard<SpinLock, IrqSave> page_guard{page->lock()};
      tracker_guard.Release();
      if constexpr (ktl::is_same_v<TrackedType, Tout>) {
        auto st2 = page->remove_locked(smap.subspan(start, count), ids, objects);
        // Ignore, but record the first error.
        st = (st != ZX_OK) ? st : st2;
      } else if constexpr (ktl::is_same_v<HolderType, Tout>) {
        st = page->get_locked(smap.subspan(start, count), ids, objects);
        if (st != ZX_OK) {
          return st;
        }
      } else {
        static_assert(false, "unimplemented for this type");
      }
    }
    start = end;
  } while (start != smap.size());

  return st;
}

template <typename TrackedType, typename HolderType, size_t utb>
zx_status_t ObjectTracker<TrackedType, HolderType, utb>::add_top_page(void* raw_page) {
  if (raw_page == nullptr) {
    // Alloc and init a single page.
    auto result = page_cache_->Allocate(1);
    if (result.is_error()) {
      return result.error_value();
    }
    raw_page = pop_and_init_phys_page(&result.value().page_list);
  }
  // Construct and insert the page.
  {
    Guard<Mutex> guard{&tracker_lock_};
    auto page_id = ++next_key_;
    if (page_id == 0) {
      // |next_key_| has overflowed. We need to make this condition sticky.
      page_id = ktl::numeric_limits<uint32_t>::max();
      return ZX_ERR_BAD_STATE;
    }

    if ((top_page_ != nullptr) && (top_page_->object_count_locking() < (objects_per_page / 3))) {
      // Hmmm, top page has plenty of space? which means we got beat to it. Undo the work.
      // It is not fatal to have ignored the free_count() and just used this page as top
      // but it means that we'll have a non-top page with a lot of free space which leads
      // to avoidable fragmentation.
      free_phys_page(raw_page);
      return ZX_OK;
    }
    if (page_map_.size() > max_pages()) {
      // We can't fit more ids in 32-bits given the constraints.
      free_phys_page(raw_page);
      return ZX_ERR_OUT_OF_RANGE;
    }

    auto new_top = new (raw_page) PageObj(page_id);
    page_map_.insert(new_top);
    top_page_ = new_top;
  }
  return ZX_OK;
}

template <typename TrackedType, typename HolderType, size_t utb>
void ObjectTracker<TrackedType, HolderType, utb>::optimize_pages(uint32_t page_id_hint,
                                                                 size_t page_count) {
  if (page_count < 3u) {
    return;
  }

  PageObj* page_to_free = nullptr;
  {
    Guard<Mutex> guard{&tracker_lock_};
    auto hint_page = page_map_.find(page_id_hint);
    if (hint_page == page_map_.end()) {
      return;
    }

    if ((&(*hint_page) != top_page_)) {
      auto hint_count = hint_page->object_count_locking();
      auto top_count = top_page_->object_count_locking();
      // Note we still hold the |tracker_lock| and object_count() took and released
      // each page lock so we know there are no threads touching either pages.
      if (hint_count == 0u) {
        // Free this page (back to the page cache) since it is not the top page.
        page_to_free = page_map_.erase(hint_page);
      } else if (hint_count < top_count) {
        // This page still has objects but is better for allocations than |top_page|.
        top_page_ = &(*hint_page);
      } else if (top_count == 0u) {
        // We can free the top page and use the other page as |top_page|.
        page_to_free = page_map_.erase(top_page_->page_id());
        top_page_ = &(*hint_page);
      }
    }
  }

  if (page_to_free != nullptr) {
    page_to_free->~PageObject();
    free_phys_page(page_to_free);
  }
}

template <typename TrackedType, typename HolderType, size_t utb>
void* ObjectTracker<TrackedType, HolderType, utb>::pop_and_init_phys_page(
    page_cache::PageCache::PageList* page_list) {
  vm_page_t* page = list_remove_head_type(page_list, vm_page_t, queue_node);
  void* page_addr = paddr_to_physmap(page->paddr());
  // We could issue arch_zero_page(page_addr) here if we are paranoid.
  return page_addr;
}

template <typename TrackedType, typename HolderType, size_t utb>
void ObjectTracker<TrackedType, HolderType, utb>::free_phys_page(void* po) {
  auto paddr = physmap_to_paddr(po);
  auto page = paddr_to_vm_page(paddr);
  page_cache::PageCache::PageList list;
  list_add_tail(&list, &page->queue_node);
  page_cache_->Free(ktl::move(list));
}

// static.
template <typename TrackedType, typename HolderType, size_t utb>
auto ObjectTracker<TrackedType, HolderType, utb>::decode(uint32_t id) {
  struct Result {
    uint32_t page_id;
    uint32_t index;
    uint32_t user_tag;
  } r;
  r.page_id = id >> page_id_shift;
  r.user_tag = (id >> user_tag_shift) & user_tag_mask;
  r.index = id & index_mask;
  return r;
}

// static.
template <typename TrackedType, typename HolderType, size_t utb>
uint32_t ObjectTracker<TrackedType, HolderType, utb>::encode(uint32_t page_id, uint32_t index,
                                                             uint32_t user_tag) {
  return (page_id << page_id_shift) | ((user_tag & user_tag_mask) << user_tag_shift) |
         (index & index_mask);
}

// static.
template <typename TrackedType, typename HolderType, size_t utb>
uint32_t ObjectTracker<TrackedType, HolderType, utb>::encode(uint32_t page_id,
                                                             uint32_t index_and_tag) {
  return (page_id << page_id_shift) | index_and_tag;
}

}  // namespace fbl

#endif  // ZIRCON_KERNEL_LIB_FBL_INCLUDE_FBL_OBJECT_TRACKER_H_
