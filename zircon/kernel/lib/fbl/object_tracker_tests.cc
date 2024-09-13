// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fit/defer.h>
#include <lib/page_cache.h>
#include <lib/unittest/unittest.h>
#include <sys/types.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cassert>
#include <cstddef>
#include <cstdint>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <fbl/int_set.h>
#include <fbl/object_tracker.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>

namespace {

template <typename T>
void sieve_in_place(T &set, uint32_t sqr_max) {
  const uint32_t max = sqr_max * sqr_max;
  uint32_t p = 1u;
  do {
    p += 1;
    if (set.exists(p)) {
      for (auto m = (p * p); m <= max; m += p) {
        set.remove(m);
      }
    }
  } while (p <= sqr_max);
}

bool bitmap_sieve_2k5() {
  BEGIN_TEST;
  // The test is a basic prime number sieve which is surprisingly good at finding issues
  // in set implementations. The sieve is between [2, 2500] and should find 367 primes.
  constexpr uint32_t expected_prime_count = 367u;

  constexpr uint32_t sqrt_max = 50;
  constexpr uint32_t max_value = sqrt_max * sqrt_max;  // 2500.

  fbl::AllocChecker ac;
  auto set = new (&ac) fbl::IntSet<max_value + 1>();
  ASSERT_TRUE(ac.check());
  auto d = fit::defer([&set]() { delete set; });

  // Add all the integers to the set except 0 and 1.
  for (uint32_t v = 2u; v <= max_value; v++) {
    set->add(v);
  }
  EXPECT_EQ(2u, set->free_count());

  sieve_in_place(*set, sqrt_max);
  EXPECT_EQ(max_value - expected_prime_count + 1, set->free_count());

  // Rather than comparing the found primes to a list, we use four facts:
  // - We know how many primes there are in the range.
  // - Every prime (except for 2 or 3) is +1 or -1 distance from a multiple of six!
  // - Left primes (primes right before a multiple of 6) sum in the range is known.
  // - Right primes (primes right after a multiple of 6) sum in the range is known.

  uint32_t not_a_prime = 0u;
  uint32_t left_sum = 0u;
  uint32_t right_sum = 0u;
  uint32_t prime_count = 0;
  set->remove_all_fn([&not_a_prime, &left_sum, &right_sum, &prime_count](uint32_t prime) {
    auto c = prime % 6;
    if (c == 1) {
      left_sum += prime;
    } else if (c == 5) {
      right_sum += prime;
    } else if ((prime != 2u) && (prime != 3u)) {
      not_a_prime += 1;
    }
    prime_count += 1;
  });

  EXPECT_EQ(0u, not_a_prime);
  EXPECT_EQ(204946u, left_sum);
  EXPECT_EQ(215861u, right_sum);
  EXPECT_EQ(expected_prime_count, prime_count);

  END_TEST;
}

using fbl::ObjectTracker;

// Move-only object to be tracked.
struct TrackedItem {
  TrackedItem() = delete;
  TrackedItem(const TrackedItem &) = delete;
  TrackedItem &operator=(const TrackedItem &) = delete;

  explicit TrackedItem(uint32_t id) : value(2), id(id) {}

  TrackedItem(TrackedItem &&src) noexcept
      : value(ktl::exchange(src.value, 0u)), id(ktl::exchange(src.id, 0u)) {}

  TrackedItem &operator=(TrackedItem &&src) noexcept {
    value = ktl::exchange(src.value, 0u);
    id = ktl::exchange(src.id, 0u);
    return *this;
  }

  static constexpr uint32_t kTag = 10u;

  // Called when object is inserted in the tracker.
  uint32_t on_tracked() {
    // Without setting |value| to zero, some tests fail and others assert in
    // PageObject<>::destroy_all.
    value = 0;
    return kTag;  // the tag, 1010 binary.
  }

  zx_status_t on_holder(uint32_t tag) const { return (tag == kTag) ? ZX_OK : ZX_ERR_BAD_HANDLE; }

  // Called before being removed from the tracker.
  zx_status_t on_remove(uint32_t tag) const {
    if (tag == kTag || tag == 0xf) {
      // tag == 0xf is during destroy_all.
      return (value == 0) ? ZX_OK : ZX_ERR_ADDRESS_IN_USE;
    }
    return ZX_ERR_BAD_HANDLE;
  }

  int32_t value;
  uint32_t id;
};

struct ItemHolder {
  ItemHolder() : it_ptr(nullptr) {}
  explicit ItemHolder(TrackedItem &item) : it_ptr(&item) {}
  ~ItemHolder() = default;

  int32_t lock() const { return ktl::exchange(it_ptr->value, 1u); }

  void unlock() const { it_ptr->value = 0; }
  uint32_t get_id() const { return it_ptr->id; }

  TrackedItem *it_ptr;
};

bool objtracker_basic() {
  BEGIN_TEST;
  auto page_cache_result = page_cache::PageCache::Create(2u);
  ASSERT_TRUE(page_cache_result.is_ok());
  auto cache = ktl::move(page_cache_result.value());

  // Test unique id. They never decrease, while object tracker ids can.
  uint32_t test_uid_base = 100u;
  using Tracker = ObjectTracker<TrackedItem, ItemHolder>;

  {
    static_assert(Tracker::obs_per_page() == 496u);
    Tracker tracker(&cache);

    // We better don't have ghosts in this container.
    EXPECT_EQ(0u, tracker.count_slow());
    EXPECT_EQ(0u, tracker.page_count());

    // The page ids start at page_id 1 and index 0:
    // The actual value needs knowledge of the id packing internals but its here for completeness.
    const uint32_t first_id = Tracker::encode(1u, 0u, TrackedItem::kTag);
    ASSERT_EQ(0x3400u, first_id);
    // The user tag is 4 bits wide by default.
    ASSERT_EQ(0x2000u, Tracker::encode(1u, 0u, 0));
    ASSERT_EQ(0x3e00u, Tracker::encode(1u, 0u, 0xfu));
    // Both the above limit the number of pages.
    ASSERT_EQ(0x7ffffu, Tracker::max_pages());

    // Add 21 items.
    // All |id|s allocated within the same empty page are consecutive. The first 21 should stay
    // within page 1.
    for (uint32_t ix = 0; ix != 21; ++ix) {
      auto id = tracker.add(TrackedItem{test_uid_base++});
      ASSERT_TRUE(id.is_ok());
      EXPECT_EQ(ix + first_id, id.value());
      // The encoding and decoding helpers follows expectations.
      auto decoded = Tracker::decode(id.value());
      EXPECT_EQ(1u, decoded.page_id);
      EXPECT_EQ(ix, decoded.index);
      EXPECT_EQ(id.value(),
                Tracker::encode(decoded.page_id, decoded.index, decoded.user_tag));  // ???
    }

    EXPECT_EQ(21u, tracker.count_slow());
    EXPECT_EQ(1u, tracker.page_count());

    // Spot check the fourth and last item are there. Note that |tluid|'s start at 100.
    {
      auto holder1 = tracker.get(Tracker::encode(1u, 3u, TrackedItem::kTag));
      ASSERT_TRUE(holder1.is_ok());
      EXPECT_EQ(103u, holder1.value().get_id());
      auto holder2 = tracker.get(Tracker::encode(1u, 20u, TrackedItem::kTag));
      ASSERT_TRUE(holder2.is_ok());
      EXPECT_EQ(120u, holder2.value().get_id());
    }

    // Removing the last value succeeds, but removing the same again fails with "not-found".
    {
      auto removed1 = tracker.remove(20u + first_id);
      ASSERT_TRUE(removed1.is_ok());
      EXPECT_EQ(120u, removed1.value().id);
      EXPECT_EQ(20u, tracker.count_slow());
      auto removed2 = tracker.remove(20u + first_id);
      ASSERT_TRUE(removed2.is_error());
      EXPECT_EQ(ZX_ERR_NOT_FOUND, removed2.error_value());
      EXPECT_EQ(20u, tracker.count_slow());
    }

    // Lets get the 7th object Holder and use it to logically lock the item so
    // remove() wil fail. Then try again unlocked and it should succeed.
    {
      auto id = Tracker::encode(1u, 6u, TrackedItem::kTag);
      auto holder = tracker.get(id);
      ASSERT_TRUE(holder.is_ok());
      EXPECT_EQ(106u, holder.value().get_id());
      ASSERT_EQ(0, holder->lock());
      auto removed1 = tracker.remove(id);
      ASSERT_TRUE(removed1.is_error());
      EXPECT_EQ(ZX_ERR_ADDRESS_IN_USE, removed1.error_value());
      holder->unlock();
      auto removed2 = tracker.remove(id);
      ASSERT_TRUE(removed2.is_ok());
      EXPECT_EQ(106u, removed2.value().id);
      EXPECT_EQ(19u, tracker.count_slow());
    }
    // uint32_t first_2p = 0;
    //  Now we add objects until we get to the second page. Note that there are
    //  two free slots at position 7th and 20th.
    while (true) {
      auto id = tracker.add(TrackedItem{test_uid_base++});
      ASSERT_TRUE(id.is_ok());
      auto decoded = Tracker::decode(id.value());
      if (decoded.page_id > 1) {
        EXPECT_EQ(2u, decoded.page_id);
        EXPECT_EQ(0u, decoded.index);
        break;
      }
    };
    EXPECT_EQ(Tracker::obs_per_page() + 1, tracker.count_slow());
    EXPECT_EQ(2u, tracker.page_count());
    // The 7th and 20th memory slot must have been reused to hold the 121 and 122 items.
    {
      auto holder1 = tracker.get(Tracker::encode(1u, 6u, TrackedItem::kTag));
      ASSERT_TRUE(holder1.is_ok());
      EXPECT_EQ(121u, holder1.value().get_id());
      auto holder2 = tracker.get(Tracker::encode(1u, 20u, TrackedItem::kTag));
      ASSERT_TRUE(holder2.is_ok());
      EXPECT_EQ(122u, holder2.value().get_id());
    }
    // Removing the last added element will make the second page completely free, but since the
    // first page is full, we should be holding to that second page.
    {
      auto removed = tracker.remove(Tracker::encode(2u, 0, TrackedItem::kTag));
      ASSERT_TRUE(removed.is_ok());
      EXPECT_EQ(test_uid_base - 1, removed.value().id);
      EXPECT_EQ(2u, tracker.page_count());
      EXPECT_EQ(Tracker::obs_per_page(), tracker.count_slow());
    }
    // The only way to free the (now empty) second page is to remove all elements from the
    // first page so that we have Since we know it is completely full, we simply remove all
    // until we get a "not found" and check we have the right id.
    {
      uint32_t id = Tracker::encode(1u, 0, TrackedItem::kTag);
      uint32_t last = Tracker::encode(1u, Tracker::obs_per_page() - 1, TrackedItem::kTag);
      while (true) {
        auto removed = tracker.remove(id);
        if (removed.is_error()) {
          EXPECT_EQ(ZX_ERR_NOT_FOUND, removed.error_value());
          EXPECT_EQ(last + 1, id);
          break;
        }
        id += 1;
      }
      // We could check tracker.page_count() but how pages are actually freed is in flux.
      EXPECT_EQ(0u, tracker.count_slow());
    }
  }
  END_TEST;
}

bool objtracker_multi() {
  BEGIN_TEST;
  auto page_cache_result = page_cache::PageCache::Create(2u);
  ASSERT_TRUE(page_cache_result.is_ok());
  auto cache = ktl::move(page_cache_result.value());

  using Tracker = ObjectTracker<TrackedItem, ItemHolder>;
  static_assert(Tracker::obs_per_page() == 496u);
  Tracker tracker(&cache);

  // Adding or removing zero objects is allowed.
  {
    ASSERT_EQ(ZX_OK, tracker.add(ktl::span<TrackedItem>(), ktl::span<uint32_t>()));
    ASSERT_EQ(ZX_OK, tracker.remove(ktl::span<uint32_t>(), ktl::span<TrackedItem>()));
  }

  constexpr uint32_t test_uid_base = 50u;
  // Add and remove 260 objects at once. They will fit on the first page.
  {
    constexpr size_t num_objs = 260u;
    fbl::Vector<TrackedItem> items;
    fbl::AllocChecker ac;
    items.reserve(num_objs, &ac);
    ASSERT_TRUE(ac.check());
    for (uint32_t ix = 0; ix != num_objs; ix++) {
      items.push_back(TrackedItem(ix + test_uid_base), &ac);
      ASSERT_TRUE(ac.check());
    }

    fbl::Vector<uint32_t> ids;
    ids.resize(num_objs, &ac);
    ASSERT_TRUE(ac.check());

    auto st = tracker.add(items, ids);
    ASSERT_EQ(ZX_OK, st);

    EXPECT_EQ(num_objs, ids.size());
    EXPECT_EQ(num_objs, tracker.count_slow());
    EXPECT_EQ(1u, tracker.page_count());

    auto c_id = ids[0];
    EXPECT_EQ(0x3400u, c_id);
    // Verify we got unique ids, the are sequential and on the first page.
    for (auto id : ids) {
      EXPECT_EQ(c_id, id);
      auto dec = Tracker::decode(id);
      EXPECT_EQ(1u, dec.page_id);
      c_id++;
    }
    // Test that all items have been moved into the container.
    uint32_t ix = 0;
    for (auto id : ids) {
      auto holder = tracker.get(id);
      ASSERT_TRUE(holder.is_ok());
      EXPECT_EQ(ix + test_uid_base, holder.value().get_id());
      ix++;
    }
    // .. and moved out of the input vector.
    for (const auto &it : items) {
      EXPECT_EQ(0u, it.id);
    }
    // Add a sentinel object.
    auto sentinel = tracker.add(TrackedItem{10001u});
    EXPECT_TRUE(sentinel.is_ok());
    EXPECT_EQ(num_objs + 1, tracker.count_slow());
    EXPECT_EQ(0x3400u + num_objs, sentinel.value());

    // reverse the ids and remove all the objects pointing to it.
    ktl::reverse(ids.begin(), ids.end());
    EXPECT_EQ(ZX_OK, tracker.remove(ids, items));

    // Verify that we got the objects in the reversed order..
    for (uint32_t ik = 0; ik != num_objs; ik++) {
      EXPECT_EQ(test_uid_base + num_objs - 1 - ik, items[ik].id);
    }
    // Sentinel still there.
    EXPECT_EQ(1u, tracker.count_slow());

    // Remove the sentinel but include two bogus ids, it should still remove
    // the sentinel.
    uint32_t bogus[] = {7777u, sentinel.value(), 6666u};
    auto just_sentinel = ktl::span(items).subspan(0, 3);
    st = tracker.remove(bogus, just_sentinel);
    EXPECT_EQ(ZX_ERR_NOT_FOUND, st);
    EXPECT_EQ(0u, tracker.count_slow());
    EXPECT_EQ(10001u, just_sentinel[1].id);
  }

  {
    // Add 2000 faux objects. This tests the multi-allocation paths.
    constexpr size_t num_objs = 2000u;
    fbl::Vector<TrackedItem> items;
    fbl::Vector<uint32_t> ids;
    fbl::AllocChecker ac;
    items.reserve(num_objs, &ac);
    ASSERT_TRUE(ac.check());
    ids.resize(num_objs, &ac);
    ASSERT_TRUE(ac.check());
    for (size_t ix = 0; ix != num_objs; ix++) {
      items.push_back(TrackedItem(static_cast<uint32_t>(ix)), &ac);
      ASSERT_TRUE(ac.check());
    }
    // Note that each TrackedObject::id() matches its index in |ids|.
    EXPECT_EQ(ZX_OK, tracker.add(items, ids));
    EXPECT_EQ(num_objs, tracker.count_slow());
    EXPECT_EQ((num_objs + 496u - 1) / 496u, tracker.page_count());

    // Check a subset via the batch-get. This will cross 3 pages since each page
    // fits 496 TrackedItems.
    constexpr size_t start_sub = 300u;
    constexpr size_t count_sub = 700u;
    auto ids_subset = ktl::span<uint32_t>(ids).subspan(start_sub, count_sub);

    fbl::Vector<ItemHolder> holders;
    holders.resize(count_sub, &ac);
    ASSERT_TRUE(ac.check());

    EXPECT_EQ(ZX_OK, tracker.get(ids_subset, holders));
    uint32_t ix = 0;
    for (auto &holder : holders) {
      EXPECT_EQ(ix + start_sub, holder.get_id());
      ++ix;
    }

    ktl::reverse(ids_subset.begin(), ids_subset.end());

    EXPECT_EQ(ZX_OK, tracker.get(ids_subset, holders));
    ix = 0;
    for (auto &holder : holders) {
      EXPECT_EQ(start_sub + count_sub - 1 - ix, holder.get_id());
      ++ix;
    }

    EXPECT_EQ(num_objs, tracker.destroy_all());
  }

  END_TEST;
}

}  // namespace

UNITTEST_START_TESTCASE(object_tracker_tests)
UNITTEST("Bitmap Sieve", bitmap_sieve_2k5)
UNITTEST("ObjectTracker basic", objtracker_basic)
UNITTEST("ObjectTracker multiple", objtracker_multi)
UNITTEST_END_TESTCASE(object_tracker_tests, "ObjectTracker", "Object Tracker test")
