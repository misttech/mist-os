// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fit/defer.h>

#include <arch/defines.h>
#include <vm/discardable_vmo_tracker.h>
#include <vm/pinned_vm_object.h>

#include "test_helper.h"

namespace {

// Helper wrapper around reclaiming a page that returns the pages to the pmm.
uint64_t reclaim_page(fbl::RefPtr<VmCowPages> vmo, vm_page_t* page, uint64_t offset,
                      VmCowPages::EvictionHintAction hint_action, VmCompressor* compressor) {
  const uint64_t count = vmo->ReclaimPage(page, offset, hint_action, compressor).Total();
  return count;
}

uint64_t reclaim_page(fbl::RefPtr<VmObjectPaged> vmo, vm_page_t* page, uint64_t offset,
                      VmCowPages::EvictionHintAction hint_action, VmCompressor* compressor) {
  return reclaim_page(vmo->DebugGetCowPages(), page, offset, hint_action, compressor);
}

}  // namespace

namespace vm_unittest {

// Creates a vm object.
static bool vmo_create_test() {
  BEGIN_TEST;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, PAGE_SIZE, &vmo);
  ASSERT_EQ(status, ZX_OK);
  ASSERT_TRUE(vmo);
  EXPECT_FALSE(vmo->is_contiguous(), "vmo is not contig\n");
  EXPECT_FALSE(vmo->is_resizable(), "vmo is not resizable\n");
  END_TEST;
}

static bool vmo_create_maximum_size() {
  BEGIN_TEST;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, 0xfffffffffffe0000, &vmo);
  EXPECT_EQ(status, ZX_OK, "should be ok\n");

  status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, 0xfffffffffffe1000, &vmo);
  EXPECT_EQ(status, ZX_ERR_OUT_OF_RANGE, "should be too large\n");
  END_TEST;
}

// Helper that tests if all pages in a vmo in the specified range pass the given predicate.
template <typename F>
static bool AllPagesMatch(VmObject* vmo, F pred, uint64_t offset, uint64_t len) {
  bool pred_matches = true;
  zx_status_t status =
      vmo->Lookup(offset, len, [&pred, &pred_matches](uint64_t offset, paddr_t pa) {
        const vm_page_t* p = paddr_to_vm_page(pa);
        if (!pred(p)) {
          pred_matches = false;
          return ZX_ERR_STOP;
        }
        return ZX_ERR_NEXT;
      });
  return status == ZX_OK ? pred_matches : false;
}

static bool PagesInAnyAnonymousQueue(VmObject* vmo, uint64_t offset, uint64_t len) {
  return AllPagesMatch(
      vmo, [](const vm_page_t* p) { return pmm_page_queues()->DebugPageIsAnyAnonymous(p); }, offset,
      len);
}

static bool PagesInWiredQueue(VmObject* vmo, uint64_t offset, uint64_t len) {
  return AllPagesMatch(
      vmo, [](const vm_page_t* p) { return pmm_page_queues()->DebugPageIsWired(p); }, offset, len);
}

// Creates a vm object, commits memory.
static bool vmo_commit_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  static const size_t alloc_size = PAGE_SIZE * 16;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, alloc_size, &vmo);
  ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");

  auto ret = vmo->CommitRange(0, alloc_size);
  ASSERT_EQ(ZX_OK, ret, "committing vm object\n");
  EXPECT_TRUE(make_private_attribution_counts(alloc_size, 0) == vmo->GetAttributedMemory())
  EXPECT_TRUE(PagesInAnyAnonymousQueue(vmo.get(), 0, alloc_size));
  END_TEST;
}

static bool vmo_commit_compressed_pages_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;
  // Need a working compressor.
  auto compression = Pmm::Node().GetPageCompression();
  if (!compression) {
    END_TEST;
  }

  // Create a VMO and commit some real pages.
  constexpr size_t kPages = 8;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, kPages * PAGE_SIZE, &vmo);
  ASSERT_OK(status);
  status = vmo->CommitRange(0, kPages * PAGE_SIZE);
  ASSERT_OK(status);

  // Validate these are committed.
  EXPECT_TRUE(make_private_attribution_counts(kPages * PAGE_SIZE, 0) == vmo->GetAttributedMemory())
  EXPECT_TRUE(PagesInAnyAnonymousQueue(vmo.get(), 0, kPages * PAGE_SIZE));

  // Lookup and compress each page;
  for (size_t i = 0; i < kPages; i++) {
    // Write some data (possibly zero) to the page.
    EXPECT_OK(vmo->Write(&i, i * PAGE_SIZE, sizeof(i)));
    vm_page_t* page;
    status = vmo->GetPageBlocking(i * PAGE_SIZE, 0, nullptr, &page, nullptr);
    ASSERT_OK(status);
    auto compressor = compression->AcquireCompressor();
    ASSERT_OK(compressor.get().Arm());

    uint64_t reclaimed = reclaim_page(vmo, page, i * PAGE_SIZE,
                                      VmCowPages::EvictionHintAction::Follow, &compressor.get());
    EXPECT_EQ(reclaimed, 1u);
  }

  // Should be no real pages, and one of the pages should have been deduped to zero and not even be
  // compressed.
  EXPECT_TRUE(make_private_attribution_counts(0, (kPages - 1) * PAGE_SIZE) ==
              vmo->GetAttributedMemory())

  // Now use commit again, this should decompress things.
  status = vmo->CommitRange(0, kPages * PAGE_SIZE);
  ASSERT_OK(status);

  EXPECT_TRUE(make_private_attribution_counts(kPages * PAGE_SIZE, 0) == vmo->GetAttributedMemory())
  EXPECT_TRUE(PagesInAnyAnonymousQueue(vmo.get(), 0, kPages * PAGE_SIZE));

  END_TEST;
}

// Creates paged VMOs, pins them, and tries operations that should unpin.
static bool vmo_pin_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  static const size_t alloc_size = PAGE_SIZE * 16;
  for (uint32_t is_loaning_enabled = 0; is_loaning_enabled < 2; ++is_loaning_enabled) {
    bool loaning_was_enabled = pmm_physical_page_borrowing_config()->is_loaning_enabled();
    pmm_physical_page_borrowing_config()->set_loaning_enabled(!!is_loaning_enabled);
    auto cleanup = fit::defer([loaning_was_enabled] {
      pmm_physical_page_borrowing_config()->set_loaning_enabled(loaning_was_enabled);
    });

    fbl::RefPtr<VmObjectPaged> vmo;
    zx_status_t status;
    status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kResizable, alloc_size, &vmo);
    ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
    ASSERT_TRUE(vmo, "vmobject creation\n");

    status = vmo->CommitRangePinned(PAGE_SIZE, alloc_size, false);
    EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, status, "pinning out of range\n");
    status = vmo->CommitRangePinned(PAGE_SIZE, 0, false);
    EXPECT_EQ(ZX_ERR_INVALID_ARGS, status, "pinning range of len 0\n");

    status = vmo->CommitRangePinned(PAGE_SIZE, 3 * PAGE_SIZE, false);
    EXPECT_EQ(ZX_OK, status, "pinning range\n");
    EXPECT_TRUE(PagesInWiredQueue(vmo.get(), PAGE_SIZE, 3 * PAGE_SIZE));

    status = vmo->DecommitRange(PAGE_SIZE, 3 * PAGE_SIZE);
    EXPECT_EQ(ZX_ERR_BAD_STATE, status, "decommitting pinned range\n");
    status = vmo->DecommitRange(PAGE_SIZE, PAGE_SIZE);
    EXPECT_EQ(ZX_ERR_BAD_STATE, status, "decommitting pinned range\n");
    status = vmo->DecommitRange(3 * PAGE_SIZE, PAGE_SIZE);
    EXPECT_EQ(ZX_ERR_BAD_STATE, status, "decommitting pinned range\n");

    vmo->Unpin(PAGE_SIZE, 3 * PAGE_SIZE);
    EXPECT_TRUE(PagesInAnyAnonymousQueue(vmo.get(), PAGE_SIZE, 3 * PAGE_SIZE));

    status = vmo->DecommitRange(PAGE_SIZE, 3 * PAGE_SIZE);
    EXPECT_EQ(ZX_OK, status, "decommitting unpinned range\n");

    status = vmo->CommitRangePinned(PAGE_SIZE, 3 * PAGE_SIZE, false);
    EXPECT_EQ(ZX_OK, status, "pinning range after decommit\n");
    EXPECT_TRUE(PagesInWiredQueue(vmo.get(), PAGE_SIZE, 3 * PAGE_SIZE));

    status = vmo->Resize(0);
    EXPECT_EQ(ZX_ERR_BAD_STATE, status, "resizing pinned range\n");

    vmo->Unpin(PAGE_SIZE, 3 * PAGE_SIZE);

    status = vmo->Resize(0);
    EXPECT_EQ(ZX_OK, status, "resizing unpinned range\n");
  }

  END_TEST;
}

// Creates contiguous VMOs, pins them, and tries operations that should unpin.
static bool vmo_pin_contiguous_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  static const size_t alloc_size = PAGE_SIZE * 16;
  for (uint32_t is_loaning_enabled = 0; is_loaning_enabled < 2; ++is_loaning_enabled) {
    bool loaning_was_enabled = pmm_physical_page_borrowing_config()->is_loaning_enabled();
    pmm_physical_page_borrowing_config()->set_loaning_enabled(!!is_loaning_enabled);
    auto cleanup = fit::defer([loaning_was_enabled] {
      pmm_physical_page_borrowing_config()->set_loaning_enabled(loaning_was_enabled);
    });

    fbl::RefPtr<VmObjectPaged> vmo;
    zx_status_t status;
    status = VmObjectPaged::CreateContiguous(PMM_ALLOC_FLAG_ANY, alloc_size,
                                             /*alignment_log2=*/0, &vmo);
    ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
    ASSERT_TRUE(vmo, "vmobject creation\n");

    status = vmo->CommitRangePinned(PAGE_SIZE, alloc_size, false);
    EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, status, "pinning out of range\n");
    status = vmo->CommitRangePinned(PAGE_SIZE, 0, false);
    EXPECT_EQ(ZX_ERR_INVALID_ARGS, status, "pinning range of len 0\n");

    status = vmo->CommitRangePinned(PAGE_SIZE, 3 * PAGE_SIZE, false);
    EXPECT_EQ(ZX_OK, status, "pinning range\n");
    EXPECT_TRUE(PagesInWiredQueue(vmo.get(), PAGE_SIZE, 3 * PAGE_SIZE));

    status = vmo->DecommitRange(PAGE_SIZE, 3 * PAGE_SIZE);
    if (!is_loaning_enabled) {
      EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, status, "decommitting pinned range\n");
    } else {
      EXPECT_EQ(ZX_ERR_BAD_STATE, status, "decommitting pinned range\n");
    }
    status = vmo->DecommitRange(PAGE_SIZE, PAGE_SIZE);
    if (!is_loaning_enabled) {
      EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, status, "decommitting pinned range\n");
    } else {
      EXPECT_EQ(ZX_ERR_BAD_STATE, status, "decommitting pinned range\n");
    }
    status = vmo->DecommitRange(3 * PAGE_SIZE, PAGE_SIZE);
    if (!is_loaning_enabled) {
      EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, status, "decommitting pinned range\n");
    } else {
      EXPECT_EQ(ZX_ERR_BAD_STATE, status, "decommitting pinned range\n");
    }

    vmo->Unpin(PAGE_SIZE, 3 * PAGE_SIZE);
    EXPECT_TRUE(PagesInWiredQueue(vmo.get(), PAGE_SIZE, 3 * PAGE_SIZE));

    status = vmo->DecommitRange(PAGE_SIZE, 3 * PAGE_SIZE);
    if (!is_loaning_enabled) {
      EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, status, "decommitting unpinned range\n");
    } else {
      EXPECT_EQ(ZX_OK, status, "decommitting unpinned range\n");
    }

    status = vmo->CommitRangePinned(PAGE_SIZE, 3 * PAGE_SIZE, false);
    EXPECT_EQ(ZX_OK, status, "pinning range after decommit\n");
    EXPECT_TRUE(PagesInWiredQueue(vmo.get(), PAGE_SIZE, 3 * PAGE_SIZE));

    vmo->Unpin(PAGE_SIZE, 3 * PAGE_SIZE);
  }

  END_TEST;
}

// Creates a page VMO and pins the same pages multiple times
static bool vmo_multiple_pin_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  static const size_t alloc_size = PAGE_SIZE * 16;
  for (uint32_t is_ppb_enabled = 0; is_ppb_enabled < 2; ++is_ppb_enabled) {
    bool loaning_was_enabled = pmm_physical_page_borrowing_config()->is_loaning_enabled();
    bool borrowing_was_enabled =
        pmm_physical_page_borrowing_config()->is_borrowing_in_supplypages_enabled();
    pmm_physical_page_borrowing_config()->set_loaning_enabled(is_ppb_enabled);
    pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(is_ppb_enabled);
    auto cleanup = fit::defer([loaning_was_enabled, borrowing_was_enabled] {
      pmm_physical_page_borrowing_config()->set_loaning_enabled(loaning_was_enabled);
      pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(
          borrowing_was_enabled);
    });

    fbl::RefPtr<VmObjectPaged> vmo;
    zx_status_t status;
    status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, alloc_size, &vmo);
    ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
    ASSERT_TRUE(vmo, "vmobject creation\n");

    status = vmo->CommitRangePinned(0, alloc_size, false);
    EXPECT_EQ(ZX_OK, status, "pinning whole range\n");
    EXPECT_TRUE(PagesInWiredQueue(vmo.get(), 0, alloc_size));
    status = vmo->CommitRangePinned(PAGE_SIZE, 4 * PAGE_SIZE, false);
    EXPECT_EQ(ZX_OK, status, "pinning subrange\n");
    EXPECT_TRUE(PagesInWiredQueue(vmo.get(), 0, alloc_size));

    for (unsigned int i = 1; i < VM_PAGE_OBJECT_MAX_PIN_COUNT; ++i) {
      status = vmo->CommitRangePinned(0, PAGE_SIZE, false);
      EXPECT_EQ(ZX_OK, status, "pinning first page max times\n");
    }
    status = vmo->CommitRangePinned(0, PAGE_SIZE, false);
    EXPECT_EQ(ZX_ERR_UNAVAILABLE, status, "page is pinned too much\n");

    vmo->Unpin(0, alloc_size);
    EXPECT_TRUE(PagesInWiredQueue(vmo.get(), PAGE_SIZE, 4 * PAGE_SIZE));
    EXPECT_TRUE(PagesInAnyAnonymousQueue(vmo.get(), 5 * PAGE_SIZE, alloc_size - 5 * PAGE_SIZE));
    status = vmo->DecommitRange(PAGE_SIZE, 4 * PAGE_SIZE);
    EXPECT_EQ(ZX_ERR_BAD_STATE, status, "decommitting pinned range\n");
    status = vmo->DecommitRange(5 * PAGE_SIZE, alloc_size - 5 * PAGE_SIZE);
    EXPECT_EQ(ZX_OK, status, "decommitting unpinned range\n");

    vmo->Unpin(PAGE_SIZE, 4 * PAGE_SIZE);
    status = vmo->DecommitRange(PAGE_SIZE, 4 * PAGE_SIZE);
    EXPECT_EQ(ZX_OK, status, "decommitting unpinned range\n");

    for (unsigned int i = 2; i < VM_PAGE_OBJECT_MAX_PIN_COUNT; ++i) {
      vmo->Unpin(0, PAGE_SIZE);
    }
    status = vmo->DecommitRange(0, PAGE_SIZE);
    EXPECT_EQ(ZX_ERR_BAD_STATE, status, "decommitting unpinned range\n");

    vmo->Unpin(0, PAGE_SIZE);
    status = vmo->DecommitRange(0, PAGE_SIZE);
    EXPECT_EQ(ZX_OK, status, "decommitting unpinned range\n");
  }

  END_TEST;
}

// Creates a contiguous VMO and pins the same pages multiple times
static bool vmo_multiple_pin_contiguous_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  static const size_t alloc_size = PAGE_SIZE * 16;
  for (uint32_t is_ppb_enabled = 0; is_ppb_enabled < 2; ++is_ppb_enabled) {
    bool loaning_was_enabled = pmm_physical_page_borrowing_config()->is_loaning_enabled();
    bool borrowing_was_enabled =
        pmm_physical_page_borrowing_config()->is_borrowing_in_supplypages_enabled();
    pmm_physical_page_borrowing_config()->set_loaning_enabled(is_ppb_enabled);
    pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(is_ppb_enabled);
    auto cleanup = fit::defer([loaning_was_enabled, borrowing_was_enabled] {
      pmm_physical_page_borrowing_config()->set_loaning_enabled(loaning_was_enabled);
      pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(
          borrowing_was_enabled);
    });

    fbl::RefPtr<VmObjectPaged> vmo;
    zx_status_t status;
    status = VmObjectPaged::CreateContiguous(PMM_ALLOC_FLAG_ANY, alloc_size,
                                             /*alignment_log2=*/0, &vmo);
    ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
    ASSERT_TRUE(vmo, "vmobject creation\n");

    status = vmo->CommitRangePinned(0, alloc_size, false);
    EXPECT_EQ(ZX_OK, status, "pinning whole range\n");
    EXPECT_TRUE(PagesInWiredQueue(vmo.get(), 0, alloc_size));
    status = vmo->CommitRangePinned(PAGE_SIZE, 4 * PAGE_SIZE, false);
    EXPECT_EQ(ZX_OK, status, "pinning subrange\n");
    EXPECT_TRUE(PagesInWiredQueue(vmo.get(), 0, alloc_size));

    for (unsigned int i = 1; i < VM_PAGE_OBJECT_MAX_PIN_COUNT; ++i) {
      status = vmo->CommitRangePinned(0, PAGE_SIZE, false);
      EXPECT_EQ(ZX_OK, status, "pinning first page max times\n");
    }
    status = vmo->CommitRangePinned(0, PAGE_SIZE, false);
    EXPECT_EQ(ZX_ERR_UNAVAILABLE, status, "page is pinned too much\n");

    vmo->Unpin(0, alloc_size);
    EXPECT_TRUE(PagesInWiredQueue(vmo.get(), PAGE_SIZE, 4 * PAGE_SIZE));
    EXPECT_TRUE(PagesInWiredQueue(vmo.get(), 5 * PAGE_SIZE, alloc_size - 5 * PAGE_SIZE));
    status = vmo->DecommitRange(PAGE_SIZE, 4 * PAGE_SIZE);
    if (!is_ppb_enabled) {
      EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, status, "decommitting pinned range\n");
    } else {
      EXPECT_EQ(ZX_ERR_BAD_STATE, status, "decommitting pinned range\n");
    }
    status = vmo->DecommitRange(5 * PAGE_SIZE, alloc_size - 5 * PAGE_SIZE);
    if (!is_ppb_enabled) {
      EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, status, "decommitting unpinned range\n");
    } else {
      EXPECT_EQ(ZX_OK, status, "decommitting unpinned range\n");
    }

    vmo->Unpin(PAGE_SIZE, 4 * PAGE_SIZE);
    status = vmo->DecommitRange(PAGE_SIZE, 4 * PAGE_SIZE);
    if (!is_ppb_enabled) {
      EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, status, "decommitting unpinned range\n");
    } else {
      EXPECT_EQ(ZX_OK, status, "decommitting unpinned range\n");
    }

    for (unsigned int i = 2; i < VM_PAGE_OBJECT_MAX_PIN_COUNT; ++i) {
      vmo->Unpin(0, PAGE_SIZE);
    }
    status = vmo->DecommitRange(0, PAGE_SIZE);
    if (!is_ppb_enabled) {
      EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, status, "decommitting unpinned range\n");
    } else {
      EXPECT_EQ(ZX_ERR_BAD_STATE, status, "decommitting unpinned range\n");
    }

    vmo->Unpin(0, PAGE_SIZE);
    status = vmo->DecommitRange(0, PAGE_SIZE);
    if (!is_ppb_enabled) {
      EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, status, "decommitting unpinned range\n");
    } else {
      EXPECT_EQ(ZX_OK, status, "decommitting unpinned range\n");
    }
  }

  END_TEST;
}

// Checks that VMOs must be page aligned sizes.
static bool vmo_unaligned_size_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  static const size_t alloc_size = 15;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, alloc_size, &vmo);
  ASSERT_EQ(status, ZX_ERR_INVALID_ARGS, "vmobject creation\n");

  END_TEST;
}

// Creates a vm object, checks that attribution via reference doesn't attribute pages unless we
// specifically request it
static bool vmo_reference_attribution_commit_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  static const size_t alloc_size = 8ul * PAGE_SIZE;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, alloc_size, &vmo);
  ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");

  fbl::RefPtr<VmObject> vmo_reference;
  status =
      vmo->CreateChildReference(Resizability::NonResizable, 0u, 0, true, nullptr, &vmo_reference);
  ASSERT_EQ(status, ZX_OK, "vmobject reference creation\n");
  ASSERT_TRUE(vmo_reference, "vmobject reference creation\n");

  auto ret = vmo->CommitRange(0, alloc_size);
  EXPECT_EQ(ZX_OK, ret, "committing vm object\n");
  EXPECT_TRUE(make_private_attribution_counts(alloc_size, 0) == vmo->GetAttributedMemory())

  EXPECT_TRUE((vm::AttributionCounts{}) == vmo_reference->GetAttributedMemory(),
              "vmo_reference attribution\n");

  EXPECT_TRUE(make_private_attribution_counts(alloc_size, 0) ==
                  vmo_reference->GetAttributedMemoryInReferenceOwner(),
              "vmo_reference explicit reference attribution\n");

  END_TEST;
}

static bool vmo_create_physical_test() {
  BEGIN_TEST;

  paddr_t pa;
  vm_page_t* vm_page;
  zx_status_t status = pmm_alloc_page(0, &vm_page, &pa);
  uint32_t cache_policy;

  ASSERT_EQ(ZX_OK, status, "vm page allocation\n");
  ASSERT_TRUE(vm_page);

  fbl::RefPtr<VmObjectPhysical> vmo;
  status = VmObjectPhysical::Create(pa, PAGE_SIZE, &vmo);
  ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");
  cache_policy = vmo->GetMappingCachePolicy();
  EXPECT_EQ(ARCH_MMU_FLAG_UNCACHED, cache_policy, "check initial cache policy");
  EXPECT_TRUE(vmo->is_contiguous(), "check contiguous");

  vmo.reset();
  pmm_free_page(vm_page);

  END_TEST;
}

static bool vmo_physical_pin_test() {
  BEGIN_TEST;

  paddr_t pa;
  vm_page_t* vm_page;
  zx_status_t status = pmm_alloc_page(0, &vm_page, &pa);
  ASSERT_EQ(ZX_OK, status);

  fbl::RefPtr<VmObjectPhysical> vmo;
  status = VmObjectPhysical::Create(pa, PAGE_SIZE, &vmo);

  // Validate we can pin the range.
  EXPECT_EQ(ZX_OK, vmo->CommitRangePinned(0, PAGE_SIZE, false));

  // Pinning out side should fail.
  EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, vmo->CommitRangePinned(PAGE_SIZE, PAGE_SIZE, false));

  // Unpin for physical VMOs does not currently do anything, but still call it to be API correct.
  vmo->Unpin(0, PAGE_SIZE);

  vmo.reset();
  pmm_free_page(vm_page);

  END_TEST;
}

// Creates a vm object that commits contiguous memory.
static bool vmo_create_contiguous_test() {
  BEGIN_TEST;
  static const size_t alloc_size = PAGE_SIZE * 16;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::CreateContiguous(PMM_ALLOC_FLAG_ANY, alloc_size, 0, &vmo);
  ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");

  EXPECT_TRUE(vmo->is_contiguous(), "vmo is contig\n");

  // Contiguous VMOs are not pinned, but they are notionally wired as they will not be automatically
  // manipulated by the kernel.
  EXPECT_TRUE(PagesInWiredQueue(vmo.get(), 0, alloc_size));

  paddr_t last_pa;
  auto lookup_func = [&last_pa](uint64_t offset, paddr_t pa) {
    if (offset != 0 && last_pa + PAGE_SIZE != pa) {
      return ZX_ERR_BAD_STATE;
    }
    last_pa = pa;
    return ZX_ERR_NEXT;
  };
  status = vmo->Lookup(0, alloc_size, lookup_func);
  paddr_t first_pa;
  paddr_t second_pa;
  EXPECT_EQ(status, ZX_OK, "vmo lookup\n");
  EXPECT_EQ(ZX_OK, vmo->LookupContiguous(0, alloc_size, &first_pa));
  EXPECT_EQ(first_pa + alloc_size - PAGE_SIZE, last_pa);
  EXPECT_EQ(ZX_OK, vmo->LookupContiguous(PAGE_SIZE, PAGE_SIZE, &second_pa));
  EXPECT_EQ(first_pa + PAGE_SIZE, second_pa);
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, vmo->LookupContiguous(42, PAGE_SIZE, nullptr));
  EXPECT_EQ(ZX_ERR_OUT_OF_RANGE,
            vmo->LookupContiguous(alloc_size - PAGE_SIZE, PAGE_SIZE * 2, nullptr));

  END_TEST;
}

// Make sure decommitting pages from a contiguous VMO is allowed, and that we get back the correct
// pages when committing pages back into a contiguous VMO, even if another VMO was (temporarily)
// using those pages.
static bool vmo_contiguous_decommit_test() {
  BEGIN_TEST;

  bool loaning_was_enabled = pmm_physical_page_borrowing_config()->is_loaning_enabled();
  bool borrowing_was_enabled =
      pmm_physical_page_borrowing_config()->is_borrowing_in_supplypages_enabled();
  pmm_physical_page_borrowing_config()->set_loaning_enabled(true);
  pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(true);
  auto cleanup = fit::defer([loaning_was_enabled, borrowing_was_enabled] {
    pmm_physical_page_borrowing_config()->set_loaning_enabled(loaning_was_enabled);
    pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(
        borrowing_was_enabled);
  });

  static const size_t alloc_size = PAGE_SIZE * 16;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::CreateContiguous(PMM_ALLOC_FLAG_ANY, alloc_size, 0, &vmo);
  ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");

  paddr_t base_pa = static_cast<paddr_t>(-1);
  status = vmo->Lookup(0, PAGE_SIZE, [&base_pa](size_t offset, paddr_t pa) {
    ASSERT(base_pa == static_cast<paddr_t>(-1));
    ASSERT(offset == 0);
    base_pa = pa;
    return ZX_ERR_NEXT;
  });
  ASSERT_EQ(status, ZX_OK, "stash base pa works\n");
  ASSERT_TRUE(base_pa != static_cast<paddr_t>(-1));

  bool borrowed_seen = false;

  bool page_expected[alloc_size / PAGE_SIZE];
  for (bool& present : page_expected) {
    // Default to true.
    present = true;
  }
  // Make sure expected pages (and only expected pages) are present and consistent with start
  // physical address of contiguous VMO.
  auto verify_expected_pages = [vmo, base_pa, &borrowed_seen, &page_expected]() -> void {
    auto cow = vmo->DebugGetCowPages();
    bool page_seen[alloc_size / PAGE_SIZE] = {};
    auto lookup_func = [base_pa, &page_seen](size_t offset, paddr_t pa) {
      ASSERT(!page_seen[offset / PAGE_SIZE]);
      page_seen[offset / PAGE_SIZE] = true;
      if (pa - base_pa != offset) {
        return ZX_ERR_BAD_STATE;
      }
      return ZX_ERR_NEXT;
    };
    zx_status_t status = vmo->Lookup(0, alloc_size, lookup_func);
    ASSERT_MSG(status == ZX_OK, "vmo->Lookup() failed - status: %d\n", status);
    for (uint64_t offset = 0; offset < alloc_size; offset += PAGE_SIZE) {
      uint64_t page_index = offset / PAGE_SIZE;
      ASSERT_MSG(page_expected[page_index] == page_seen[page_index],
                 "page_expected[page_index] != page_seen[page_index]\n");
      vm_page_t* page_from_cow = cow->DebugGetPage(offset);
      vm_page_t* page_from_pmm = paddr_to_vm_page(base_pa + offset);
      ASSERT(page_from_pmm);
      if (page_expected[page_index]) {
        ASSERT(page_from_cow);
        ASSERT(page_from_cow == page_from_pmm);
        ASSERT(cow->DebugIsPage(offset));
        ASSERT(!page_from_pmm->is_loaned());
      } else {
        ASSERT(!page_from_cow);
        ASSERT(cow->DebugIsEmpty(offset));
        ASSERT(page_from_pmm->is_loaned());
        if (!page_from_pmm->is_free()) {
          // It's not in cow, and it's not free, so note that we observed a borrowed page.
          borrowed_seen = true;
        }
      }
      ASSERT(!page_from_pmm->is_loan_cancelled());
    }
  };
  verify_expected_pages();
  auto track_decommit = [vmo, &page_expected, &verify_expected_pages](uint64_t start_offset,
                                                                      uint64_t size) {
    ASSERT(IS_PAGE_ALIGNED(start_offset));
    ASSERT(IS_PAGE_ALIGNED(size));
    uint64_t end_offset = start_offset + size;
    for (uint64_t offset = start_offset; offset < end_offset; offset += PAGE_SIZE) {
      page_expected[offset / PAGE_SIZE] = false;
    }
    verify_expected_pages();
  };
  auto track_commit = [vmo, &page_expected, &verify_expected_pages](uint64_t start_offset,
                                                                    uint64_t size) {
    ASSERT(IS_PAGE_ALIGNED(start_offset));
    ASSERT(IS_PAGE_ALIGNED(size));
    uint64_t end_offset = start_offset + size;
    for (uint64_t offset = start_offset; offset < end_offset; offset += PAGE_SIZE) {
      page_expected[offset / PAGE_SIZE] = true;
    }
    verify_expected_pages();
  };

  status = vmo->DecommitRange(PAGE_SIZE, 4 * PAGE_SIZE);
  ASSERT_EQ(status, ZX_OK, "decommit of contiguous VMO pages works\n");
  track_decommit(PAGE_SIZE, 4 * PAGE_SIZE);

  status = vmo->DecommitRange(0, 4 * PAGE_SIZE);
  ASSERT_EQ(status, ZX_OK,
            "decommit of contiguous VMO pages overlapping non-present pages works\n");
  track_decommit(0, 4 * PAGE_SIZE);

  status = vmo->DecommitRange(alloc_size - PAGE_SIZE, PAGE_SIZE);
  ASSERT_EQ(status, ZX_OK, "decommit at end of contiguous VMO works\n");
  track_decommit(alloc_size - PAGE_SIZE, PAGE_SIZE);

  status = vmo->DecommitRange(0, alloc_size);
  ASSERT_EQ(status, ZX_OK, "decommit all overlapping non-present pages\n");
  track_decommit(0, alloc_size);

  // Due to concurrent activity of the system, we may not be able to allocate the loaned pages into
  // a VMO we're creating here, and depending on timing, we may also not observe the pages being
  // borrowed.  However, it shouldn't take many tries, if we continue to allocate non-pinned pages
  // to a VMO repeatedly, since loaned pages are preferred for allocations that can use them.
  //
  // We pay attention to whether ASAN is enabled in order to apply a strategy that's optimized for
  // pages being put on the head (normal) or tail (ASAN) of the free list
  // (PmmNode::free_loaned_list_).

  // Reset borrowed_seen since we should be able to see borrowing _within_ the loop below, mainly
  // so we can also have the loop below do a CommitRange() to reclaim before the borrowing VMO is
  // deleted.
  borrowed_seen = false;
  zx_instant_mono_t complain_deadline = current_mono_time() + ZX_SEC(5);
  uint32_t loop_count = 0;
  while (!borrowed_seen || loop_count < 5) {
    // Not super small, in case we end up needing to do multiple iterations of the loop to see the
    // pages being borrowed, and ASAN is enabled which could require more iterations of this loop
    // if this size were smaller.  Also hopefully not big enough to fail on small-ish devices.
    constexpr uint64_t kBorrowingVmoPages = 64;
    vm_page_t* pages[kBorrowingVmoPages];
    fbl::RefPtr<VmObjectPaged> borrowing_vmo;
    status = make_committed_pager_vmo(kBorrowingVmoPages, /*trap_dirty=*/false, /*resizable=*/false,
                                      &pages[0], &borrowing_vmo);
    ASSERT_EQ(status, ZX_OK);

    // Updates borrowing_seen to true, if any pages of vmo are seen to be borrowed (maybe by
    // borrowing_vmo, or maybe by some other VMO; we don't care which here).
    verify_expected_pages();

    // We want the last iteration of the loop to have seen borrowing itself, so we're sure the else
    // case below runs (to commit) before the borrowing VMO is deleted.
    if (loop_count < 5) {
      borrowed_seen = false;
    }

    if (!borrowed_seen || loop_count < 5) {
      if constexpr (!__has_feature(address_sanitizer)) {
        // By committing and de-committing in the loop, we put the pages we're paying attention to
        // back at the head of the free_loaned_list_, so the next iteration of the loop is more
        // likely to see them being borrowed (by allocating them).
        status = vmo->CommitRange(0, 4 * PAGE_SIZE);
        ASSERT_EQ(status, ZX_OK, "temp commit back to contiguous VMO, to remove from free list\n");
        track_commit(0, 4 * PAGE_SIZE);

        status = vmo->DecommitRange(0, 4 * PAGE_SIZE);
        ASSERT_EQ(status, ZX_OK, "decommit back to free list at head of free list\n");
        track_decommit(0, 4 * PAGE_SIZE);
      } else {
        // By _not_ committing and de-committing in the loop, the pages we're allocating in a loop
        // will eventually work through the free_loaned_list_, even if a large contiguous VMO was
        // decomitted at an inconvenient time.
      }
      zx_instant_mono_t now = current_mono_time();
      if (now > complain_deadline) {
        dprintf(INFO, "!borrowed_seen is persisting longer than expected; still trying...\n");
        complain_deadline = now + ZX_SEC(5);
      }
    } else {
      // This covers the case where a page is reclaimed before being freed from the borrowing VMO.
      // And by forcing an iteration with loop_count >= 1 with the last iteration of the loop seeing
      // borrowing durign the last iteration, we cover the case where we free the pages from the
      // borrowing VMO before reclaiming.
      status = vmo->CommitRange(0, alloc_size);
      ASSERT_EQ(status, ZX_OK, "committed pages back into contiguous VMO\n");
      track_commit(0, alloc_size);
    }
    ++loop_count;
  }

  status = vmo->DecommitRange(0, alloc_size);
  ASSERT_EQ(status, ZX_OK, "decommit from contiguous VMO\n");

  status = vmo->CommitRange(0, alloc_size);
  ASSERT_EQ(status, ZX_OK, "committed pages back into contiguous VMO\n");
  track_commit(0, alloc_size);

  END_TEST;
}

static bool vmo_contiguous_decommit_disabled_test() {
  BEGIN_TEST;

  bool loaning_was_enabled = pmm_physical_page_borrowing_config()->is_loaning_enabled();
  pmm_physical_page_borrowing_config()->set_loaning_enabled(false);
  auto cleanup = fit::defer([loaning_was_enabled] {
    pmm_physical_page_borrowing_config()->set_loaning_enabled(loaning_was_enabled);
  });

  static const size_t alloc_size = PAGE_SIZE * 16;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::CreateContiguous(PMM_ALLOC_FLAG_ANY, alloc_size, 0, &vmo);
  ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");

  status = vmo->DecommitRange(PAGE_SIZE, 4 * PAGE_SIZE);
  ASSERT_EQ(status, ZX_ERR_NOT_SUPPORTED, "decommit fails as expected\n");
  status = vmo->DecommitRange(0, 4 * PAGE_SIZE);
  ASSERT_EQ(status, ZX_ERR_NOT_SUPPORTED, "decommit fails as expected\n");
  status = vmo->DecommitRange(alloc_size - PAGE_SIZE, PAGE_SIZE);
  ASSERT_EQ(status, ZX_ERR_NOT_SUPPORTED, "decommit fails as expected\n");

  END_TEST;
}

static bool vmo_contiguous_decommit_enabled_test() {
  BEGIN_TEST;

  bool loaning_was_enabled = pmm_physical_page_borrowing_config()->is_loaning_enabled();
  pmm_physical_page_borrowing_config()->set_loaning_enabled(true);
  auto cleanup = fit::defer([loaning_was_enabled] {
    pmm_physical_page_borrowing_config()->set_loaning_enabled(loaning_was_enabled);
  });

  static const size_t alloc_size = PAGE_SIZE * 16;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::CreateContiguous(PMM_ALLOC_FLAG_ANY, alloc_size, 0, &vmo);
  ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");

  // Scope the memsetting so that the kernel mapping does not keep existing to the point that the
  // Decommits happen below. As those decommits would need to perform unmaps, and we prefer to not
  // modify kernel mappings in this way, we just remove the kernel region.
  {
    auto ka = VmAspace::kernel_aspace();
    void* ptr;
    auto ret = ka->MapObjectInternal(vmo, "test", 0, alloc_size, &ptr, 0, VmAspace::VMM_FLAG_COMMIT,
                                     kArchRwFlags);
    ASSERT_EQ(ZX_OK, ret, "mapping object");
    auto cleanup_mapping = fit::defer([&ka, ptr] {
      auto err = ka->FreeRegion((vaddr_t)ptr);
      DEBUG_ASSERT(err == ZX_OK);
    });
    uint8_t* base = reinterpret_cast<uint8_t*>(ptr);

    for (uint64_t offset = 0; offset < alloc_size; offset += PAGE_SIZE) {
      memset(&base[offset], 0x42, PAGE_SIZE);
    }
  }

  paddr_t base_pa = -1;
  status = vmo->Lookup(0, PAGE_SIZE, [&base_pa](uint64_t offset, paddr_t pa) {
    DEBUG_ASSERT(offset == 0);
    base_pa = pa;
    return ZX_ERR_NEXT;
  });
  ASSERT_EQ(status, ZX_OK);
  ASSERT_TRUE(base_pa != static_cast<paddr_t>(-1));

  status = vmo->DecommitRange(PAGE_SIZE, 4 * PAGE_SIZE);
  ASSERT_EQ(status, ZX_OK, "decommit pretends to work\n");
  status = vmo->DecommitRange(0, 4 * PAGE_SIZE);
  ASSERT_EQ(status, ZX_OK, "decommit pretends to work\n");
  status = vmo->DecommitRange(alloc_size - PAGE_SIZE, PAGE_SIZE);
  ASSERT_EQ(status, ZX_OK, "decommit pretends to work\n");

  // Make sure decommit removed pages.  Make sure pages which are present are the correct physical
  // address.
  for (uint64_t offset = 0; offset < alloc_size; offset += PAGE_SIZE) {
    bool page_absent = true;
    status = vmo->Lookup(offset, PAGE_SIZE,
                         [base_pa, offset, &page_absent](uint64_t lookup_offset, paddr_t pa) {
                           // TODO(johngro): remove this explicit unused-capture warning suppression
                           // when https://bugs.llvm.org/show_bug.cgi?id=35450 gets fixed.
                           (void)base_pa;
                           (void)offset;

                           page_absent = false;
                           DEBUG_ASSERT(offset == lookup_offset);
                           DEBUG_ASSERT(base_pa + lookup_offset == pa);
                           return ZX_ERR_NEXT;
                         });
    bool absent_expected = (offset < 5 * PAGE_SIZE) || (offset == alloc_size - PAGE_SIZE);
    ASSERT_EQ(absent_expected, page_absent);
  }

  END_TEST;
}

// Creats a vm object, maps it, precommitted.
static bool vmo_precommitted_map_test() {
  BEGIN_TEST;
  static const size_t alloc_size = PAGE_SIZE * 16;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, alloc_size, &vmo);
  ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");

  auto ka = VmAspace::kernel_aspace();
  void* ptr;
  auto ret = ka->MapObjectInternal(vmo, "test", 0, alloc_size, &ptr, 0, VmAspace::VMM_FLAG_COMMIT,
                                   kArchRwFlags);
  ASSERT_EQ(ZX_OK, ret, "mapping object");

  // fill with known pattern and test
  if (!fill_and_test(ptr, alloc_size)) {
    all_ok = false;
  }

  auto err = ka->FreeRegion((vaddr_t)ptr);
  EXPECT_EQ(ZX_OK, err, "unmapping object");
  END_TEST;
}

// Creates a vm object, maps it, demand paged.
static bool vmo_demand_paged_map_test() {
  BEGIN_TEST;

  static const size_t alloc_size = PAGE_SIZE * 16;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, alloc_size, &vmo);
  ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");

  fbl::RefPtr<VmAspace> aspace = VmAspace::Create(VmAspace::Type::User, "test aspace");
  ASSERT_NONNULL(aspace, "VmAspace::Create pointer");

  VmAspace* old_aspace = Thread::Current::active_aspace();
  auto cleanup_aspace = fit::defer([&]() {
    vmm_set_active_aspace(old_aspace);
    ASSERT(aspace->Destroy() == ZX_OK);
  });
  vmm_set_active_aspace(aspace.get());

  static constexpr const uint kArchFlags = kArchRwFlags | ARCH_MMU_FLAG_PERM_USER;
  auto mapping_result =
      aspace->RootVmar()->CreateVmMapping(0, alloc_size, 0, 0, vmo, 0, kArchFlags, "test");
  ASSERT_MSG(mapping_result.is_ok(), "mapping object");

  auto uptr = make_user_inout_ptr(reinterpret_cast<void*>(mapping_result->base));

  // fill with known pattern and test
  if (!fill_and_test_user(uptr, alloc_size)) {
    all_ok = false;
  }

  // cleanup_aspace destroys the whole space now.

  END_TEST;
}

// Creates a vm object, maps it, drops ref before unmapping.
static bool vmo_dropped_ref_test() {
  BEGIN_TEST;
  static const size_t alloc_size = PAGE_SIZE * 16;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, alloc_size, &vmo);
  ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");

  auto ka = VmAspace::kernel_aspace();
  void* ptr;
  auto ret = ka->MapObjectInternal(ktl::move(vmo), "test", 0, alloc_size, &ptr, 0,
                                   VmAspace::VMM_FLAG_COMMIT, kArchRwFlags);
  ASSERT_EQ(ret, ZX_OK, "mapping object");

  EXPECT_NULL(vmo, "dropped ref to object");

  // fill with known pattern and test
  if (!fill_and_test(ptr, alloc_size)) {
    all_ok = false;
  }

  auto err = ka->FreeRegion((vaddr_t)ptr);
  EXPECT_EQ(ZX_OK, err, "unmapping object");
  END_TEST;
}

// Creates a vm object, maps it, fills it with data, unmaps,
// maps again somewhere else.
static bool vmo_remap_test() {
  BEGIN_TEST;
  static const size_t alloc_size = PAGE_SIZE * 16;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, alloc_size, &vmo);
  ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");

  auto ka = VmAspace::kernel_aspace();
  void* ptr;
  auto ret = ka->MapObjectInternal(vmo, "test", 0, alloc_size, &ptr, 0, VmAspace::VMM_FLAG_COMMIT,
                                   kArchRwFlags);
  ASSERT_EQ(ZX_OK, ret, "mapping object");

  // fill with known pattern and test.  The initial virtual address will be used
  // to generate the seed which is used to generate the fill pattern.  Make sure
  // we save it off right now to use when we test the fill pattern later on
  // after re-mapping.
  const uintptr_t fill_seed = reinterpret_cast<uintptr_t>(ptr);
  if (!fill_and_test(ptr, alloc_size)) {
    all_ok = false;
  }

  auto err = ka->FreeRegion((vaddr_t)ptr);
  EXPECT_EQ(ZX_OK, err, "unmapping object");

  // map it again
  ret = ka->MapObjectInternal(vmo, "test", 0, alloc_size, &ptr, 0, VmAspace::VMM_FLAG_COMMIT,
                              kArchRwFlags);
  ASSERT_EQ(ret, ZX_OK, "mapping object");

  // test that the pattern is still valid.  Be sure to use the original seed we
  // saved off earlier when verifying.
  bool result = test_region(fill_seed, ptr, alloc_size);
  EXPECT_TRUE(result, "testing region for corruption");

  err = ka->FreeRegion((vaddr_t)ptr);
  EXPECT_EQ(ZX_OK, err, "unmapping object");
  END_TEST;
}

// Creates a vm object, maps it, fills it with data, maps it a second time and
// third time somwehere else.
static bool vmo_double_remap_test() {
  BEGIN_TEST;
  static const size_t alloc_size = PAGE_SIZE * 16;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, alloc_size, &vmo);
  ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");

  auto ka = VmAspace::kernel_aspace();
  void* ptr;
  auto ret = ka->MapObjectInternal(vmo, "test0", 0, alloc_size, &ptr, 0, VmAspace::VMM_FLAG_COMMIT,
                                   kArchRwFlags);
  ASSERT_EQ(ZX_OK, ret, "mapping object");

  // fill with known pattern and test
  if (!fill_and_test(ptr, alloc_size)) {
    all_ok = false;
  }

  // map it again
  void* ptr2;
  ret = ka->MapObjectInternal(vmo, "test1", 0, alloc_size, &ptr2, 0, VmAspace::VMM_FLAG_COMMIT,
                              kArchRwFlags);
  ASSERT_EQ(ret, ZX_OK, "mapping object second time");
  EXPECT_NE(ptr, ptr2, "second mapping is different");

  // test that the pattern is still valid
  bool result = test_region((uintptr_t)ptr, ptr2, alloc_size);
  EXPECT_TRUE(result, "testing region for corruption");

  // map it a third time with an offset
  void* ptr3;
  static const size_t alloc_offset = PAGE_SIZE;
  ret = ka->MapObjectInternal(vmo, "test2", alloc_offset, alloc_size - alloc_offset, &ptr3, 0,
                              VmAspace::VMM_FLAG_COMMIT, kArchRwFlags);
  ASSERT_EQ(ret, ZX_OK, "mapping object third time");
  EXPECT_NE(ptr3, ptr2, "third mapping is different");
  EXPECT_NE(ptr3, ptr, "third mapping is different");

  // test that the pattern is still valid
  int mc = memcmp((uint8_t*)ptr + alloc_offset, ptr3, alloc_size - alloc_offset);
  EXPECT_EQ(0, mc, "testing region for corruption");

  ret = ka->FreeRegion((vaddr_t)ptr3);
  EXPECT_EQ(ZX_OK, ret, "unmapping object third time");

  ret = ka->FreeRegion((vaddr_t)ptr2);
  EXPECT_EQ(ZX_OK, ret, "unmapping object second time");

  ret = ka->FreeRegion((vaddr_t)ptr);
  EXPECT_EQ(ZX_OK, ret, "unmapping object");
  END_TEST;
}

static bool vmo_read_write_smoke_test() {
  BEGIN_TEST;
  static const size_t alloc_size = PAGE_SIZE * 16;

  // create object
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, alloc_size, &vmo);
  ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");

  // create test buffer
  fbl::AllocChecker ac;
  fbl::Vector<uint8_t> a;
  a.reserve(alloc_size, &ac);
  ASSERT_TRUE(ac.check());
  fill_region(99, a.data(), alloc_size);

  // write to it, make sure it seems to work with valid args
  zx_status_t err = vmo->Write(a.data(), 0, 0);
  EXPECT_EQ(ZX_OK, err, "writing to object");

  err = vmo->Write(a.data(), 0, 37);
  EXPECT_EQ(ZX_OK, err, "writing to object");

  err = vmo->Write(a.data(), 99, 37);
  EXPECT_EQ(ZX_OK, err, "writing to object");

  // can't write past end
  err = vmo->Write(a.data(), 0, alloc_size + 47);
  EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, err, "writing to object");

  // can't write past end
  err = vmo->Write(a.data(), 31, alloc_size + 47);
  EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, err, "writing to object");

  // should return an error because out of range
  err = vmo->Write(a.data(), alloc_size + 99, 42);
  EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, err, "writing to object");

  // map the object
  auto ka = VmAspace::kernel_aspace();
  uint8_t* ptr;
  err = ka->MapObjectInternal(vmo, "test", 0, alloc_size, (void**)&ptr, 0,
                              VmAspace::VMM_FLAG_COMMIT, kArchRwFlags);
  ASSERT_EQ(ZX_OK, err, "mapping object");

  // write to it at odd offsets
  err = vmo->Write(a.data(), 31, 4197);
  EXPECT_EQ(ZX_OK, err, "writing to object");
  int cmpres = memcmp(ptr + 31, a.data(), 4197);
  EXPECT_EQ(0, cmpres, "reading from object");

  // write to it, filling the object completely
  err = vmo->Write(a.data(), 0, alloc_size);
  EXPECT_EQ(ZX_OK, err, "writing to object");

  // test that the data was actually written to it
  bool result = test_region(99, ptr, alloc_size);
  EXPECT_TRUE(result, "writing to object");

  // unmap it
  ka->FreeRegion((vaddr_t)ptr);

  // test that we can read from it
  fbl::Vector<uint8_t> b;
  b.reserve(alloc_size, &ac);
  ASSERT_TRUE(ac.check(), "can't allocate buffer");

  err = vmo->Read(b.data(), 0, alloc_size);
  EXPECT_EQ(ZX_OK, err, "reading from object");

  // validate the buffer is valid
  cmpres = memcmp(b.data(), a.data(), alloc_size);
  EXPECT_EQ(0, cmpres, "reading from object");

  // read from it at an offset
  err = vmo->Read(b.data(), 31, 4197);
  EXPECT_EQ(ZX_OK, err, "reading from object");
  cmpres = memcmp(b.data(), a.data() + 31, 4197);
  EXPECT_EQ(0, cmpres, "reading from object");
  END_TEST;
}

static bool vmo_cache_test() {
  BEGIN_TEST;

  paddr_t pa;
  vm_page_t* vm_page;
  zx_status_t status = pmm_alloc_page(0, &vm_page, &pa);
  auto ka = VmAspace::kernel_aspace();
  uint32_t cache_policy = ARCH_MMU_FLAG_UNCACHED_DEVICE;
  uint32_t cache_policy_get;
  void* ptr;

  ASSERT_TRUE(vm_page);
  // Test that the flags set/get properly
  {
    fbl::RefPtr<VmObjectPhysical> vmo;
    status = VmObjectPhysical::Create(pa, PAGE_SIZE, &vmo);
    ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
    ASSERT_TRUE(vmo, "vmobject creation\n");
    cache_policy_get = vmo->GetMappingCachePolicy();
    EXPECT_NE(cache_policy, cache_policy_get, "check initial cache policy");
    EXPECT_EQ(ZX_OK, vmo->SetMappingCachePolicy(cache_policy), "try set");
    cache_policy_get = vmo->GetMappingCachePolicy();
    EXPECT_EQ(cache_policy, cache_policy_get, "compare flags");
  }

  // Test valid flags
  for (uint32_t i = 0; i <= ARCH_MMU_FLAG_CACHE_MASK; i++) {
    fbl::RefPtr<VmObjectPhysical> vmo;
    status = VmObjectPhysical::Create(pa, PAGE_SIZE, &vmo);
    ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
    ASSERT_TRUE(vmo, "vmobject creation\n");
    EXPECT_EQ(ZX_OK, vmo->SetMappingCachePolicy(cache_policy), "try setting valid flags");
  }

  // Test invalid flags
  for (uint32_t i = ARCH_MMU_FLAG_CACHE_MASK + 1; i < 32; i++) {
    fbl::RefPtr<VmObjectPhysical> vmo;
    status = VmObjectPhysical::Create(pa, PAGE_SIZE, &vmo);
    ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
    ASSERT_TRUE(vmo, "vmobject creation\n");
    EXPECT_EQ(ZX_ERR_INVALID_ARGS, vmo->SetMappingCachePolicy(i), "try set with invalid flags");
  }

  // Test valid flags with invalid flags
  {
    fbl::RefPtr<VmObjectPhysical> vmo;
    status = VmObjectPhysical::Create(pa, PAGE_SIZE, &vmo);
    ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
    ASSERT_TRUE(vmo, "vmobject creation\n");
    EXPECT_EQ(ZX_ERR_INVALID_ARGS, vmo->SetMappingCachePolicy(cache_policy | 0x5), "bad 0x5");
    EXPECT_EQ(ZX_ERR_INVALID_ARGS, vmo->SetMappingCachePolicy(cache_policy | 0xA), "bad 0xA");
    EXPECT_EQ(ZX_ERR_INVALID_ARGS, vmo->SetMappingCachePolicy(cache_policy | 0x55), "bad 0x55");
    EXPECT_EQ(ZX_ERR_INVALID_ARGS, vmo->SetMappingCachePolicy(cache_policy | 0xAA), "bad 0xAA");
  }

  // Test that changing policy while mapped is blocked
  {
    fbl::RefPtr<VmObjectPhysical> vmo;
    status = VmObjectPhysical::Create(pa, PAGE_SIZE, &vmo);
    ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
    ASSERT_TRUE(vmo, "vmobject creation\n");
    ASSERT_EQ(ZX_OK,
              ka->MapObjectInternal(vmo, "test", 0, PAGE_SIZE, (void**)&ptr, 0,
                                    VmAspace::VMM_FLAG_COMMIT, kArchRwFlags),
              "map vmo");
    EXPECT_EQ(ZX_ERR_BAD_STATE, vmo->SetMappingCachePolicy(cache_policy), "set flags while mapped");
    EXPECT_EQ(ZX_OK, ka->FreeRegion((vaddr_t)ptr), "unmap vmo");
    EXPECT_EQ(ZX_OK, vmo->SetMappingCachePolicy(cache_policy), "set flags after unmapping");
    ASSERT_EQ(ZX_OK,
              ka->MapObjectInternal(vmo, "test", 0, PAGE_SIZE, (void**)&ptr, 0,
                                    VmAspace::VMM_FLAG_COMMIT, kArchRwFlags),
              "map vmo again");
    EXPECT_EQ(ZX_OK, ka->FreeRegion((vaddr_t)ptr), "unmap vmo");
  }

  pmm_free_page(vm_page);
  END_TEST;
}

static bool vmo_lookup_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  static const size_t alloc_size = PAGE_SIZE * 16;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, alloc_size, &vmo);
  ASSERT_EQ(status, ZX_OK, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");

  size_t pages_seen = 0;
  auto lookup_fn = [&pages_seen](size_t offset, paddr_t pa) {
    pages_seen++;
    return ZX_ERR_NEXT;
  };
  status = vmo->Lookup(0, alloc_size, lookup_fn);
  EXPECT_EQ(ZX_OK, status);
  EXPECT_EQ(0u, pages_seen, "lookup on uncommitted pages\n");
  pages_seen = 0;

  status = vmo->CommitRange(PAGE_SIZE, PAGE_SIZE);
  EXPECT_EQ(ZX_OK, status, "committing vm object\n");
  EXPECT_TRUE(make_private_attribution_counts(PAGE_SIZE, 0) == vmo->GetAttributedMemory(),
              "committing vm object\n");

  // Should not see any pages in the early range.
  status = vmo->Lookup(0, PAGE_SIZE, lookup_fn);
  EXPECT_EQ(ZX_OK, status);
  EXPECT_EQ(0u, pages_seen, "lookup on partially committed pages\n");
  pages_seen = 0;

  // Should see a committed page if looking at any range covering the committed.
  status = vmo->Lookup(0, alloc_size, lookup_fn);
  EXPECT_EQ(ZX_OK, status);
  EXPECT_EQ(1u, pages_seen, "lookup on partially committed pages\n");
  pages_seen = 0;

  status = vmo->Lookup(PAGE_SIZE, alloc_size - PAGE_SIZE, lookup_fn);
  EXPECT_EQ(ZX_OK, status);
  EXPECT_EQ(1u, pages_seen, "lookup on partially committed pages\n");
  pages_seen = 0;

  status = vmo->Lookup(PAGE_SIZE, PAGE_SIZE, lookup_fn);
  EXPECT_EQ(ZX_OK, status);
  EXPECT_EQ(1u, pages_seen, "lookup on partially committed pages\n");
  pages_seen = 0;

  // Contiguous lookups of single pages should also succeed
  status = vmo->LookupContiguous(PAGE_SIZE, PAGE_SIZE, nullptr);
  EXPECT_EQ(ZX_OK, status, "contiguous lookup of single page\n");

  // Commit the rest
  status = vmo->CommitRange(0, alloc_size);
  EXPECT_EQ(ZX_OK, status, "committing vm object\n");
  EXPECT_TRUE(make_private_attribution_counts(alloc_size, 0) == vmo->GetAttributedMemory(),
              "committing vm object\n");

  status = vmo->Lookup(0, alloc_size, lookup_fn);
  EXPECT_EQ(ZX_OK, status, "lookup on partially committed pages\n");
  EXPECT_EQ(alloc_size / PAGE_SIZE, pages_seen, "lookup on partially committed pages\n");
  status = vmo->LookupContiguous(0, PAGE_SIZE, nullptr);
  EXPECT_EQ(ZX_OK, status, "contiguous lookup of single page\n");
  status = vmo->LookupContiguous(0, alloc_size, nullptr);
  EXPECT_NE(ZX_OK, status, "contiguous lookup of multiple pages\n");

  END_TEST;
}

static bool vmo_lookup_slice_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  constexpr size_t kAllocSize = PAGE_SIZE * 16;
  constexpr size_t kCommitOffset = PAGE_SIZE * 4;
  constexpr size_t kSliceOffset = PAGE_SIZE;
  constexpr size_t kSliceSize = kAllocSize - kSliceOffset;
  fbl::RefPtr<VmObjectPaged> vmo;
  ASSERT_OK(VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, kAllocSize, &vmo));
  ASSERT_TRUE(vmo);

  // Commit a page in the vmo.
  ASSERT_OK(vmo->CommitRange(kCommitOffset, PAGE_SIZE));

  // Create a slice that is offset slightly.
  fbl::RefPtr<VmObject> slice;
  ASSERT_OK(vmo->CreateChildSlice(kSliceOffset, kSliceSize, false, &slice));
  ASSERT_TRUE(slice);

  // Query the slice and validate we see one page at the offset relative to us, not the parent it is
  // committed in.
  uint64_t offset_seen = UINT64_MAX;

  auto lookup_fn = [&offset_seen](size_t offset, paddr_t pa) {
    ASSERT(offset_seen == UINT64_MAX);
    offset_seen = offset;
    return ZX_ERR_NEXT;
  };
  EXPECT_OK(slice->Lookup(0, kSliceSize, lookup_fn));

  EXPECT_EQ(offset_seen, kCommitOffset - kSliceOffset);

  END_TEST;
}

static bool vmo_lookup_clone_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  static const size_t page_count = 4;
  static const size_t alloc_size = PAGE_SIZE * page_count;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, alloc_size, &vmo);
  ASSERT_EQ(ZX_OK, status, "vmobject creation\n");
  ASSERT_TRUE(vmo, "vmobject creation\n");

  vmo->set_user_id(ZX_KOID_KERNEL);

  // Commit the whole original VMO and the first and last page of the clone.
  status = vmo->CommitRange(0, alloc_size);
  ASSERT_EQ(ZX_OK, status, "vmobject creation\n");

  fbl::RefPtr<VmObject> clone;
  status = vmo->CreateClone(Resizability::NonResizable, SnapshotType::Full, 0, alloc_size, false,
                            &clone);
  ASSERT_EQ(ZX_OK, status, "vmobject creation\n");
  ASSERT_TRUE(clone, "vmobject creation\n");

  clone->set_user_id(ZX_KOID_KERNEL);

  status = clone->CommitRange(0, PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status, "vmobject creation\n");
  status = clone->CommitRange(alloc_size - PAGE_SIZE, PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status, "vmobject creation\n");

  // Lookup the paddrs for both VMOs.
  paddr_t vmo_lookup[page_count] = {};
  paddr_t clone_lookup[page_count] = {};
  auto vmo_lookup_func = [&vmo_lookup](uint64_t offset, paddr_t pa) {
    vmo_lookup[offset / PAGE_SIZE] = pa;
    return ZX_ERR_NEXT;
  };
  auto clone_lookup_func = [&clone_lookup](uint64_t offset, paddr_t pa) {
    clone_lookup[offset / PAGE_SIZE] = pa;
    return ZX_ERR_NEXT;
  };
  status = vmo->Lookup(0, alloc_size, vmo_lookup_func);
  EXPECT_EQ(ZX_OK, status, "vmo lookup\n");
  status = clone->Lookup(0, alloc_size, clone_lookup_func);
  EXPECT_EQ(ZX_OK, status, "vmo lookup\n");

  // The original VMO is now copy-on-write so we should see none of its pages,
  // and we should only see the two pages that explicitly committed into the clone.
  for (unsigned i = 0; i < page_count; i++) {
    EXPECT_EQ(0ul, vmo_lookup[i], "Bad paddr\n");
    if (i == 0 || i == page_count - 1) {
      EXPECT_NE(0ul, clone_lookup[i], "Bad paddr\n");
    }
  }

  END_TEST;
}

static bool vmo_clone_removes_write_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  // Create and map a VMO.
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, PAGE_SIZE, &vmo);
  EXPECT_EQ(ZX_OK, status, "vmo create");

  // Use UserMemory to map the VMO, instead of mapping into the kernel aspace, so that we can freely
  // cause the mappings to modified as a consequence of the clone operation. Causing kernel mappings
  // to get modified in such a way is preferably avoided.
  ktl::unique_ptr<testing::UserMemory> mapping = testing::UserMemory::Create(vmo);
  ASSERT_NONNULL(mapping);
  status = mapping->CommitAndMap(PAGE_SIZE);
  EXPECT_OK(status);

  // Query the aspace and validate there is a writable mapping.
  paddr_t paddr_writable;
  uint mmu_flags;
  status = mapping->aspace()->arch_aspace().Query(mapping->base(), &paddr_writable, &mmu_flags);
  EXPECT_EQ(ZX_OK, status, "query aspace");

  EXPECT_TRUE(mmu_flags & ARCH_MMU_FLAG_PERM_WRITE, "mapping is writable check");

  // Clone the VMO, which causes the parent to have to downgrade any mappings to read-only so that
  // copy-on-write can take place. Need to set a fake user id so that the COW creation code is
  // happy.
  vmo->set_user_id(42);
  fbl::RefPtr<VmObject> clone;
  status =
      vmo->CreateClone(Resizability::NonResizable, SnapshotType::Full, 0, PAGE_SIZE, true, &clone);
  EXPECT_EQ(ZX_OK, status, "create clone");

  // Aspace should now have a read only mapping with the same underlying page.
  paddr_t paddr_readable;
  status = mapping->aspace()->arch_aspace().Query(mapping->base(), &paddr_readable, &mmu_flags);
  EXPECT_EQ(ZX_OK, status, "query aspace");
  EXPECT_FALSE(mmu_flags & ARCH_MMU_FLAG_PERM_WRITE, "mapping is read only check");
  EXPECT_EQ(paddr_writable, paddr_readable, "mapping has same page");

  END_TEST;
}

// Test that when creating or destroying clones that compressed pages, even if forked, do not need
// to get unnecessarily uncompressed.
static bool vmo_clones_of_compressed_pages_test() {
  BEGIN_TEST;

  // Need a compressor.
  auto compression = Pmm::Node().GetPageCompression();
  if (!compression) {
    END_TEST;
  }

  AutoVmScannerDisable scanner_disable;

  // Create a VMO and make one of its pages compressed.
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, PAGE_SIZE, &vmo);
  ASSERT_OK(status);
  // Set ids so that attribution can work correctly.
  vmo->set_user_id(42);

  status = vmo->CommitRange(0, PAGE_SIZE);
  EXPECT_OK(status);

  // Write non-zero data to the page.
  uint64_t data = 42;
  EXPECT_OK(vmo->Write(&data, 0, sizeof(data)));

  EXPECT_TRUE(make_private_attribution_counts(PAGE_SIZE, 0) == vmo->GetAttributedMemory());

  vm_page_t* page = nullptr;
  status = vmo->GetPageBlocking(0, 0, nullptr, &page, nullptr);
  ASSERT_OK(status);
  ASSERT_NONNULL(page);
  {
    auto compressor = compression->AcquireCompressor();
    ASSERT_OK(compressor.get().Arm());
    uint64_t reclaimed =
        reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, &compressor.get());
    EXPECT_EQ(reclaimed, 1u);
  }
  page = nullptr;
  EXPECT_TRUE(make_private_attribution_counts(0, PAGE_SIZE) == vmo->GetAttributedMemory());

  // Creating a clone should keep the page compressed.
  fbl::RefPtr<VmObject> clone;
  status =
      vmo->CreateClone(Resizability::NonResizable, SnapshotType::Full, 0, PAGE_SIZE, true, &clone);
  ASSERT_OK(status);
  clone->set_user_id(43);
  EXPECT_TRUE((VmObject::AttributionCounts{.compressed_bytes = PAGE_SIZE,
                                           .scaled_compressed_bytes = vm::FractionalBytes(
                                               PAGE_SIZE, 2)}) == vmo->GetAttributedMemory());
  EXPECT_TRUE((VmObject::AttributionCounts{.compressed_bytes = PAGE_SIZE,
                                           .scaled_compressed_bytes = vm::FractionalBytes(
                                               PAGE_SIZE, 2)}) == clone->GetAttributedMemory());

  // Forking the page into a child will decompress in order to do the copy.
  status = clone->Write(&data, 0, sizeof(data));
  EXPECT_OK(status);
  EXPECT_TRUE(make_private_attribution_counts(PAGE_SIZE, 0) == vmo->GetAttributedMemory());
  EXPECT_TRUE(make_private_attribution_counts(PAGE_SIZE, 0) == clone->GetAttributedMemory());

  // Compress the parent page again by reaching into the hidden VMO parent.
  fbl::RefPtr<VmCowPages> hidden_root = vmo->DebugGetCowPages()->DebugGetParent();
  ASSERT_NONNULL(hidden_root);
  page = hidden_root->DebugGetPage(0);
  ASSERT_NONNULL(page);
  {
    auto compressor = compression->AcquireCompressor();

    ASSERT_OK(compressor.get().Arm());
    uint64_t reclaimed = reclaim_page(hidden_root, page, 0, VmCowPages::EvictionHintAction::Follow,
                                      &compressor.get());
    EXPECT_EQ(reclaimed, 1u);
  }
  page = nullptr;
  EXPECT_TRUE(make_private_attribution_counts(0, PAGE_SIZE) == vmo->GetAttributedMemory());
  EXPECT_TRUE(make_private_attribution_counts(PAGE_SIZE, 0) == clone->GetAttributedMemory());

  // Closing the child VMO should allow the now merged VMO to just have the compressed page without
  // causing it to be decompressed.
  clone.reset();
  EXPECT_TRUE(make_private_attribution_counts(0, PAGE_SIZE) == vmo->GetAttributedMemory());

  END_TEST;
}

// Test that CoW clones mapped into the kernel behave correctly if a 'parent' page gets compressed.
static bool vmo_clone_kernel_mapped_compressed_test() {
  BEGIN_TEST;

  // Need a compressor.
  auto compression = Pmm::Node().GetPageCompression();
  if (!compression) {
    END_TEST;
  }

  AutoVmScannerDisable scanner_disable;

  // Create a VMO and write to its page to commit it.
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, PAGE_SIZE, &vmo);
  ASSERT_OK(status);

  uint64_t data = 42;
  EXPECT_OK(vmo->Write(&data, 0, sizeof(data)));

  // Now create a child of the VMO and fork the page into it.
  fbl::RefPtr<VmObject> clone;
  ASSERT_OK(
      vmo->CreateClone(Resizability::NonResizable, SnapshotType::Full, 0, PAGE_SIZE, true, &clone));
  data = 41;
  EXPECT_OK(clone->Write(&data, 0, sizeof(data)));

  // Pin and map the clone into the kernel aspace.
  PinnedVmObject pinned_vmo;
  ASSERT_OK(PinnedVmObject::Create(clone, 0, PAGE_SIZE, true, &pinned_vmo));
  fbl::RefPtr<VmMapping> mapping;
  auto result = VmAspace::kernel_aspace()->RootVmar()->CreateVmMapping(
      0, PAGE_SIZE, 0, VMAR_FLAG_CAN_MAP_READ | VMAR_FLAG_CAN_MAP_WRITE, clone, 0,
      ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE, "pin clone map");
  ASSERT_TRUE(result.is_ok());
  mapping = ktl::move(result->mapping);
  auto cleanup = fit::defer([&]() { mapping->Destroy(); });
  ASSERT_OK(mapping->MapRange(0, PAGE_SIZE, true));
  volatile uint64_t* ptr = reinterpret_cast<volatile uint64_t*>(result->base);

  // Should be able to use ptr without a fault.
  EXPECT_EQ(*ptr, 41u);

  // Compress the parent page by reaching into the hidden VMO parent.
  fbl::RefPtr<VmCowPages> hidden_root = vmo->DebugGetCowPages()->DebugGetParent();
  ASSERT_NONNULL(hidden_root);
  vm_page_t* page = hidden_root->DebugGetPage(0);
  ASSERT_NONNULL(page);
  {
    auto compressor = compression->AcquireCompressor();
    ASSERT_OK(compressor.get().Arm());
    // Attempt to reclaim the page in the hidden parent. As the clone is a fully committed and
    // mapped VMO this should *not* cause any of its pages to be unmapped. If it did this violate
    // the requirement that kernel mappings are always pinned and mapped.
    uint64_t reclaimed = reclaim_page(hidden_root, page, 0, VmCowPages::EvictionHintAction::Follow,
                                      &compressor.get());
    EXPECT_EQ(reclaimed, 1u);
    page = nullptr;
  }

  // Should still be able to touch the ptr;
  EXPECT_EQ(*ptr, 41u);

  END_TEST;
}

static bool vmo_move_pages_on_access_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  fbl::RefPtr<VmObjectPaged> vmo;
  vm_page_t* page;
  zx_status_t status =
      make_committed_pager_vmo(1, /*trap_dirty=*/false, /*resizable=*/false, &page, &vmo);
  ASSERT_EQ(ZX_OK, status);

  // Our page should now be in a pager backed page queue.
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page));

  PageRequest request;
  // If we lookup the page then it should be moved to specifically the first page queue.
  status = vmo->GetPageBlocking(0, VMM_PF_FLAG_SW_FAULT, nullptr, nullptr, nullptr);
  EXPECT_EQ(ZX_OK, status);
  size_t queue;
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // Rotate the queues and check the page moves.
  pmm_page_queues()->RotateReclaimQueues();
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(1u, queue);

  // Touching the page should move it back to the first queue.
  status = vmo->GetPageBlocking(0, VMM_PF_FLAG_SW_FAULT, nullptr, nullptr, nullptr);
  EXPECT_EQ(ZX_OK, status);
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // Touching pages in a child should also move the page to the front of the queues.
  fbl::RefPtr<VmObject> child;
  status = vmo->CreateClone(Resizability::NonResizable, SnapshotType::OnWrite, 0, PAGE_SIZE, true,
                            &child);
  ASSERT_EQ(ZX_OK, status);

  status = child->GetPageBlocking(0, VMM_PF_FLAG_SW_FAULT, nullptr, nullptr, nullptr);
  EXPECT_EQ(ZX_OK, status);
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);
  pmm_page_queues()->RotateReclaimQueues();
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(1u, queue);
  status = child->GetPageBlocking(0, VMM_PF_FLAG_SW_FAULT, nullptr, nullptr, nullptr);
  EXPECT_EQ(ZX_OK, status);
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  END_TEST;
}

static bool vmo_eviction_hints_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

  // Create a pager-backed VMO with a single page.
  fbl::RefPtr<VmObjectPaged> vmo;
  vm_page_t* page;
  zx_status_t status =
      make_committed_pager_vmo(1, /*trap_dirty=*/false, /*resizable=*/false, &page, &vmo);
  ASSERT_EQ(ZX_OK, status);

  // Newly created page should be in the first pager backed page queue.
  size_t queue;
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // Hint that the page is not needed.
  ASSERT_OK(vmo->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::DontNeed));

  // The page should now have moved to the DontNeed queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaimDontNeed(page));

  // Hint that the page is always needed.
  ASSERT_OK(vmo->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::AlwaysNeed));

  // If the page was loaned, it will be replaced with a non-loaned page now.
  page = vmo->DebugGetPage(0);

  // The page should now have moved to the first LRU queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaimDontNeed(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // Evicting the page should fail.
  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);

  // Hint that the page is not needed again.
  ASSERT_OK(vmo->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::DontNeed));

  // HintRange() is allowed to replace the page.
  page = vmo->DebugGetPage(0);

  // The page should now have moved to the DontNeed queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaimDontNeed(page));

  // We should still not be able to evict the page, the AlwaysNeed hint is sticky.
  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);

  // Accessing the page should move it out of the DontNeed queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaimDontNeed(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // Verify that the page can be rotated as normal.
  pmm_page_queues()->RotateReclaimQueues();
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(1u, queue);

  // Touching the page should move it back to the first queue.
  status = vmo->GetPageBlocking(0, VMM_PF_FLAG_SW_FAULT, nullptr, nullptr, nullptr);
  EXPECT_EQ(ZX_OK, status);
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // We should still not be able to evict the page, the AlwaysNeed hint is sticky.
  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);

  // We should be able to evict the page when told to override the hint.
  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Ignore, nullptr), 1u);

  END_TEST;
}

static bool vmo_always_need_evicts_loaned_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  // Depending on which loaned page we get, it may not still be loaned at the time HintRange() is
  // called, so try a few times and make sure we see non-loaned after HintRange() for all the tries.
  const uint32_t kTryCount = 30;
  for (uint32_t try_ordinal = 0; try_ordinal < kTryCount; ++try_ordinal) {
    bool loaning_was_enabled = pmm_physical_page_borrowing_config()->is_loaning_enabled();
    bool borrowing_was_enabled =
        pmm_physical_page_borrowing_config()->is_borrowing_in_supplypages_enabled();
    pmm_physical_page_borrowing_config()->set_loaning_enabled(true);
    pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(true);
    auto cleanup = fit::defer([loaning_was_enabled, borrowing_was_enabled] {
      pmm_physical_page_borrowing_config()->set_loaning_enabled(loaning_was_enabled);
      pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(
          borrowing_was_enabled);
    });

    zx_status_t status;
    fbl::RefPtr<VmObjectPaged> vmo;
    vm_page_t* page;
    const uint32_t kPagesToLoan = 10;
    fbl::RefPtr<VmObjectPaged> contiguous_vmos[kPagesToLoan];
    uint32_t iteration_count = 0;
    const uint32_t kMaxIterations = 2000;
    do {
      // Before we call make_committed_pager_vmo(), we create a few 1-page contiguous VMOs and
      // decommit them, to increase the chance that make_committed_pager_vmo() picks up a loaned
      // page, so we'll get to replace that page during HintRange() below.  The decommit (loaning)
      // is best effort in case loaning is disabled.
      for (uint32_t i = 0; i < kPagesToLoan; ++i) {
        status = VmObjectPaged::CreateContiguous(PMM_ALLOC_FLAG_ANY, PAGE_SIZE,
                                                 /*alignment_log2=*/0, &contiguous_vmos[i]);
        ASSERT_EQ(ZX_OK, status);
        status = contiguous_vmos[i]->DecommitRange(0, PAGE_SIZE);
        ASSERT_TRUE(status == ZX_OK || status == ZX_ERR_NOT_SUPPORTED);
      }

      // Create a pager-backed VMO with a single page.
      status = make_committed_pager_vmo(1, /*trap_dirty=*/false, /*resizable=*/false, &page, &vmo);
      ASSERT_EQ(ZX_OK, status);
      ++iteration_count;
    } while (!page->is_loaned() && iteration_count < kMaxIterations);

    // If we hit this iteration count, something almost certainly went wrong...
    ASSERT_TRUE(iteration_count < kMaxIterations);

    // At this point we can't be absolutely certain that the page will stay loaned depending on
    // which loaned page we got, so we run this in an outer loop that tries this a few times.

    // Hint that the page is always needed.
    ASSERT_OK(vmo->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::AlwaysNeed));

    // If the page was still loaned, it will be replaced with a non-loaned page now.
    page = vmo->DebugGetPage(0);

    ASSERT_FALSE(page->is_loaned());
  }

  END_TEST;
}

static bool vmo_eviction_hints_clone_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

  // Create a pager-backed VMO with two pages. We will fork a page in a clone later.
  fbl::RefPtr<VmObjectPaged> vmo;
  vm_page_t* pages[2];
  zx_status_t status =
      make_committed_pager_vmo(2, /*trap_dirty=*/false, /*resizable=*/false, pages, &vmo);
  ASSERT_EQ(ZX_OK, status);

  // Newly created pages should be in the first pager backed page queue.
  size_t queue;
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(pages[0], &queue));
  EXPECT_EQ(0u, queue);
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(pages[1], &queue));
  EXPECT_EQ(0u, queue);

  // Create a clone.
  fbl::RefPtr<VmObject> clone;
  status = vmo->CreateClone(Resizability::NonResizable, SnapshotType::OnWrite, 0, 2 * PAGE_SIZE,
                            true, &clone);
  ASSERT_EQ(ZX_OK, status);

  // Use the clone to perform a bunch of hinting operations on the first page.
  // Hint that the page is not needed.
  ASSERT_OK(clone->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::DontNeed));

  // The page should now have moved to the DontNeed queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(pages[0]));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaimDontNeed(pages[0]));

  // Hint that the page is always needed.
  ASSERT_OK(clone->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::AlwaysNeed));

  // If the page was loaned, it will be replaced with a non-loaned page now.
  pages[0] = vmo->DebugGetPage(0);

  // The page should now have moved to the first LRU queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaimDontNeed(pages[0]));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(pages[0], &queue));
  EXPECT_EQ(0u, queue);

  // Evicting the page should fail.
  ASSERT_EQ(reclaim_page(vmo, pages[0], 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);

  // Hinting should also work via a clone of a clone.
  fbl::RefPtr<VmObject> clone2;
  status = clone->CreateClone(Resizability::NonResizable, SnapshotType::OnWrite, 0, 2 * PAGE_SIZE,
                              true, &clone2);
  ASSERT_EQ(ZX_OK, status);

  // Hint that the page is not needed.
  ASSERT_OK(clone2->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::DontNeed));

  // The page should now have moved to the DontNeed queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(pages[0]));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaimDontNeed(pages[0]));

  // Hint that the page is always needed.
  ASSERT_OK(clone2->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::AlwaysNeed));

  // If the page was loaned, it will be replaced with a non-loaned page now.
  pages[0] = vmo->DebugGetPage(0);

  // The page should now have moved to the first LRU queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaimDontNeed(pages[0]));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(pages[0], &queue));
  EXPECT_EQ(0u, queue);

  // Evicting the page should fail.
  ASSERT_EQ(reclaim_page(vmo, pages[0], 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);

  // Verify that hinting still works via the parent VMO.
  // Hint that the page is not needed again.
  ASSERT_OK(vmo->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::DontNeed));

  // The page should now have moved to the DontNeed queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(pages[0]));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaimDontNeed(pages[0]));

  // Fork the page in the clone. And make sure hints no longer apply.
  uint64_t data = 0xff;
  clone->Write(&data, 0, sizeof(data));
  EXPECT_TRUE(make_private_attribution_counts(PAGE_SIZE, 0) == clone->GetAttributedMemory());

  // The write will have moved the page to the first page queue, because the page is still accessed
  // in order to perform the fork. So hint using the parent again to move to the DontNeed queue.
  ASSERT_OK(vmo->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::DontNeed));

  // The page should now have moved to the DontNeed queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(pages[0]));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaimDontNeed(pages[0]));

  // Hint that the page is always needed via the clone.
  ASSERT_OK(clone->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::AlwaysNeed));

  // The page should still be in the DontNeed queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(pages[0]));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaimDontNeed(pages[0]));

  // Hint that the page is always needed via the second level clone.
  ASSERT_OK(clone2->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::AlwaysNeed));

  // This should move the page out of the the DontNeed queue. Since we forked the page in the
  // intermediate clone *after* this clone was created, it will still refer to the original page,
  // which is the same as the page in the root.
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(pages[0]));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaimDontNeed(pages[0]));

  // Create another clone that sees the forked page.
  // Hinting through this clone should have no effect, since it will see the forked page.
  fbl::RefPtr<VmObject> clone3;
  status = clone->CreateClone(Resizability::NonResizable, SnapshotType::OnWrite, 0, 2 * PAGE_SIZE,
                              true, &clone3);
  ASSERT_EQ(ZX_OK, status);

  // Move the page back to the DontNeed queue first.
  ASSERT_OK(vmo->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::DontNeed));

  // The page should now have moved to the DontNeed queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(pages[0]));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaimDontNeed(pages[0]));

  // Hint through clone3.
  ASSERT_OK(clone3->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::AlwaysNeed));

  // The page should still be in the DontNeed queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(pages[0]));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaimDontNeed(pages[0]));

  // Hint on the second page using clone3. This page hasn't been forked by the intermediate clone.
  // So clone3 should still be able to see the root page.
  // First verify that the page is still in queue 0.
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(pages[1], &queue));
  EXPECT_EQ(0u, queue);

  // Hint DontNeed through clone 3.
  ASSERT_OK(clone3->HintRange(PAGE_SIZE, PAGE_SIZE, VmObject::EvictionHint::DontNeed));

  // The page should have moved to the DontNeed queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(pages[1]));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaimDontNeed(pages[1]));

  END_TEST;
}

static bool vmo_eviction_test() {
  BEGIN_TEST;
  // Disable the page scanner as this test would be flaky if our pages get evicted by someone else.
  AutoVmScannerDisable scanner_disable;

  // Make two pager backed vmos
  fbl::RefPtr<VmObjectPaged> vmo;
  fbl::RefPtr<VmObjectPaged> vmo2;
  vm_page_t* page;
  vm_page_t* page2;
  zx_status_t status =
      make_committed_pager_vmo(1, /*trap_dirty=*/false, /*resizable=*/false, &page, &vmo);
  ASSERT_EQ(ZX_OK, status);
  status = make_committed_pager_vmo(1, /*trap_dirty=*/false, /*resizable=*/false, &page2, &vmo2);
  ASSERT_EQ(ZX_OK, status);

  // Shouldn't be able to evict pages from the wrong VMO.
  ASSERT_EQ(reclaim_page(vmo, page2, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);
  ASSERT_EQ(reclaim_page(vmo2, page, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);

  // We stack-own loaned pages from ReclaimPage() to pmm_free_page().
  __UNINITIALIZED StackOwnedLoanedPagesInterval raii_interval;

  // Eviction should actually drop the number of committed pages.
  EXPECT_TRUE(make_private_attribution_counts(PAGE_SIZE, 0) == vmo2->GetAttributedMemory());
  ASSERT_EQ(reclaim_page(vmo2, page2, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 1u);
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo2->GetAttributedMemory());
  EXPECT_GT(vmo2->ReclamationEventCount(), 0u);

  // Pinned pages should not be evictable.
  status = vmo->CommitRangePinned(0, PAGE_SIZE, false);
  EXPECT_EQ(ZX_OK, status);
  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);
  vmo->Unpin(0, PAGE_SIZE);

  END_TEST;
}

// This test exists to provide a location for VmObjectPaged::DebugValidatePageSharing to be
// regularly called so that it doesn't bitrot. Additionally it *might* detect VMO object corruption,
// but it's primary goal is to test the implementation of DebugValidatePageSharing.
static bool vmo_validate_page_shares_test() {
  BEGIN_TEST;

  zx_status_t status = VmObject::ForEach([](const VmObject& vmo) -> zx_status_t {
    if (vmo.is_paged()) {
      const VmObjectPaged& paged = static_cast<const VmObjectPaged&>(vmo);
      if (!paged.DebugValidatePageSharing()) {
        return ZX_ERR_INTERNAL;
      }
    }
    return ZX_OK;
  });

  // Although DebugValidatePageSharing says to panic as soon as possible if it returns false, this
  // test errs on side of assuming that the validation is broken, and not the hierarchy, and so does
  // not panic. Either way the test still fails, this is just more graceful.
  EXPECT_EQ(ZX_OK, status);

  END_TEST;
}

// Tests memory attribution under various cloning behaviors - creation of snapshot clones and
// slices, removal of clones, committing pages in the original vmo and in the clones.
static bool vmo_attribution_clones_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;
  using AttributionCounts = VmObject::AttributionCounts;

  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, 4 * PAGE_SIZE, &vmo);
  ASSERT_EQ(ZX_OK, status);
  // Fake user id to keep the cloning code happy.
  vmo->set_user_id(0xff);

  EXPECT_TRUE(vmo->GetAttributedMemory() == AttributionCounts{});

  // Commit the first two pages.
  status = vmo->CommitRange(0, 2 * PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(2ul * PAGE_SIZE, 0));

  // Create a clone that sees the second and third pages.
  fbl::RefPtr<VmObject> clone;
  status = vmo->CreateClone(Resizability::NonResizable, SnapshotType::Full, PAGE_SIZE,
                            2 * PAGE_SIZE, true, &clone);
  ASSERT_EQ(ZX_OK, status);
  clone->set_user_id(0xfc);

  EXPECT_TRUE(vmo->GetAttributedMemory() ==
              (AttributionCounts{.uncompressed_bytes = 2ul * PAGE_SIZE,
                                 .private_uncompressed_bytes = PAGE_SIZE,
                                 .scaled_uncompressed_bytes = vm::FractionalBytes(PAGE_SIZE, 2) +
                                                              vm::FractionalBytes(PAGE_SIZE)}));
  EXPECT_TRUE(clone->GetAttributedMemory() ==
              (AttributionCounts{.uncompressed_bytes = PAGE_SIZE,
                                 .scaled_uncompressed_bytes = vm::FractionalBytes(PAGE_SIZE, 2)}));

  // Commit both pages in the clone.
  status = clone->CommitRange(0, 2 * PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(2ul * PAGE_SIZE, 0));
  EXPECT_TRUE(clone->GetAttributedMemory() == make_private_attribution_counts(2ul * PAGE_SIZE, 0));

  // Commit the last page in the original vmo.
  status = vmo->CommitRange(3 * PAGE_SIZE, PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(3ul * PAGE_SIZE, 0));

  // Create a slice that sees all four pages of the original vmo.
  fbl::RefPtr<VmObject> slice;
  status = vmo->CreateChildSlice(0, 4 * PAGE_SIZE, true, &slice);
  ASSERT_EQ(ZX_OK, status);
  slice->set_user_id(0xf5);

  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(3ul * PAGE_SIZE, 0));
  EXPECT_TRUE(clone->GetAttributedMemory() == make_private_attribution_counts(2ul * PAGE_SIZE, 0));
  EXPECT_TRUE(slice->GetAttributedMemory() == AttributionCounts{});

  // Committing the slice's last page is a no-op (as the page is already committed).
  status = slice->CommitRange(3 * PAGE_SIZE, PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(3ul * PAGE_SIZE, 0));

  // Committing the remaining 3 pages in the slice will commit pages in the original vmo.
  status = slice->CommitRange(0, 4 * PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(4ul * PAGE_SIZE, 0));
  EXPECT_TRUE(clone->GetAttributedMemory() == make_private_attribution_counts(2ul * PAGE_SIZE, 0));
  EXPECT_TRUE(slice->GetAttributedMemory() == AttributionCounts{});

  clone.reset();
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(4ul * PAGE_SIZE, 0));
  EXPECT_TRUE(slice->GetAttributedMemory() == AttributionCounts{});

  slice.reset();
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(4ul * PAGE_SIZE, 0));

  END_TEST;
}

// Tests that memory attribution behaves as expected under various operations performed on the vmo
// that can change its page list - committing / decommitting pages, reading / writing, zero range,
// resizing.
static bool vmo_attribution_ops_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  for (uint32_t is_ppb_enabled = 0; is_ppb_enabled < 2; ++is_ppb_enabled) {
    dprintf(INFO, "is_ppb_enabled: %u\n", is_ppb_enabled);

    bool loaning_was_enabled = pmm_physical_page_borrowing_config()->is_loaning_enabled();
    bool borrowing_was_enabled =
        pmm_physical_page_borrowing_config()->is_borrowing_in_supplypages_enabled();
    pmm_physical_page_borrowing_config()->set_loaning_enabled(is_ppb_enabled);
    pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(is_ppb_enabled);
    auto cleanup = fit::defer([loaning_was_enabled, borrowing_was_enabled] {
      pmm_physical_page_borrowing_config()->set_loaning_enabled(loaning_was_enabled);
      pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(
          borrowing_was_enabled);
    });

    fbl::RefPtr<VmObjectPaged> vmo;
    zx_status_t status;
    status =
        VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kResizable, 4 * PAGE_SIZE, &vmo);
    ASSERT_EQ(ZX_OK, status);

    VmObject::AttributionCounts expected_attribution_counts;
    expected_attribution_counts.uncompressed_bytes = 0;
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    status = vmo->CommitRange(0, 4 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    expected_attribution_counts = make_private_attribution_counts(4ul * PAGE_SIZE, 0);
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    // Committing the same range again will be a no-op.
    status = vmo->CommitRange(0, 4 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    status = vmo->DecommitRange(0, 4 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    expected_attribution_counts = make_private_attribution_counts(0, 0);
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    status = vmo->CommitRange(0, 4 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    expected_attribution_counts = make_private_attribution_counts(4ul * PAGE_SIZE, 0);
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    status = vmo->DecommitRange(0, 4 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    expected_attribution_counts = make_private_attribution_counts(0, 0);
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    fbl::AllocChecker ac;
    fbl::Vector<uint8_t> buf;
    buf.reserve(2 * PAGE_SIZE, &ac);
    ASSERT_TRUE(ac.check());

    // Read the first two pages.
    status = vmo->Read(buf.data(), 0, 2 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    // Since these are zero pages being read, this won't commit any pages in
    // the vmo.
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    // Write the first two pages, committing them.
    status = vmo->Write(buf.data(), 0, 2 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    expected_attribution_counts = make_private_attribution_counts(2ul * PAGE_SIZE, 0);
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    // Write the last two pages, committing them.
    status = vmo->Write(buf.data(), 2 * PAGE_SIZE, 2 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    expected_attribution_counts = make_private_attribution_counts(4ul * PAGE_SIZE, 0);
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    status = vmo->Resize(2 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    expected_attribution_counts = make_private_attribution_counts(2ul * PAGE_SIZE, 0);
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    // Zero'ing the range will decommit pages.
    status = vmo->ZeroRange(0, 2 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    expected_attribution_counts = make_private_attribution_counts(0, 0);
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);
  }

  END_TEST;
}

// Tests that memory attribution behaves as expected under various operations performed on a
// contiguous vmo that can change its page list - committing / decommitting pages, reading /
// writing, zero range, resizing.
static bool vmo_attribution_ops_contiguous_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  for (uint32_t is_ppb_enabled = 0; is_ppb_enabled < 2; ++is_ppb_enabled) {
    dprintf(INFO, "is_ppb_enabled: %u\n", is_ppb_enabled);

    bool loaning_was_enabled = pmm_physical_page_borrowing_config()->is_loaning_enabled();
    bool borrowing_was_enabled =
        pmm_physical_page_borrowing_config()->is_borrowing_in_supplypages_enabled();
    pmm_physical_page_borrowing_config()->set_loaning_enabled(is_ppb_enabled);
    pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(is_ppb_enabled);
    auto cleanup = fit::defer([loaning_was_enabled, borrowing_was_enabled] {
      pmm_physical_page_borrowing_config()->set_loaning_enabled(loaning_was_enabled);
      pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(
          borrowing_was_enabled);
    });

    fbl::RefPtr<VmObjectPaged> vmo;
    zx_status_t status;
    status = VmObjectPaged::CreateContiguous(PMM_ALLOC_FLAG_ANY, 4 * PAGE_SIZE,
                                             /*alignment_log2=*/0, &vmo);
    ASSERT_EQ(ZX_OK, status);

    VmObject::AttributionCounts expected_attribution_counts;
    expected_attribution_counts = make_private_attribution_counts(4ul * PAGE_SIZE, 0);
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    status = vmo->CommitRange(0, 4 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    // Committing the same range again will be a no-op.
    status = vmo->CommitRange(0, 4 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    status = vmo->DecommitRange(0, 4 * PAGE_SIZE);
    if (!is_ppb_enabled) {
      ASSERT_EQ(ZX_ERR_NOT_SUPPORTED, status);
      // No change because DecommitRange() failed (as expected).
      DEBUG_ASSERT(expected_attribution_counts.uncompressed_bytes == 4ul * PAGE_SIZE);
    } else {
      ASSERT_EQ(ZX_OK, status);
      expected_attribution_counts = make_private_attribution_counts(0, 0);
    }
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    status = vmo->CommitRange(0, 4 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    if (!is_ppb_enabled) {
      // expected_attribution_counts don't change because the pages are already present.
      DEBUG_ASSERT(expected_attribution_counts.uncompressed_bytes == 4ul * PAGE_SIZE);
    } else {
      expected_attribution_counts = make_private_attribution_counts(4ul * PAGE_SIZE, 0);
    }
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    status = vmo->DecommitRange(0, 4 * PAGE_SIZE);
    if (!is_ppb_enabled) {
      ASSERT_EQ(ZX_ERR_NOT_SUPPORTED, status);
      // and expected_attribution_counts don't change because we're zeroing not decommitting.
      DEBUG_ASSERT(expected_attribution_counts.uncompressed_bytes == 4ul * PAGE_SIZE);
    } else {
      ASSERT_EQ(ZX_OK, status);
      expected_attribution_counts = make_private_attribution_counts(0, 0);
    }
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    fbl::AllocChecker ac;
    fbl::Vector<uint8_t> buf;
    buf.reserve(2 * PAGE_SIZE, &ac);
    ASSERT_TRUE(ac.check());

    // Read the first two pages. Reading will still cause pages to get committed.
    status = vmo->Read(buf.data(), 0, 2 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    if (!is_ppb_enabled) {
      // and expected_attribution_counts don't change because the pages are already present.
      DEBUG_ASSERT(expected_attribution_counts.uncompressed_bytes == 4ul * PAGE_SIZE);
    } else {
      expected_attribution_counts = make_private_attribution_counts(2ul * PAGE_SIZE, 0);
    }
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    // Write the last two pages, committing them.
    status = vmo->Write(buf.data(), 2 * PAGE_SIZE, 2 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    if (!is_ppb_enabled) {
      // and expected_attribution_counts don't change because the pages are already present.
      DEBUG_ASSERT(expected_attribution_counts.uncompressed_bytes == 4ul * PAGE_SIZE);
    } else {
      expected_attribution_counts = make_private_attribution_counts(4ul * PAGE_SIZE, 0);
    }
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    // Zero'ing the range will decommit pages. In the case of contiguous VMOs, we don't decommit
    // pages (so far).
    status = vmo->ZeroRange(0, 2 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    // Zeroing doesn't decommit pages of contiguous VMOs (nor does it commit pages).
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    status = vmo->DecommitRange(0, 2 * PAGE_SIZE);
    if (!is_ppb_enabled) {
      ASSERT_EQ(ZX_ERR_NOT_SUPPORTED, status);
      DEBUG_ASSERT(expected_attribution_counts.uncompressed_bytes == 4ul * PAGE_SIZE);
    } else {
      ASSERT_EQ(ZX_OK, status);
      // We were able to decommit two pages.
      expected_attribution_counts = make_private_attribution_counts(2ul * PAGE_SIZE, 0);
    }
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);

    // Zero'ing a decommitted range (if is_ppb_enabled is true) should not commit any new pages.
    // Empty slots in a decommitted contiguous VMO are zero by default, as the physical page
    // provider will zero these pages on supply.
    status = vmo->ZeroRange(0, 2 * PAGE_SIZE);
    ASSERT_EQ(ZX_OK, status);
    // The attribution counts should remain unchanged.
    EXPECT_TRUE(vmo->GetAttributedMemory() == expected_attribution_counts);
  }

  END_TEST;
}

// Tests that memory attribution behaves as expected for operations specific to pager-backed
// vmo's - supplying pages, creating COW clones.
static bool vmo_attribution_pager_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

  static const size_t kNumPages = 2;
  static const size_t alloc_size = kNumPages * PAGE_SIZE;
  using AttributionCounts = VmObject::AttributionCounts;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status =
      make_uncommitted_pager_vmo(kNumPages, /*trap_dirty=*/false, /*resizable=*/false, &vmo);
  ASSERT_EQ(ZX_OK, status);
  // Fake user id to keep the cloning code happy.
  vmo->set_user_id(0xff);

  EXPECT_TRUE(vmo->GetAttributedMemory() == AttributionCounts{});

  // Create an aux VMO to transfer pages into the pager-backed vmo.
  fbl::RefPtr<VmObjectPaged> aux_vmo;
  status =
      VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kResizable, alloc_size, &aux_vmo);
  ASSERT_EQ(ZX_OK, status);

  EXPECT_TRUE(aux_vmo->GetAttributedMemory() == AttributionCounts{});

  status = aux_vmo->CommitRange(0, alloc_size);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_TRUE(aux_vmo->GetAttributedMemory() ==
              make_private_attribution_counts(2ul * PAGE_SIZE, 0));

  VmPageSpliceList page_list;
  status = aux_vmo->TakePages(0, PAGE_SIZE, &page_list);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_TRUE(aux_vmo->GetAttributedMemory() == make_private_attribution_counts(PAGE_SIZE, 0));
  EXPECT_TRUE(vmo->GetAttributedMemory() == AttributionCounts{});

  status = vmo->SupplyPages(0, PAGE_SIZE, &page_list, SupplyOptions::PagerSupply);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(PAGE_SIZE, 0));
  EXPECT_TRUE(aux_vmo->GetAttributedMemory() == make_private_attribution_counts(PAGE_SIZE, 0));

  aux_vmo.reset();

  // Create a COW clone that sees the first page.
  fbl::RefPtr<VmObject> clone;
  status = vmo->CreateClone(Resizability::NonResizable, SnapshotType::OnWrite, 0, PAGE_SIZE, true,
                            &clone);
  ASSERT_EQ(ZX_OK, status);
  clone->set_user_id(0xfc);

  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(PAGE_SIZE, 0));
  EXPECT_TRUE(clone->GetAttributedMemory() == AttributionCounts{});

  status = clone->CommitRange(0, PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(PAGE_SIZE, 0));
  EXPECT_TRUE(clone->GetAttributedMemory() == make_private_attribution_counts(PAGE_SIZE, 0));

  clone.reset();
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(PAGE_SIZE, 0));

  END_TEST;
}

// Tests that memory attribution behaves as expected when a pager-backed vmo's page is evicted.
static bool vmo_attribution_evict_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

  using AttributionCounts = VmObject::AttributionCounts;
  fbl::RefPtr<VmObjectPaged> vmo;
  vm_page_t* page;
  zx_status_t status =
      make_committed_pager_vmo(1, /*trap_dirty=*/false, /*resizable=*/false, &page, &vmo);
  ASSERT_EQ(ZX_OK, status);

  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(PAGE_SIZE, 0));

  // We stack-own loaned pages from ReclaimPage() to pmm_free_page().
  __UNINITIALIZED StackOwnedLoanedPagesInterval raii_interval;

  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 1u);
  EXPECT_TRUE(vmo->GetAttributedMemory() == AttributionCounts{});

  END_TEST;
}

// Tests that memory attribution behaves as expected when zero pages are deduped, changing the no.
// of committed pages in the vmo.
static bool vmo_attribution_dedup_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

  using AttributionCounts = VmObject::AttributionCounts;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, 2 * PAGE_SIZE, &vmo);
  ASSERT_EQ(ZX_OK, status);

  EXPECT_TRUE(vmo->GetAttributedMemory() == AttributionCounts{});

  status = vmo->CommitRange(0, 2 * PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(2ul * PAGE_SIZE, 0));

  vm_page_t* page;
  status = vmo->GetPageBlocking(0, 0, nullptr, &page, nullptr);
  ASSERT_EQ(ZX_OK, status);

  // Dedupe the first page.
  auto vmop = static_cast<VmObjectPaged*>(vmo.get());
  ASSERT_TRUE(vmop->DebugGetCowPages()->DedupZeroPage(page, 0));
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(PAGE_SIZE, 0));

  // Dedupe the second page.
  status = vmo->GetPageBlocking(PAGE_SIZE, 0, nullptr, &page, nullptr);
  ASSERT_EQ(ZX_OK, status);
  ASSERT_TRUE(vmop->DebugGetCowPages()->DedupZeroPage(page, PAGE_SIZE));
  EXPECT_TRUE(vmo->GetAttributedMemory() == AttributionCounts{});

  // Commit the range again.
  status = vmo->CommitRange(0, 2 * PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(2ul * PAGE_SIZE, 0));

  END_TEST;
}

// Test that compressing and uncompressing pages in a VMO correctly updates memory attribution
// counts.
static bool vmo_attribution_compression_test() {
  BEGIN_TEST;

  // Need a compressor.
  auto compression = Pmm::Node().GetPageCompression();
  if (!compression) {
    END_TEST;
  }

  AutoVmScannerDisable scanner_disable;

  using AttributionCounts = VmObject::AttributionCounts;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, 2 * PAGE_SIZE, &vmo);
  ASSERT_EQ(ZX_OK, status);

  EXPECT_TRUE(vmo->GetAttributedMemory() == AttributionCounts{});

  uint64_t reclamation_count = vmo->ReclamationEventCount();

  // Committing pages should not increment the reclamation count.
  status = vmo->CommitRange(0, 2 * PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(2ul * PAGE_SIZE, 0));
  EXPECT_EQ(reclamation_count, vmo->ReclamationEventCount());

  uint64_t val = 42;
  status = vmo->Write(&val, 0, sizeof(val));
  EXPECT_OK(status);
  EXPECT_EQ(reclamation_count, vmo->ReclamationEventCount());

  // Compress the first page.
  vm_page_t* page = nullptr;
  status = vmo->GetPageBlocking(0, 0, nullptr, &page, nullptr);
  ASSERT_EQ(ZX_OK, status);
  {
    auto compressor = compression->AcquireCompressor();
    EXPECT_OK(compressor.get().Arm());
    ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, &compressor.get()),
              1u);
  }
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(PAGE_SIZE, PAGE_SIZE));
  {
    const uint64_t new_reclamation_count = vmo->ReclamationEventCount();
    EXPECT_GT(new_reclamation_count, reclamation_count);
    reclamation_count = new_reclamation_count;
  }
  // Compress the second page.
  status = vmo->GetPageBlocking(PAGE_SIZE, 0, nullptr, &page, nullptr);
  ASSERT_EQ(ZX_OK, status);
  {
    auto compressor = compression->AcquireCompressor();
    EXPECT_OK(compressor.get().Arm());
    ASSERT_EQ(reclaim_page(vmo, page, PAGE_SIZE, VmCowPages::EvictionHintAction::Follow,
                           &compressor.get()),
              1u);
  }
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(0, PAGE_SIZE));
  {
    const uint64_t new_reclamation_count = vmo->ReclamationEventCount();
    EXPECT_GT(new_reclamation_count, reclamation_count);
    reclamation_count = new_reclamation_count;
  }

  // Attempting to read the first page will require a decompress.
  status = vmo->GetPageBlocking(0, VMM_PF_FLAG_HW_FAULT, nullptr, &page, nullptr);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(PAGE_SIZE, 0));
  EXPECT_EQ(reclamation_count, vmo->ReclamationEventCount());

  // Reading the second page will just get the zero page.
  status = vmo->GetPageBlocking(PAGE_SIZE, VMM_PF_FLAG_HW_FAULT, nullptr, &page, nullptr);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_TRUE(vmo->GetAttributedMemory() == make_private_attribution_counts(PAGE_SIZE, 0));
  EXPECT_EQ(reclamation_count, vmo->ReclamationEventCount());

  END_TEST;
}

// Test that a VmObjectPaged that is only referenced by its children gets removed by effectively
// merging into its parent and re-homing all the children. This should also drop any VmCowPages
// being held open.
static bool vmo_parent_merge_test() {
  BEGIN_TEST;

  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE, &vmo);
  ASSERT_EQ(ZX_OK, status);

  // Set a user ID for testing.
  vmo->set_user_id(42);

  fbl::RefPtr<VmObject> child;
  status =
      vmo->CreateClone(Resizability::NonResizable, SnapshotType::Full, 0, PAGE_SIZE, false, &child);
  ASSERT_EQ(ZX_OK, status);

  child->set_user_id(43);

  EXPECT_EQ(0u, vmo->parent_user_id());
  EXPECT_EQ(42u, vmo->user_id());
  EXPECT_EQ(43u, child->user_id());
  EXPECT_EQ(42u, child->parent_user_id());

  // Dropping the parent should re-home the child to an empty parent.
  vmo.reset();
  EXPECT_EQ(43u, child->user_id());
  EXPECT_EQ(0u, child->parent_user_id());

  child.reset();

  // Recreate a more interesting 3 level hierarchy with vmo->child->(child2,child3)

  status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE, &vmo);
  ASSERT_EQ(ZX_OK, status);
  vmo->set_user_id(42);
  status =
      vmo->CreateClone(Resizability::NonResizable, SnapshotType::Full, 0, PAGE_SIZE, false, &child);
  ASSERT_EQ(ZX_OK, status);
  child->set_user_id(43);
  fbl::RefPtr<VmObject> child2;
  status = child->CreateClone(Resizability::NonResizable, SnapshotType::Full, 0, PAGE_SIZE, false,
                              &child2);
  ASSERT_EQ(ZX_OK, status);
  child2->set_user_id(44);
  fbl::RefPtr<VmObject> child3;
  status = child->CreateClone(Resizability::NonResizable, SnapshotType::Full, 0, PAGE_SIZE, false,
                              &child3);
  ASSERT_EQ(ZX_OK, status);
  child3->set_user_id(45);
  EXPECT_EQ(0u, vmo->parent_user_id());
  EXPECT_EQ(42u, child->parent_user_id());
  EXPECT_EQ(43u, child2->parent_user_id());
  EXPECT_EQ(43u, child3->parent_user_id());

  // Drop the intermediate child, child2+3 should get re-homed to vmo
  child.reset();
  EXPECT_EQ(42u, child2->parent_user_id());
  EXPECT_EQ(42u, child3->parent_user_id());

  END_TEST;
}

// Test that the discardable VMO's lock count is updated as expected via lock and unlock ops.
static bool vmo_lock_count_test() {
  BEGIN_TEST;

  // Create a vmo to lock and unlock from multiple threads.
  fbl::RefPtr<VmObjectPaged> vmo;
  constexpr uint64_t kSize = 3 * PAGE_SIZE;
  zx_status_t status =
      VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kDiscardable, kSize, &vmo);
  ASSERT_EQ(ZX_OK, status);

  constexpr int kNumThreads = 5;
  Thread* threads[kNumThreads];
  struct thread_state {
    VmObjectPaged* vmo;
    bool did_unlock;
  } state[kNumThreads];

  for (int i = 0; i < kNumThreads; i++) {
    state[i].vmo = vmo.get();
    state[i].did_unlock = false;

    threads[i] = Thread::Create(
        "worker",
        [](void* arg) -> int {
          zx_status_t status;
          auto state = static_cast<struct thread_state*>(arg);

          // Randomly decide between try-lock and lock.
          if (rand() % 2) {
            if ((status = state->vmo->TryLockRange(0, kSize)) != ZX_OK) {
              return status;
            }
          } else {
            zx_vmo_lock_state_t lock_state = {};
            if ((status = state->vmo->LockRange(0, kSize, &lock_state)) != ZX_OK) {
              return status;
            }
          }

          // Randomly decide whether to unlock, or leave the vmo locked.
          if (rand() % 2) {
            if ((status = state->vmo->UnlockRange(0, kSize)) != ZX_OK) {
              return status;
            }
            state->did_unlock = true;
          }

          return 0;
        },
        &state[i], DEFAULT_PRIORITY);
  }

  for (auto& t : threads) {
    t->Resume();
  }

  for (auto& t : threads) {
    int ret;
    t->Join(&ret, ZX_TIME_INFINITE);
    EXPECT_EQ(0, ret);
  }

  uint64_t expected_lock_count = kNumThreads;
  for (auto& s : state) {
    if (s.did_unlock) {
      expected_lock_count--;
    }
  }

  EXPECT_EQ(expected_lock_count,
            vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugGetLockCount());

  END_TEST;
}

// Tests the state transitions for a discardable VMO. Verifies that a discardable VMO is discarded
// only when unlocked, and can be locked / unlocked again after the discard.
static bool vmo_discardable_states_test() {
  BEGIN_TEST;

  fbl::RefPtr<VmObjectPaged> vmo;
  constexpr uint64_t kSize = 3 * PAGE_SIZE;
  zx_status_t status =
      VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kDiscardable, kSize, &vmo);
  ASSERT_EQ(ZX_OK, status);

  // A newly created discardable vmo is not on any list yet.
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsUnreclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsReclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsDiscarded());

  // Lock and commit all pages.
  EXPECT_EQ(ZX_OK, vmo->TryLockRange(0, kSize));
  EXPECT_EQ(ZX_OK, vmo->CommitRange(0, kSize));
  EXPECT_TRUE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsUnreclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsReclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsDiscarded());

  // Cannot discard when locked.
  EXPECT_EQ(0u, vmo->DebugGetCowPages()->DiscardPages());

  // Unlock.
  EXPECT_EQ(ZX_OK, vmo->UnlockRange(0, kSize));
  EXPECT_TRUE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsReclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsUnreclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsDiscarded());

  // Should be able to discard now.
  EXPECT_EQ(kSize / PAGE_SIZE, vmo->DebugGetCowPages()->DiscardPages());
  EXPECT_TRUE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsDiscarded());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsUnreclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsReclaimable());

  // Try lock should fail after discard.
  EXPECT_EQ(ZX_ERR_UNAVAILABLE, vmo->TryLockRange(0, kSize));

  // Lock should succeed.
  zx_vmo_lock_state_t lock_state = {};
  EXPECT_EQ(ZX_OK, vmo->LockRange(0, kSize, &lock_state));
  EXPECT_TRUE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsUnreclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsReclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsDiscarded());

  // Verify the lock state returned.
  EXPECT_EQ(0u, lock_state.offset);
  EXPECT_EQ(kSize, lock_state.size);
  EXPECT_EQ(0u, lock_state.discarded_offset);
  EXPECT_EQ(kSize, lock_state.discarded_size);

  EXPECT_EQ(ZX_OK, vmo->CommitRange(0, kSize));

  // Unlock.
  EXPECT_EQ(ZX_OK, vmo->UnlockRange(0, kSize));
  EXPECT_TRUE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsReclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsUnreclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsDiscarded());

  // Lock again and verify the lock state returned without a discard.
  EXPECT_EQ(ZX_OK, vmo->LockRange(0, kSize, &lock_state));
  EXPECT_TRUE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsUnreclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsReclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsDiscarded());

  EXPECT_EQ(0u, lock_state.offset);
  EXPECT_EQ(kSize, lock_state.size);
  EXPECT_EQ(0u, lock_state.discarded_offset);
  EXPECT_EQ(0u, lock_state.discarded_size);

  // Unlock and discard again.
  EXPECT_EQ(ZX_OK, vmo->UnlockRange(0, kSize));
  EXPECT_TRUE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsReclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsUnreclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsDiscarded());

  EXPECT_EQ(kSize / PAGE_SIZE, vmo->DebugGetCowPages()->DiscardPages());
  EXPECT_TRUE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsDiscarded());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsUnreclaimable());
  EXPECT_FALSE(vmo->DebugGetCowPages()->DebugGetDiscardableTracker()->DebugIsReclaimable());

  END_TEST;
}

// Test that an unlocked discardable VMO can be discarded as expected.
static bool vmo_discard_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  // Create a resizable discardable vmo.
  fbl::RefPtr<VmObjectPaged> vmo;
  constexpr uint64_t kSize = 3 * PAGE_SIZE;
  zx_status_t status = VmObjectPaged::Create(
      PMM_ALLOC_FLAG_ANY, VmObjectPaged::kDiscardable | VmObjectPaged::kResizable, kSize, &vmo);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_EQ(kSize, vmo->size());

  // Lock and commit all pages. Verify the size.
  EXPECT_EQ(ZX_OK, vmo->TryLockRange(0, kSize));
  EXPECT_EQ(ZX_OK, vmo->CommitRange(0, kSize));
  vm_page_t* page = nullptr;
  ASSERT_OK(vmo->GetPageBlocking(0, 0, nullptr, &page, nullptr));
  EXPECT_EQ(kSize, vmo->size());
  EXPECT_TRUE(make_private_attribution_counts(kSize, 0) == vmo->GetAttributedMemory());

  // Cannot discard when locked.
  EXPECT_EQ(0u, vmo->DebugGetCowPages()->DiscardPages());
  EXPECT_EQ(vmo->DebugGetCowPages()
                ->ReclaimPage(page, 0, VmCowPages::EvictionHintAction::Follow, nullptr)
                .Total(),
            0u);
  EXPECT_TRUE(make_private_attribution_counts(kSize, 0) == vmo->GetAttributedMemory());

  // Unlock.
  EXPECT_EQ(ZX_OK, vmo->UnlockRange(0, kSize));
  EXPECT_EQ(kSize, vmo->size());

  // Page should be in reclaimable queue.
  EXPECT_TRUE(Pmm::Node().GetPageQueues()->DebugPageIsReclaim(page));

  uint64_t reclamation_count = vmo->ReclamationEventCount();

  // Should be able to discard now.
  EXPECT_EQ(kSize / PAGE_SIZE,
            vmo->DebugGetCowPages()
                ->ReclaimPage(page, 0, VmCowPages::EvictionHintAction::Follow, nullptr)
                .Total());
  page = nullptr;
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());
  EXPECT_GT(vmo->ReclamationEventCount(), reclamation_count);
  // Verify that the size is not affected.
  EXPECT_EQ(kSize, vmo->size());

  // Resize the discarded vmo.
  constexpr uint64_t kNewSize = 5 * PAGE_SIZE;
  EXPECT_EQ(ZX_OK, vmo->Resize(kNewSize));
  EXPECT_EQ(kNewSize, vmo->size());
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());

  // Lock the vmo.
  zx_vmo_lock_state_t lock_state = {};
  EXPECT_EQ(ZX_OK, vmo->LockRange(0, kNewSize, &lock_state));
  EXPECT_EQ(kNewSize, vmo->size());
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());

  // Commit and pin some pages, then unlock.
  EXPECT_EQ(ZX_OK, vmo->CommitRangePinned(0, kSize, false));
  ASSERT_OK(vmo->GetPageBlocking(0, 0, nullptr, &page, nullptr));
  EXPECT_TRUE(make_private_attribution_counts(kSize, 0) == vmo->GetAttributedMemory());
  EXPECT_EQ(ZX_OK, vmo->UnlockRange(0, kNewSize));

  // Page is pinned, so not in the reclaim queue, but in the wired queue.
  EXPECT_FALSE(Pmm::Node().GetPageQueues()->DebugPageIsReclaim(page));
  EXPECT_TRUE(Pmm::Node().GetPageQueues()->DebugPageIsWired(page));

  reclamation_count = vmo->ReclamationEventCount();

  // Cannot discard a vmo with pinned pages.
  EXPECT_EQ(0u, vmo->DebugGetCowPages()->DiscardPages());
  EXPECT_EQ(vmo->DebugGetCowPages()
                ->ReclaimPage(page, 0, VmCowPages::EvictionHintAction::Follow, nullptr)
                .Total(),
            0u);
  EXPECT_EQ(kNewSize, vmo->size());
  EXPECT_TRUE(make_private_attribution_counts(kSize, 0) == vmo->GetAttributedMemory());
  EXPECT_EQ(reclamation_count, vmo->ReclamationEventCount());

  // Unpin the pages. Should be able to discard now.
  vmo->Unpin(0, kSize);
  EXPECT_TRUE(Pmm::Node().GetPageQueues()->DebugPageIsReclaim(page));
  EXPECT_EQ(kSize / PAGE_SIZE,
            vmo->DebugGetCowPages()
                ->ReclaimPage(page, 0, VmCowPages::EvictionHintAction::Follow, nullptr)
                .Total());
  page = nullptr;
  EXPECT_EQ(kNewSize, vmo->size());
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());
  EXPECT_GT(vmo->ReclamationEventCount(), reclamation_count);

  // Lock and commit pages, this time by writing them through a user mapping.
  EXPECT_EQ(ZX_OK, vmo->LockRange(0, kNewSize, &lock_state));
  ktl::unique_ptr<testing::UserMemory> user_memory = testing::UserMemory::Create(vmo);
  ASSERT_TRUE(user_memory);
  for (size_t offset = 0; offset < kNewSize; offset += PAGE_SIZE) {
    char val = 42;
    user_memory->put(val, offset);
  }
  EXPECT_EQ(ZX_OK, vmo->UnlockRange(0, kNewSize));
  ASSERT_OK(vmo->GetPageBlocking(0, 0, nullptr, &page, nullptr));
  EXPECT_TRUE(Pmm::Node().GetPageQueues()->DebugPageIsReclaim(page));

  // Cannot discard a non-discardable vmo.
  vmo.reset();
  status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kResizable, kSize, &vmo);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_EQ(0u, vmo->DebugGetCowPages()->DiscardPages());
  EXPECT_EQ(0u, vmo->ReclamationEventCount());

  END_TEST;
}

// Test operations on a discarded VMO and verify expected failures.
static bool vmo_discard_failure_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  fbl::RefPtr<VmObjectPaged> vmo;
  constexpr uint64_t kSize = 5 * PAGE_SIZE;
  zx_status_t status =
      VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kDiscardable, kSize, &vmo);
  ASSERT_EQ(ZX_OK, status);

  fbl::AllocChecker ac;
  fbl::Vector<uint8_t> buf;
  buf.reserve(kSize, &ac);
  ASSERT_TRUE(ac.check());

  fbl::Vector<uint8_t> fill;
  fill.reserve(kSize, &ac);
  ASSERT_TRUE(ac.check());
  fill_region(0x77, fill.data(), kSize);

  // Lock and commit all pages, write something and read it back to verify.
  EXPECT_EQ(ZX_OK, vmo->TryLockRange(0, kSize));
  EXPECT_EQ(ZX_OK, vmo->Write(fill.data(), 0, kSize));
  EXPECT_TRUE(make_private_attribution_counts(kSize, 0) == vmo->GetAttributedMemory());
  EXPECT_EQ(ZX_OK, vmo->Read(buf.data(), 0, kSize));
  EXPECT_EQ(0, memcmp(fill.data(), buf.data(), kSize));

  // Create a test user aspace to map the vmo.
  fbl::RefPtr<VmAspace> aspace = VmAspace::Create(VmAspace::Type::User, "test aspace");
  ASSERT_NONNULL(aspace);

  VmAspace* old_aspace = Thread::Current::active_aspace();
  auto cleanup_aspace = fit::defer([&]() {
    vmm_set_active_aspace(old_aspace);
    ASSERT(aspace->Destroy() == ZX_OK);
  });
  vmm_set_active_aspace(aspace.get());

  // Map the vmo.
  constexpr uint64_t kMapSize = 3 * PAGE_SIZE;
  static constexpr const uint kArchFlags = kArchRwFlags | ARCH_MMU_FLAG_PERM_USER;
  auto mapping_result = aspace->RootVmar()->CreateVmMapping(0, kMapSize, 0, 0, vmo,
                                                            kSize - kMapSize, kArchFlags, "test");
  ASSERT(mapping_result.is_ok());

  // Fill with a known pattern through the mapping, and verify the contents.
  auto uptr = make_user_inout_ptr(reinterpret_cast<void*>(mapping_result->base));
  fill_region_user(0x88, uptr, kMapSize);
  EXPECT_TRUE(test_region_user(0x88, uptr, kMapSize));

  // Unlock and discard.
  EXPECT_EQ(ZX_OK, vmo->UnlockRange(0, kSize));
  EXPECT_EQ(kSize / PAGE_SIZE, vmo->DebugGetCowPages()->DiscardPages());
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());
  EXPECT_EQ(kSize, vmo->size());

  // Reads, writes, commits and pins should fail now.
  EXPECT_EQ(ZX_ERR_NOT_FOUND, vmo->Read(buf.data(), 0, kSize));
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());
  EXPECT_EQ(ZX_ERR_NOT_FOUND, vmo->Write(buf.data(), 0, kSize));
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());
  EXPECT_EQ(ZX_ERR_NOT_FOUND, vmo->CommitRange(0, kSize));
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());
  EXPECT_EQ(ZX_ERR_NOT_FOUND, vmo->CommitRangePinned(0, kSize, false));
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());

  // Decommit and ZeroRange should trivially succeed.
  EXPECT_EQ(ZX_OK, vmo->DecommitRange(0, kSize));
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());
  EXPECT_EQ(ZX_OK, vmo->ZeroRange(0, kSize));
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());

  // Creating a mapping succeeds.
  auto mapping2_result = aspace->RootVmar()->CreateVmMapping(0, kMapSize, 0, 0, vmo,
                                                             kSize - kMapSize, kArchFlags, "test2");
  ASSERT(mapping2_result.is_ok());
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());

  // Lock the vmo again.
  zx_vmo_lock_state_t lock_state = {};
  EXPECT_EQ(ZX_OK, vmo->LockRange(0, kSize, &lock_state));
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());
  EXPECT_EQ(kSize, vmo->size());

  // Should be able to read now. Verify that previous contents are lost and zeros are read.
  EXPECT_EQ(ZX_OK, vmo->Read(buf.data(), 0, kSize));
  memset(fill.data(), 0, kSize);
  EXPECT_EQ(0, memcmp(fill.data(), buf.data(), kSize));
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());

  // Write should succeed as well.
  fill_region(0x99, fill.data(), kSize);
  EXPECT_EQ(ZX_OK, vmo->Write(fill.data(), 0, kSize));
  EXPECT_TRUE(make_private_attribution_counts(kSize, 0) == vmo->GetAttributedMemory());

  // Verify contents via the mapping.
  fill_region_user(0xaa, uptr, kMapSize);
  EXPECT_TRUE(test_region_user(0xaa, uptr, kMapSize));

  // Verify contents via the second mapping created when discarded.
  uptr = make_user_inout_ptr(reinterpret_cast<void*>(mapping2_result->base));
  EXPECT_TRUE(test_region_user(0xaa, uptr, kMapSize));

  // The unmapped pages should still be intact after the Write() above.
  EXPECT_EQ(ZX_OK, vmo->Read(buf.data(), 0, kSize - kMapSize));
  EXPECT_EQ(0, memcmp(fill.data(), buf.data(), kSize - kMapSize));

  END_TEST;
}

static bool vmo_discardable_counts_test() {
  BEGIN_TEST;

  constexpr int kNumVmos = 10;
  fbl::RefPtr<VmObjectPaged> vmos[kNumVmos];

  // Create some discardable vmos.
  zx_status_t status;
  for (int i = 0; i < kNumVmos; i++) {
    status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kDiscardable,
                                   (i + 1) * PAGE_SIZE, &vmos[i]);
    ASSERT_EQ(ZX_OK, status);
  }

  DiscardableVmoTracker::DiscardablePageCounts expected = {};

  // Lock all vmos. Unlock a few. And discard a few unlocked ones.
  // Compute the expected page counts as a result of these operations.
  for (int i = 0; i < kNumVmos; i++) {
    EXPECT_EQ(ZX_OK, vmos[i]->TryLockRange(0, (i + 1) * PAGE_SIZE));
    EXPECT_EQ(ZX_OK, vmos[i]->CommitRange(0, (i + 1) * PAGE_SIZE));

    if (rand() % 2) {
      EXPECT_EQ(ZX_OK, vmos[i]->UnlockRange(0, (i + 1) * PAGE_SIZE));

      if (rand() % 2) {
        // Discarded pages won't show up under locked or unlocked counts.
        EXPECT_EQ(static_cast<uint64_t>(i + 1), vmos[i]->DebugGetCowPages()->DiscardPages());
      } else {
        // Unlocked but not discarded.
        expected.unlocked += (i + 1);
      }
    } else {
      // Locked.
      expected.locked += (i + 1);
    }
  }

  DiscardableVmoTracker::DiscardablePageCounts counts =
      DiscardableVmoTracker::DebugDiscardablePageCounts();
  // There might be other discardable vmos in the rest of the system, so the actual page counts
  // might be higher than the expected counts.
  EXPECT_LE(expected.locked, counts.locked);
  EXPECT_LE(expected.unlocked, counts.unlocked);

  END_TEST;
}

// using LookupCursor with different kinds of faults reads / writes should correctly
// decompress or return an error.
static bool vmo_lookup_compressed_pages_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;
  // Need a working compressor.
  auto compression = Pmm::Node().GetPageCompression();
  if (!compression) {
    END_TEST;
  }

  // Create a VMO and commit a real non-zero page
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, PAGE_SIZE, &vmo);
  ASSERT_OK(status);
  uint64_t data = 42;
  EXPECT_OK(vmo->Write(&data, 0, sizeof(data)));
  EXPECT_TRUE(make_private_attribution_counts(PAGE_SIZE, 0) == vmo->GetAttributedMemory());

  // Compress the page.
  vm_page_t* page;
  status = vmo->GetPageBlocking(0, 0, nullptr, &page, nullptr);
  ASSERT_OK(status);
  {
    auto compressor = compression->AcquireCompressor();
    EXPECT_OK(compressor.get().Arm());
    EXPECT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, &compressor.get()),
              1u);
  }
  EXPECT_TRUE(make_private_attribution_counts(0, PAGE_SIZE) == vmo->GetAttributedMemory());

  // Looking up the page for read or write, without it being a fault, should fail and not cause the
  // page to get decompressed.
  EXPECT_NE(ZX_OK, vmo->GetPageBlocking(0, 0, nullptr, nullptr, nullptr));
  EXPECT_TRUE(make_private_attribution_counts(0, PAGE_SIZE) == vmo->GetAttributedMemory());
  EXPECT_NE(ZX_OK, vmo->GetPageBlocking(0, VMM_PF_FLAG_WRITE, nullptr, nullptr, nullptr));
  EXPECT_TRUE(make_private_attribution_counts(0, PAGE_SIZE) == vmo->GetAttributedMemory());

  // Read or write faults should decompress.
  ASSERT_OK(vmo->GetPageBlocking(0, VMM_PF_FLAG_HW_FAULT, nullptr, &page, nullptr));
  EXPECT_TRUE(make_private_attribution_counts(PAGE_SIZE, 0) == vmo->GetAttributedMemory());
  status = vmo->GetPageBlocking(0, 0, nullptr, &page, nullptr);
  ASSERT_OK(status);
  {
    auto compressor = compression->AcquireCompressor();
    EXPECT_OK(compressor.get().Arm());
    EXPECT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, &compressor.get()),
              1u);
  }
  EXPECT_TRUE(make_private_attribution_counts(0, PAGE_SIZE) == vmo->GetAttributedMemory());

  EXPECT_OK(
      vmo->GetPageBlocking(0, VMM_PF_FLAG_WRITE | VMM_PF_FLAG_SW_FAULT, nullptr, &page, nullptr));
  EXPECT_TRUE(make_private_attribution_counts(PAGE_SIZE, 0) == vmo->GetAttributedMemory());

  END_TEST;
}

static bool vmo_write_does_not_commit_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  // Create a vmo and commit a page to it.
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE, &vmo);
  ASSERT_OK(status);

  uint64_t val = 42;
  EXPECT_OK(vmo->Write(&val, 0, sizeof(val)));

  // Create a CoW clone of the vmo.
  fbl::RefPtr<VmObject> clone;
  status =
      vmo->CreateClone(Resizability::NonResizable, SnapshotType::Full, 0, PAGE_SIZE, false, &clone);

  // Querying the page for read in the clone should return it.
  EXPECT_OK(clone->GetPageBlocking(0, 0, nullptr, nullptr, nullptr));

  // Querying for write, without any fault flags, should not work as the page is not committed in
  // the clone.
  EXPECT_EQ(ZX_ERR_NOT_FOUND,
            clone->GetPageBlocking(0, VMM_PF_FLAG_WRITE, nullptr, nullptr, nullptr));

  // Adding a fault flag should cause the lookup to succeed.
  EXPECT_OK(clone->GetPageBlocking(0, VMM_PF_FLAG_WRITE | VMM_PF_FLAG_SW_FAULT, nullptr, nullptr,
                                   nullptr));

  END_TEST;
}

static bool vmo_stack_owned_loaned_pages_interval_test() {
  BEGIN_TEST;

  // This test isn't stress, but have a few threads to check on multiple waiters per page and
  // multiple waiters per stack_owner.
  constexpr uint32_t kFakePageCount = 8;
  constexpr uint32_t kWaitingThreadsPerPage = 2;
  constexpr uint32_t kWaitingThreadCount = kFakePageCount * kWaitingThreadsPerPage;
  constexpr int kOwnerThreadBasePriority = LOW_PRIORITY;
  constexpr int kBlockedThreadBasePriority = DEFAULT_PRIORITY;
  constexpr SchedWeight kOwnerThreadBaseWeight =
      SchedulerState::ConvertPriorityToWeight(kOwnerThreadBasePriority);
  constexpr SchedWeight kBlockedThreadBaseWeight =
      SchedulerState::ConvertPriorityToWeight(kBlockedThreadBasePriority);

  fbl::AllocChecker ac;

  // Local structures used by the tests.  The kernel stack is pretty small, so
  // don't take any chances here.  Heap allocate these structures instead of
  // stack allocating them.
  struct OwningThread {
    Thread* thread = nullptr;
    vm_page_t pages[kFakePageCount] = {};
    Event ownership_acquired;
    Event release_stack_ownership;
    Event exit_now;
  };

  auto ot = ktl::make_unique<OwningThread>(&ac);
  ASSERT_TRUE(ac.check());

  struct WaitingThread {
    OwningThread* ot = nullptr;
    uint32_t i = 0;
    Thread* thread = nullptr;
  };

  auto waiting_threads = ktl::make_unique<ktl::array<WaitingThread, kWaitingThreadCount>>(&ac);
  ASSERT_TRUE(ac.check());

  for (auto& page : ot->pages) {
    EXPECT_EQ(vm_page_state::FREE, page.state());
    // Normally this would be under PmmLock; only for testing.
    page.set_is_loaned();
  }

  // Test no pages stack owned in a given interval.
  {  // scope raii_interval
    StackOwnedLoanedPagesInterval raii_interval;
    DEBUG_ASSERT(&StackOwnedLoanedPagesInterval::current() == &raii_interval);
  }  // ~raii_interval

  // Test page stack owned but never waited on.  Hold SOLPI lock for this to get
  // a failure if the SOLPI lock is ever acquired for this scenario.  Normally
  // the lock would not be held for these steps, and we should DEBUG_ASSERT if
  // anything in the flow below attempts to obtain the lock while we perform the
  // sequence.
  //
  {  // scope thread_lock_guard, raii_interval
    Guard<SpinLock, IrqSave> sollock_guard{&StackOwnedLoanedPagesInterval::get_lock()};
    StackOwnedLoanedPagesInterval raii_interval;
    DEBUG_ASSERT(&StackOwnedLoanedPagesInterval::current() == &raii_interval);
    ot->pages[0].object.set_stack_owner(&StackOwnedLoanedPagesInterval::current());
    ot->pages[0].object.clear_stack_owner();
  }  // ~raii_interval, ~thread_lock_guard

  // Test pages stack owned each with multiple waiters.
  ot->thread = Thread::Create(
      "owning_thread",
      [](void* arg) -> int {
        // Take "stack ownership" of the pages involved in the test, then signal the test thread
        // that we are ready to proceed.
        OwningThread& ot = *reinterpret_cast<OwningThread*>(arg);

        {
          StackOwnedLoanedPagesInterval raii_interval;
          for (auto& page : ot.pages) {
            DEBUG_ASSERT(&StackOwnedLoanedPagesInterval::current() == &raii_interval);
            page.object.set_stack_owner(&StackOwnedLoanedPagesInterval::current());
          }
          ot.ownership_acquired.Signal();

          // Wait until the test thread tells us it is time to release ownership.
          ot.release_stack_ownership.Wait();

          // Now release ownership and wait until we are told that we can exit.
          for (auto& page : ot.pages) {
            page.object.clear_stack_owner();
          }
          // ~raii_interval
        }

        ot.exit_now.Wait();
        return 0;
      },
      ot.get(), kOwnerThreadBasePriority);

  // Let the owner thread run.  If anything goes wrong from here on out, make
  // sure we drop all of the barriers to the owner thread exiting, and clean
  // everything up.
  ot->thread->Resume();
  auto cleanup = fit::defer([&ot, &waiting_threads]() {
    int ret;

    ot->release_stack_ownership.Signal();
    ot->exit_now.Signal();
    ot->thread->Join(&ret, ZX_TIME_INFINITE);

    for (auto& wt : *waiting_threads) {
      if (wt.thread != nullptr) {
        wt.thread->Join(&ret, ZX_TIME_INFINITE);
      }
    }
  });

  // Now wait until the owner thread has taken ownership of the test pages, then
  // double check to make sure that the owner thread is still running with its
  // base profile.
  ot->ownership_acquired.Wait();
  {
    SingleChainLockGuard guard{IrqSaveOption, ot->thread->get_lock(),
                               CLT_TAG("vmo_stack_owned_loaned_pages_interval_test (1)")};
    const SchedulerState::EffectiveProfile& ep = ot->thread->scheduler_state().effective_profile();
    ASSERT_TRUE(ep.IsFair());
    ASSERT_EQ(kOwnerThreadBaseWeight.raw_value(), ep.fair.weight.raw_value());
  }

  // Start up all of our waiter threads.
  for (uint32_t i = 0; i < kWaitingThreadCount; ++i) {
    auto& wt = waiting_threads->at(i);
    wt.ot = ot.get();
    wt.i = i;
    wt.thread = Thread::Create(
        "waiting_thread",
        [](void* arg) -> int {
          WaitingThread& wt = *reinterpret_cast<WaitingThread*>(arg);
          StackOwnedLoanedPagesInterval::WaitUntilContiguousPageNotStackOwned(
              &wt.ot->pages[wt.i / kWaitingThreadsPerPage]);
          return 0;
        },
        &wt, kBlockedThreadBasePriority);
    ASSERT_NONNULL(wt.thread);
    wt.thread->Resume();
  }

  // Wait until all of the threads have blocked behind the owner of the
  // StackOwnedLoanedPagesInterval object.
  for (auto& wt : *waiting_threads) {
    while (true) {
      {
        SingleChainLockGuard guard{IrqSaveOption, wt.thread->get_lock(),
                                   CLT_TAG("vmo_stack_owned_loaned_pages_interval_test (2)")};
        if (wt.thread->state() == THREAD_BLOCKED) {
          break;
        }
      }
      Thread::Current::SleepRelative(ZX_MSEC(1));
    }
  }

  // Now that we are certain that all threads are blocked in the wait queue, we
  // should see the weight of the owning thread increased to the total of its
  // base weight, and weights of all of the threads blocked behind it.
  {
    SingleChainLockGuard guard{IrqSaveOption, ot->thread->get_lock(),
                               CLT_TAG("vmo_stack_owned_loaned_pages_interval_test (3)")};
    const SchedulerState::EffectiveProfile& ep = ot->thread->scheduler_state().effective_profile();
    constexpr SchedWeight kExpectedWeight =
        kOwnerThreadBaseWeight + (kWaitingThreadCount * kBlockedThreadBaseWeight);
    ASSERT_TRUE(ep.IsFair());
    ASSERT_EQ(kExpectedWeight.raw_value(), ep.fair.weight.raw_value());
  }

  // Wait a bit, the threads should still be blocked.
  Thread::Current::SleepRelative(ZX_MSEC(100));
  {
    for (auto& wt : *waiting_threads) {
      SingleChainLockGuard guard{IrqSaveOption, wt.thread->get_lock(),
                                 CLT_TAG("vmo_stack_owned_loaned_pages_interval_test (4)")};
      ASSERT_EQ(THREAD_BLOCKED, wt.thread->state());
    }
  }

  // Tell the owner thread that it can destroy its StackOwnedLoanedPagesInterval
  // object. This should release all of the blocked thread.  One they are all
  // unblocked, we expect to see the owner thread's priority relax back down to
  // its base priority.
  ot->release_stack_ownership.Signal();
  for (auto& wt : *waiting_threads) {
    while (true) {
      {
        SingleChainLockGuard guard{IrqSaveOption, wt.thread->get_lock(),
                                   CLT_TAG("vmo_stack_owned_loaned_pages_interval_test (5)")};
        if (wt.thread->state() != THREAD_BLOCKED) {
          break;
        }
      }
      Thread::Current::SleepRelative(ZX_MSEC(1));
    }
  }

  // Verify that the profile of the owner thread has relaxed.
  {
    SingleChainLockGuard guard{IrqSaveOption, ot->thread->get_lock(),
                               CLT_TAG("vmo_stack_owned_loaned_pages_interval_test (6)")};
    const SchedulerState::EffectiveProfile& ep = ot->thread->scheduler_state().effective_profile();
    ASSERT_TRUE(ep.IsFair());
    ASSERT_EQ(kOwnerThreadBaseWeight.raw_value(), ep.fair.weight.raw_value());
  }

  // Test is finished.  Let our fit::defer handle all of the cleanup.
  END_TEST;
}

static bool vmo_dirty_pages_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

  // Create a pager-backed VMO with a single page.
  fbl::RefPtr<VmObjectPaged> vmo;
  vm_page_t* page;
  ASSERT_OK(make_committed_pager_vmo(1, /*trap_dirty=*/true, /*resizable=*/false, &page, &vmo));

  // Newly created page should be in the first pager backed page queue.
  size_t queue;
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // Rotate the queues and check the page moves.
  pmm_page_queues()->RotateReclaimQueues();
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(1u, queue);

  // Accessing the page should move it back to the first queue.
  EXPECT_OK(vmo->GetPageBlocking(0, VMM_PF_FLAG_SW_FAULT, nullptr, nullptr, nullptr));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // Now simulate a write to the page. This should move the page to the dirty queue.
  ASSERT_OK(vmo->DirtyPages(0, PAGE_SIZE));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));
  EXPECT_GT(pmm_page_queues()->QueueCounts().pager_backed_dirty, 0u);

  // Should not be able to evict a dirty page.
  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);

  // Accessing the page again should not move the page out of the dirty queue.
  EXPECT_OK(vmo->GetPageBlocking(0, VMM_PF_FLAG_SW_FAULT, nullptr, nullptr, nullptr));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));

  END_TEST;
}

static bool vmo_dirty_pages_writeback_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

  // Create a pager-backed VMO with a single page.
  fbl::RefPtr<VmObjectPaged> vmo;
  vm_page_t* page;
  ASSERT_OK(make_committed_pager_vmo(1, /*trap_dirty=*/true, /*resizable=*/false, &page, &vmo));

  // Newly created page should be in the first pager backed page queue.
  size_t queue;
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // Now simulate a write to the page. This should move the page to the dirty queue.
  ASSERT_OK(vmo->DirtyPages(0, PAGE_SIZE));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));

  // Should not be able to evict a dirty page.
  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);

  // Begin writeback on the page. This should still keep the page in the dirty queue.
  ASSERT_OK(vmo->WritebackBegin(0, PAGE_SIZE, false));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));

  // Should not be able to evict a dirty page.
  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);

  // Accessing the page should not move the page out of the dirty queue either.
  ASSERT_OK(vmo->GetPageBlocking(0, VMM_PF_FLAG_SW_FAULT, nullptr, nullptr, nullptr));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));

  // Should not be able to evict a dirty page.
  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);

  // End writeback on the page. This should finally move the page out of the dirty queue.
  ASSERT_OK(vmo->WritebackEnd(0, PAGE_SIZE));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // We should be able to rotate the page as usual.
  pmm_page_queues()->RotateReclaimQueues();
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(1u, queue);

  // Another write moves the page back to the Dirty queue.
  ASSERT_OK(vmo->DirtyPages(0, PAGE_SIZE));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));

  // Clean the page again, and try to evict it.
  ASSERT_OK(vmo->WritebackBegin(0, PAGE_SIZE, false));
  ASSERT_OK(vmo->WritebackEnd(0, PAGE_SIZE));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // We should now be able to evict the page.
  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 1u);

  END_TEST;
}

static bool vmo_dirty_pages_with_hints_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

  // Create a pager-backed VMO with a single page.
  fbl::RefPtr<VmObjectPaged> vmo;
  vm_page_t* page;
  ASSERT_OK(make_committed_pager_vmo(1, /*trap_dirty=*/true, /*resizable=*/false, &page, &vmo));

  // Newly created page should be in the first pager backed page queue.
  size_t queue;
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // Now simulate a write to the page. This should move the page to the dirty queue.
  ASSERT_OK(vmo->DirtyPages(0, PAGE_SIZE));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));

  // Hint DontNeed on the page. It should remain in the dirty queue.
  ASSERT_OK(vmo->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::DontNeed));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaimDontNeed(page));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));

  // Should not be able to evict a dirty page.
  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);

  // Hint AlwaysNeed on the page. It should remain in the dirty queue.
  ASSERT_OK(vmo->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::AlwaysNeed));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(page));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaimDontNeed(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));

  // Clean the page.
  ASSERT_OK(vmo->WritebackBegin(0, PAGE_SIZE, false));
  ASSERT_OK(vmo->WritebackEnd(0, PAGE_SIZE));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // Eviction should fail still because we hinted AlwaysNeed previously.
  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // Eviction should succeed if we ignore the hint.
  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Ignore, nullptr), 1u);

  // Reset the vmo and retry some of the same actions as before, this time dirtying
  // the page *after* hinting.
  vmo.reset();

  ASSERT_OK(make_committed_pager_vmo(1, /*trap_dirty=*/true, /*resizable=*/false, &page, &vmo));

  // Newly created page should be in the first pager backed page queue.
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page, &queue));
  EXPECT_EQ(0u, queue);

  // Hint DontNeed on the page. This should move the page to the DontNeed queue.
  ASSERT_OK(vmo->HintRange(0, PAGE_SIZE, VmObject::EvictionHint::DontNeed));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(page));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaimDontNeed(page));

  // Write to the page now. This should move it to the dirty queue.
  ASSERT_OK(vmo->DirtyPages(0, PAGE_SIZE));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(page));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaimDontNeed(page));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));

  // Should not be able to evict a dirty page.
  ASSERT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, nullptr), 0u);

  END_TEST;
}

// Tests that pinning pager-backed pages retains backlink information.
static bool vmo_pinning_backlink_test() {
  BEGIN_TEST;
  // Disable the page scanner as this test would be flaky if our pages get evicted by someone else.
  AutoVmScannerDisable scanner_disable;

  // Create a pager-backed VMO with two pages, so we can verify a non-zero offset value.
  fbl::RefPtr<VmObjectPaged> vmo;
  vm_page_t* pages[2];
  zx_status_t status =
      make_committed_pager_vmo(2, /*trap_dirty=*/false, /*resizable=*/false, pages, &vmo);
  ASSERT_EQ(ZX_OK, status);

  // Pages should be in the pager queue.
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(pages[0]));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(pages[1]));

  // Verify backlink information.
  auto cow = vmo->DebugGetCowPages().get();
  EXPECT_EQ(cow, pages[0]->object.get_object());
  EXPECT_EQ(0u, pages[0]->object.get_page_offset());
  EXPECT_EQ(cow, pages[1]->object.get_object());
  EXPECT_EQ(static_cast<uint64_t>(PAGE_SIZE), pages[1]->object.get_page_offset());

  // Pin the pages.
  status = vmo->CommitRangePinned(0, 2 * PAGE_SIZE, false);
  ASSERT_EQ(ZX_OK, status);

  // Pages might get swapped out on pinning if they were loaned. Look them up again.
  pages[0] = vmo->DebugGetPage(0);
  pages[1] = vmo->DebugGetPage(PAGE_SIZE);

  // Pages should be in the wired queue.
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsWired(pages[0]));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsWired(pages[1]));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(pages[0]));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsReclaim(pages[1]));

  // Moving to the wired queue should retain backlink information.
  EXPECT_EQ(cow, pages[0]->object.get_object());
  EXPECT_EQ(0u, pages[0]->object.get_page_offset());
  EXPECT_EQ(cow, pages[1]->object.get_object());
  EXPECT_EQ(static_cast<uint64_t>(PAGE_SIZE), pages[1]->object.get_page_offset());

  // Unpin the pages.
  vmo->Unpin(0, 2 * PAGE_SIZE);

  // Pages should be back in the pager queue.
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsWired(pages[0]));
  EXPECT_FALSE(pmm_page_queues()->DebugPageIsWired(pages[1]));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(pages[0]));
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(pages[1]));

  // Verify backlink information again.
  EXPECT_EQ(cow, pages[0]->object.get_object());
  EXPECT_EQ(0u, pages[0]->object.get_page_offset());
  EXPECT_EQ(cow, pages[1]->object.get_object());
  EXPECT_EQ(static_cast<uint64_t>(PAGE_SIZE), pages[1]->object.get_page_offset());

  END_TEST;
}

static bool vmo_supply_compressed_pages_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;
  // Need a working compressor.
  auto compression = Pmm::Node().GetPageCompression();
  if (!compression) {
    END_TEST;
  }

  fbl::RefPtr<VmObjectPaged> vmop;
  ASSERT_OK(make_uncommitted_pager_vmo(1, false, false, &vmop));

  fbl::RefPtr<VmObjectPaged> vmo;
  ASSERT_OK(VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE, &vmo));

  // Write non-zero data to the VMO so we can compress it.
  uint64_t data = 42;
  EXPECT_OK(vmo->Write(&data, 0, sizeof(data)));

  vm_page_t* page;
  zx_status_t status = vmo->GetPageBlocking(0, 0, nullptr, &page, nullptr);
  ASSERT_OK(status);

  {
    auto compressor = compression->AcquireCompressor();
    EXPECT_OK(compressor.get().Arm());
    EXPECT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Follow, &compressor.get()),
              1u);
  }
  EXPECT_TRUE(make_private_attribution_counts(0, PAGE_SIZE) == vmo->GetAttributedMemory());

  // Taking the pages should work.
  VmPageSpliceList pl;
  EXPECT_OK(vmo->TakePages(0, PAGE_SIZE, &pl));
  EXPECT_TRUE((VmObject::AttributionCounts{}) == vmo->GetAttributedMemory());

  // After being supplied the pager backed VMO should not have compressed pages.
  EXPECT_OK(vmop->SupplyPages(0, PAGE_SIZE, &pl, SupplyOptions::PagerSupply));
  EXPECT_TRUE(make_private_attribution_counts(PAGE_SIZE, 0) == vmop->GetAttributedMemory());

  END_TEST;
}

static bool is_page_zero(vm_page_t* page) {
  auto* base = reinterpret_cast<uint64_t*>(paddr_to_physmap(page->paddr()));
  for (size_t i = 0; i < PAGE_SIZE / sizeof(uint64_t); i++) {
    if (base[i] != 0)
      return false;
  }
  return true;
}

// Tests that ZeroRange does not remove pinned pages. Regression test for
// https://fxbug.dev/42052452.
static bool vmo_zero_pinned_test() {
  BEGIN_TEST;

  // Create a non pager-backed VMO.
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE, &vmo);
  ASSERT_EQ(ZX_OK, status);

  // Pin the page for write.
  status = vmo->CommitRangePinned(0, PAGE_SIZE, true);
  ASSERT_EQ(ZX_OK, status);

  // Write non-zero content to the page.
  vm_page_t* page = vmo->DebugGetPage(0);
  *reinterpret_cast<uint8_t*>(paddr_to_physmap(page->paddr())) = 0xff;

  // Zero the page and check that it is not removed.
  status = vmo->ZeroRange(0, PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_EQ(page, vmo->DebugGetPage(0));

  // The page should be zero.
  EXPECT_TRUE(is_page_zero(page));

  vmo->Unpin(0, PAGE_SIZE);

  // Create a pager-backed VMO.
  fbl::RefPtr<VmObjectPaged> pager_vmo;
  vm_page_t* old_page;
  status =
      make_committed_pager_vmo(1, /*trap_dirty=*/false, /*resizable=*/true, &old_page, &pager_vmo);
  ASSERT_EQ(ZX_OK, status);

  // Pin the page for write.
  status = pager_vmo->CommitRangePinned(0, PAGE_SIZE, true);
  ASSERT_EQ(ZX_OK, status);

  // Write non-zero content to the page. Lookup the page again, as pinning might have switched out
  // the page if it was originally loaned.
  old_page = pager_vmo->DebugGetPage(0);
  *reinterpret_cast<uint8_t*>(paddr_to_physmap(old_page->paddr())) = 0xff;

  // Zero the page and check that it is not removed.
  status = pager_vmo->ZeroRange(0, PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_EQ(old_page, pager_vmo->DebugGetPage(0));

  // The page should be zero.
  EXPECT_TRUE(is_page_zero(old_page));

  // Resize the VMO up, and pin a page in the newly extended range.
  status = pager_vmo->Resize(2 * PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status);
  status = pager_vmo->CommitRangePinned(PAGE_SIZE, PAGE_SIZE, true);
  ASSERT_EQ(ZX_OK, status);

  // Write non-zero content to the page.
  vm_page_t* new_page = pager_vmo->DebugGetPage(PAGE_SIZE);
  *reinterpret_cast<uint8_t*>(paddr_to_physmap(new_page->paddr())) = 0xff;

  // Zero the new page, and ensure that it is not removed.
  status = pager_vmo->ZeroRange(PAGE_SIZE, PAGE_SIZE);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_EQ(new_page, pager_vmo->DebugGetPage(PAGE_SIZE));

  // The page should be zero.
  EXPECT_TRUE(is_page_zero(new_page));

  pager_vmo->Unpin(0, 2 * PAGE_SIZE);

  END_TEST;
}

static bool vmo_pinned_wrapper_test() {
  BEGIN_TEST;

  {
    fbl::RefPtr<VmObjectPaged> vmo;
    zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE, &vmo);
    ASSERT_EQ(ZX_OK, status);

    PinnedVmObject pinned;
    status = PinnedVmObject::Create(vmo, 0, PAGE_SIZE, true, &pinned);
    EXPECT_OK(status);
    status = PinnedVmObject::Create(vmo, 0, PAGE_SIZE, true, &pinned);
    EXPECT_OK(status);
  }

  {
    fbl::RefPtr<VmObjectPaged> vmo;
    zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE, &vmo);
    ASSERT_EQ(ZX_OK, status);

    PinnedVmObject pinned;
    status = PinnedVmObject::Create(vmo, 0, PAGE_SIZE, true, &pinned);
    EXPECT_OK(status);

    PinnedVmObject empty;
    pinned = ktl::move(empty);
  }

  {
    fbl::RefPtr<VmObjectPaged> vmo;
    zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE, &vmo);
    ASSERT_EQ(ZX_OK, status);

    PinnedVmObject pinned;
    status = PinnedVmObject::Create(vmo, 0, PAGE_SIZE, true, &pinned);
    EXPECT_OK(status);

    PinnedVmObject empty;
    empty = ktl::move(pinned);
  }

  {
    fbl::RefPtr<VmObjectPaged> vmo;
    zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE, &vmo);
    ASSERT_EQ(ZX_OK, status);

    PinnedVmObject pinned1;
    status = PinnedVmObject::Create(vmo, 0, PAGE_SIZE, true, &pinned1);
    EXPECT_OK(status);

    PinnedVmObject pinned2;
    status = PinnedVmObject::Create(vmo, 0, PAGE_SIZE, true, &pinned2);
    EXPECT_OK(status);

    pinned1 = ktl::move(pinned2);
  }

  END_TEST;
}

// Tests that dirty pages cannot be deduped.
static bool vmo_dedup_dirty_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  fbl::RefPtr<VmObjectPaged> vmo;
  vm_page_t* page;
  zx_status_t status =
      make_committed_pager_vmo(1, /*trap_dirty=*/false, /*resizable=*/false, &page, &vmo);
  ASSERT_EQ(ZX_OK, status);

  // Our page should now be in a pager backed page queue.
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page));

  // The page is clean. We should be able to dedup the page.
  EXPECT_TRUE(vmo->DebugGetCowPages()->DedupZeroPage(page, 0));

  // No committed pages remaining.
  EXPECT_TRUE((vm::AttributionCounts{}) == vmo->GetAttributedMemory());

  // Write to the page making it dirty.
  uint8_t data = 0xff;
  status = vmo->Write(&data, 0, sizeof(data));
  ASSERT_EQ(ZX_OK, status);

  // The page should now be dirty.
  page = vmo->DebugGetPage(0);
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));

  // We should not be able to dedup the page.
  EXPECT_FALSE(vmo->DebugGetCowPages()->DedupZeroPage(page, 0));
  EXPECT_TRUE(make_private_attribution_counts(PAGE_SIZE, 0) == vmo->GetAttributedMemory());

  END_TEST;
}

// Test that attempting to reclaim pages from a high priority VMO will not work.
static bool vmo_high_priority_reclaim_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  fbl::RefPtr<VmObjectPaged> vmo;
  vm_page_t* page;
  zx_status_t status =
      make_committed_pager_vmo(1, /*trap_dirty=*/false, /*resizable=*/false, &page, &vmo);
  ASSERT_EQ(ZX_OK, status);

  auto change_priority = [&vmo](int64_t delta) {
    Guard<CriticalMutex> guard{vmo->lock()};
    vmo->ChangeHighPriorityCountLocked(delta);
  };

  // Our page should be in a pager backed page queue.
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page));

  // Indicate our VMO is high priority.
  change_priority(1);

  // Our page should now be in a high priority page queue.
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsHighPriority(page));
  EXPECT_GT(pmm_page_queues()->QueueCounts().high_priority, 0u);

  // Attempting to reclaim should fail.
  EXPECT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Ignore, nullptr), 0u);

  // Page should still be in the queue.
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsHighPriority(page));

  // Switch to a regular anonymous VMO.
  change_priority(-1);

  // Page should be back in the regular pager backed page queue.
  EXPECT_TRUE(pmm_page_queues()->DebugPageIsReclaim(page));

  vmo.reset();
  status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE, &vmo);
  ASSERT_EQ(ZX_OK, status);
  change_priority(1);

  // Commit a single page.
  EXPECT_OK(vmo->CommitRange(0, PAGE_SIZE));
  page = vmo->DebugGetPage(0);

  // Deduping as zero should fail.
  EXPECT_FALSE(vmo->DebugGetCowPages()->DedupZeroPage(page, 0));
  EXPECT_EQ(page, vmo->DebugGetPage(0));

  // If we have a compressor, then compressing should also fail.
  VmCompression* compression = Pmm::Node().GetPageCompression();
  if (compression) {
    auto compressor = compression->AcquireCompressor();
    EXPECT_OK(compressor.get().Arm());
    EXPECT_EQ(reclaim_page(vmo, page, 0, VmCowPages::EvictionHintAction::Ignore, &compressor.get()),
              0u);
    EXPECT_EQ(page, vmo->DebugGetPage(0));
  }

  change_priority(-1);

  END_TEST;
}

// Tests that snapshot modified behaves as expected
static bool vmo_snapshot_modified_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  // Create 3 page, pager-backed VMO.
  constexpr uint64_t kNumPages = 3;
  vm_page_t* pages[kNumPages];
  auto alloc_size = kNumPages * PAGE_SIZE;
  fbl::RefPtr<VmObjectPaged> vmo;

  zx_status_t status = make_committed_pager_vmo(kNumPages, false, false, pages, &vmo);
  ASSERT_EQ(ZX_OK, status);
  vmo->set_user_id(42);

  // Snapshot-modified all 3 pages of root.
  fbl::RefPtr<VmObject> clone;
  status = vmo->CreateClone(Resizability::NonResizable, SnapshotType::Modified, 0, alloc_size,
                            false, &clone);
  ASSERT_EQ(ZX_OK, status, "vmobject full clone\n");
  ASSERT_NONNULL(clone, "vmobject full clone\n");
  clone->set_user_id(43);

  // Hang another snapshot-modified clone off root that only sees the first page.
  fbl::RefPtr<VmObject> clone2;
  status = vmo->CreateClone(Resizability::NonResizable, SnapshotType::Modified, 0, PAGE_SIZE, false,
                            &clone2);
  ASSERT_EQ(ZX_OK, status, "vmobject partial clone\n");
  ASSERT_NONNULL(clone2, "vmobject partial clone\n");
  clone2->set_user_id(44);

  // Ensures all pages are attributed to the root VMO, and not the clones, as the root VMO is not a
  // hidden node.
  EXPECT_TRUE(make_private_attribution_counts(alloc_size, 0) == vmo->GetAttributedMemory());
  EXPECT_TRUE((vm::AttributionCounts{}) == clone->GetAttributedMemory());
  EXPECT_TRUE((vm::AttributionCounts{}) == clone2->GetAttributedMemory());

  // COW page into clone & check that it is attributed.
  uint8_t data = 0xff;
  status = clone->Write(&data, 0, sizeof(data));
  ASSERT_EQ(ZX_OK, status);

  EXPECT_TRUE(make_private_attribution_counts(PAGE_SIZE, 0) == clone->GetAttributedMemory());

  // Try to COW a page into clone2 that it doesn't see.
  status = clone2->Write(&data, PAGE_SIZE, sizeof(data));
  ASSERT_EQ(ZX_ERR_OUT_OF_RANGE, status);

  // Call snapshot-modified again on the full clone, which will create a hidden parent.
  fbl::RefPtr<VmObject> snapshot;
  status = clone->CreateClone(Resizability::NonResizable, SnapshotType::Modified, 0,
                              PAGE_SIZE * kNumPages, false, &snapshot);
  ASSERT_EQ(ZX_OK, status, "vmobject snapshot-modified\n");
  ASSERT_NONNULL(snapshot, "vmobject snapshot-modified clone\n");

  // Pages in hidden parent will be attributed to both children.
  EXPECT_TRUE((vm::AttributionCounts{.uncompressed_bytes = PAGE_SIZE,
                                     .scaled_uncompressed_bytes = vm::FractionalBytes(
                                         PAGE_SIZE, 2)}) == clone->GetAttributedMemory());
  EXPECT_TRUE((vm::AttributionCounts{.uncompressed_bytes = PAGE_SIZE,
                                     .scaled_uncompressed_bytes = vm::FractionalBytes(
                                         PAGE_SIZE, 2)}) == snapshot->GetAttributedMemory());

  // Calling CreateClone directly with SnapshotAtLeastOnWrite should upgrade to snapshot-modified.
  fbl::RefPtr<VmObject> atleastonwrite;
  status = clone->CreateClone(Resizability::NonResizable, SnapshotType::OnWrite, 0, alloc_size,
                              false, &atleastonwrite);
  ASSERT_EQ(ZX_OK, status, "vmobject snapshot-at-least-on-write clone.\n");
  ASSERT_NONNULL(atleastonwrite, "vmobject snapshot-at-least-on-write clone\n");

  // Create a slice of the first two pages of the root VMO.
  auto kSliceSize = 2 * PAGE_SIZE;
  fbl::RefPtr<VmObject> slice;
  ASSERT_OK(vmo->CreateChildSlice(0, kSliceSize, false, &slice));
  ASSERT_NONNULL(slice, "slice root vmo");
  slice->set_user_id(45);

  // The oot VMO should have 3 children at this point.
  ASSERT_EQ(vmo->num_children(), (uint32_t)3);

  // Snapshot-modified of root-slice should work.
  fbl::RefPtr<VmObject> slicesnapshot;
  status = slice->CreateClone(Resizability::NonResizable, SnapshotType::Modified, 0, kSliceSize,
                              false, &slicesnapshot);
  ASSERT_EQ(ZX_OK, status, "snapshot-modified root-slice\n");
  ASSERT_NONNULL(slicesnapshot, "snapshot modified root-slice\n");
  slicesnapshot->set_user_id(46);

  // At the VMO level, the slice should see the snapshot as a child.
  ASSERT_EQ(vmo->num_children(), (uint32_t)3);
  ASSERT_EQ(slice->num_children(), (uint32_t)1);

  // The cow pages, however, should be hung off the root VMO.
  auto slicesnapshot_p = static_cast<VmObjectPaged*>(slicesnapshot.get());
  auto vmo_cow_pages = vmo->DebugGetCowPages();
  auto slicesnapshot_cow_pages = slicesnapshot_p->DebugGetCowPages();

  ASSERT_EQ(slicesnapshot_cow_pages->DebugGetParent().get(), vmo_cow_pages.get());

  // Create a slice of the clone of the root-slice.
  fbl::RefPtr<VmObject> slicesnapshot_slice;
  status = slicesnapshot->CreateClone(Resizability::NonResizable, SnapshotType::Modified, 0,
                                      kSliceSize, false, &slicesnapshot_slice);
  ASSERT_EQ(ZX_OK, status, "slice snapshot-modified-root-slice\n");
  ASSERT_NONNULL(slicesnapshot_slice, "slice snapshot-modified-root-slice\n");
  slicesnapshot_slice->set_user_id(47);

  // Check that snapshot-modified will work again on the snapshot-modified clone of the slice.
  fbl::RefPtr<VmObject> slicesnapshot2;
  status = slicesnapshot->CreateClone(Resizability::NonResizable, SnapshotType::Modified, 0,
                                      kSliceSize, false, &slicesnapshot2);
  ASSERT_EQ(ZX_OK, status, "snapshot-modified root-slice-snapshot\n");
  ASSERT_NONNULL(slicesnapshot2, "snapshot-modified root-slice-snapshot\n");
  slicesnapshot2->set_user_id(48);

  // Create a slice of a clone
  fbl::RefPtr<VmObject> cloneslice;
  ASSERT_OK(clone->CreateChildSlice(0, kSliceSize, false, &cloneslice));
  ASSERT_NONNULL(slice, "slice root vmo");

  // Snapshot-modified should not be allowed on a slice of a clone.
  fbl::RefPtr<VmObject> cloneslicesnapshot;
  status = cloneslice->CreateClone(Resizability::NonResizable, SnapshotType::Modified, 0,
                                   kSliceSize, false, &cloneslicesnapshot);
  ASSERT_EQ(ZX_ERR_NOT_SUPPORTED, status, "snapshot-modified clone-slice\n");
  ASSERT_NULL(cloneslicesnapshot, "snapshot-modified clone-slice\n");

  // Tests that SnapshotModified will be upgraded to Snapshot when used on an anonymous VMO.
  fbl::RefPtr<VmObjectPaged> anon_vmo;
  status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, alloc_size, &anon_vmo);
  ASSERT_EQ(ZX_OK, status);
  anon_vmo->set_user_id(0x49);

  fbl::RefPtr<VmObject> anon_clone;
  status = anon_vmo->CreateClone(Resizability::NonResizable, SnapshotType::Modified, 0, PAGE_SIZE,
                                 true, &anon_clone);
  ASSERT_OK(status);
  anon_clone->set_user_id(0x50);

  // Check that a hidden, common cow pages was made.
  auto anon_clone_p = static_cast<VmObjectPaged*>(anon_clone.get());
  auto anon_vmo_cow_pages = anon_vmo->DebugGetCowPages();
  auto anon_clone_cow_pages = anon_clone_p->DebugGetCowPages();

  ASSERT_EQ(anon_clone_cow_pages->DebugGetParent().get(),
            anon_vmo_cow_pages->DebugGetParent().get());

  // Snapshot-modified should also be upgraded when used on a SNAPSHOT clone.
  fbl::RefPtr<VmObject> anon_snapshot;
  status = anon_clone->CreateClone(Resizability::NonResizable, SnapshotType::Modified, 0, PAGE_SIZE,
                                   true, &anon_snapshot);
  ASSERT_OK(status);
  anon_snapshot->set_user_id(0x51);

  // Snapshot-modified shold not be allowed on a unidirectional chain of length > 2
  fbl::RefPtr<VmObject> chain1;
  status = vmo->CreateClone(Resizability::NonResizable, SnapshotType::OnWrite, 0, PAGE_SIZE, true,
                            &chain1);
  ASSERT_OK(status);
  chain1->set_user_id(0x52);
  uint64_t data1 = 42;
  EXPECT_OK(chain1->Write(&data1, 0, sizeof(data)));

  fbl::RefPtr<VmObject> chain2;
  status = chain1->CreateClone(Resizability::NonResizable, SnapshotType::OnWrite, 0, PAGE_SIZE,
                               true, &chain2);
  ASSERT_OK(status);
  chain2->set_user_id(0x51);
  uint64_t data2 = 43;
  EXPECT_OK(chain2->Write(&data2, 0, sizeof(data)));

  fbl::RefPtr<VmObject> chain_snap;
  status = chain2->CreateClone(Resizability::NonResizable, SnapshotType::Modified, 0, PAGE_SIZE,
                               true, &chain_snap);
  ASSERT_EQ(ZX_ERR_NOT_SUPPORTED, status, "snapshot-modified unidirectional chain\n");

  END_TEST;
}

// Regression test for https://fxbug.dev/42080926. Concurrent pinning of different ranges in a
// contiguous VMO that has its pages loaned.
static bool vmo_pin_race_loaned_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  const uint32_t kTryCount = 5000;
  for (uint32_t try_ordinal = 0; try_ordinal < kTryCount; ++try_ordinal) {
    bool loaning_was_enabled = pmm_physical_page_borrowing_config()->is_loaning_enabled();
    bool borrowing_was_enabled =
        pmm_physical_page_borrowing_config()->is_borrowing_in_supplypages_enabled();
    pmm_physical_page_borrowing_config()->set_loaning_enabled(true);
    pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(true);
    auto cleanup = fit::defer([loaning_was_enabled, borrowing_was_enabled] {
      pmm_physical_page_borrowing_config()->set_loaning_enabled(loaning_was_enabled);
      pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(
          borrowing_was_enabled);
    });

    const int kNumLoaned = 10;
    fbl::RefPtr<VmObjectPaged> contiguous_vmo;
    zx_status_t status =
        VmObjectPaged::CreateContiguous(PMM_ALLOC_FLAG_ANY, (kNumLoaned + 1) * PAGE_SIZE,
                                        /*alignment_log2=*/0, &contiguous_vmo);
    ASSERT_EQ(ZX_OK, status);
    vm_page_t* pages[kNumLoaned];
    for (int i = 0; i < kNumLoaned; i++) {
      pages[i] = contiguous_vmo->DebugGetPage((i + 1) * PAGE_SIZE);
    }
    status = contiguous_vmo->DecommitRange(PAGE_SIZE, kNumLoaned * PAGE_SIZE);
    ASSERT_TRUE(status == ZX_OK);

    uint32_t iteration_count = 0;
    const uint32_t kMaxIterations = 1000;
    int loaned = 0;
    do {
      // Create a pager-backed VMO with a single page.
      fbl::RefPtr<VmObjectPaged> vmo;
      vm_page_t* page;
      status = make_committed_pager_vmo(1, /*trap_dirty=*/false, /*resizable=*/false, &page, &vmo);
      ASSERT_EQ(ZX_OK, status);
      ++iteration_count;
      for (int i = 0; i < kNumLoaned; i++) {
        if (page == pages[i]) {
          ASSERT_TRUE(page->is_loaned());
          loaned++;
        }
      }
    } while (loaned < kNumLoaned && iteration_count < kMaxIterations);

    // If we hit this iteration count, something almost certainly went wrong...
    ASSERT_TRUE(iteration_count < kMaxIterations);
    ASSERT_EQ(kNumLoaned, loaned);

    Thread* threads[kNumLoaned];
    struct thread_state {
      VmObjectPaged* vmo;
      int index;
    } states[kNumLoaned];

    for (int i = 0; i < kNumLoaned; i++) {
      states[i].vmo = contiguous_vmo.get();
      states[i].index = i;
      threads[i] = Thread::Create(
          "worker",
          [](void* arg) -> int {
            auto state = static_cast<struct thread_state*>(arg);

            zx_status_t status;
            if (state->index == 0) {
              status = state->vmo->CommitRangePinned(0, 2 * PAGE_SIZE, false);
            } else {
              status =
                  state->vmo->CommitRangePinned((state->index + 1) * PAGE_SIZE, PAGE_SIZE, false);
            }
            if (status != ZX_OK) {
              return -1;
            }
            return 0;
          },
          &states[i], DEFAULT_PRIORITY);
    }

    for (int i = 0; i < kNumLoaned; i++) {
      threads[i]->Resume();
    }

    for (int i = 0; i < kNumLoaned; i++) {
      int ret;
      threads[i]->Join(&ret, ZX_TIME_INFINITE);
      EXPECT_EQ(0, ret);
    }

    for (int i = 0; i < kNumLoaned; i++) {
      EXPECT_EQ(pages[i], contiguous_vmo->DebugGetPage((i + 1) * PAGE_SIZE));
    }
    contiguous_vmo->Unpin(0, (kNumLoaned + 1) * PAGE_SIZE);
  }

  END_TEST;
}

static bool vmo_prefetch_compressed_pages_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  // Need a working compressor.
  auto compression = Pmm::Node().GetPageCompression();
  if (!compression) {
    END_TEST;
  }

  // Create a VMO and commit some pages to it, ensuring they have non-zero content.
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, PAGE_SIZE * 2, &vmo);
  ASSERT_OK(status);
  uint64_t data = 42;
  EXPECT_OK(vmo->Write(&data, 0, sizeof(data)));
  EXPECT_OK(vmo->Write(&data, PAGE_SIZE, sizeof(data)));
  EXPECT_TRUE(make_private_attribution_counts(2ul * PAGE_SIZE, 0) == vmo->GetAttributedMemory())

  // Compress the second page.
  vm_page_t* page;
  status = vmo->GetPageBlocking(PAGE_SIZE, 0, nullptr, &page, nullptr);
  ASSERT_OK(status);
  {
    auto compressor = compression->AcquireCompressor();
    EXPECT_OK(compressor.get().Arm());
    ASSERT_TRUE(reclaim_page(vmo, page, PAGE_SIZE, VmCowPages::EvictionHintAction::Follow,
                             &compressor.get()));
  }
  EXPECT_TRUE(make_private_attribution_counts(PAGE_SIZE, PAGE_SIZE) == vmo->GetAttributedMemory())

  // Prefetch the entire VMO.
  EXPECT_OK(vmo->PrefetchRange(0, PAGE_SIZE * 2));

  // Both pages should be back to being uncompressed.
  EXPECT_TRUE(make_private_attribution_counts(2ul * PAGE_SIZE, 0) == vmo->GetAttributedMemory())

  END_TEST;
}

// Check that committed ranges in children correctly have range updates skipped.
static bool vmo_skip_range_update_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;
  constexpr uint64_t kNumPages = 16;

  fbl::RefPtr<VmObjectPaged> vmo;
  ASSERT_OK(VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, PAGE_SIZE * kNumPages, &vmo));

  EXPECT_OK(vmo->CommitRange(0, PAGE_SIZE * kNumPages));

  fbl::RefPtr<VmObject> child;
  ASSERT_OK(vmo->CreateClone(Resizability::NonResizable, SnapshotType::Full, 0u,
                             PAGE_SIZE * kNumPages, false, &child));

  // Fork some pages into the child to have some regions that should be able to avoid range updates.
  for (auto page : {4, 5, 6, 10, 11, 12}) {
    uint64_t data = 42;
    EXPECT_OK(child->Write(&data, PAGE_SIZE * page, sizeof(data)));
  }

  // Create a user memory mapping to check if unmaps do and do not get performed.
  ktl::unique_ptr<testing::UserMemory> user_memory = testing::UserMemory::Create(child, 0, 0);
  ASSERT_TRUE(user_memory);

  // Reach into the hidden parent so we can directly perform range updates.
  fbl::RefPtr<VmCowPages> hidden_parent = vmo->DebugGetCowPages()->DebugGetParent();
  ASSERT_TRUE(hidden_parent);

  struct {
    uint64_t page_start;
    uint64_t num_pages;
    ktl::array<int, kNumPages> unmapped;
  } test_ranges[] = {
      // Simple range that is not covered in the child should get unmapped
      {0, 1, {0, -1}},
      // Various ranges fully covered by the child should have no unmappings
      {4, 1, {-1}},
      {4, 3, {-1}},
      {6, 1, {-1}},
      // Ranges that partially touch a single committed range should get trimmed
      {3, 2, {3, -1}},
      {6, 2, {7, -1}},
      // Range that spans a single gap that sees the parent should get trimmed at both ends to just
      // that gap.
      {4, 9, {7, 8, 9, -1}},
      // Spanning across a committed range causes us to still have to unnecessarily unmap.
      {3, 10, {3, 4, 5, 6, 7, 8, 9, -1}},
      {4, 10, {7, 8, 9, 10, 11, 12, 13, -1}},
      {3, 11, {3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, -1}},
  };

  for (auto& range : test_ranges) {
    // Ensure all the mappings start populated.
    for (uint64_t i = 0; i < kNumPages; i++) {
      user_memory->get<char>(i * PAGE_SIZE);
      paddr_t paddr;
      uint mmu_flags;
      EXPECT_OK(user_memory->aspace()->arch_aspace().Query(user_memory->base() + i * PAGE_SIZE,
                                                           &paddr, &mmu_flags));
    }
    // Perform the requested range update.
    {
      Guard<CriticalMutex> guard{hidden_parent->lock()};
      hidden_parent->RangeChangeUpdateLocked(
          VmCowRange(range.page_start * PAGE_SIZE, range.num_pages * PAGE_SIZE),
          VmCowPages::RangeChangeOp::Unmap);
    }
    // Check all the mappings are either there or not there as expected.
    bool expected[kNumPages];
    for (uint64_t i = 0; i < kNumPages; i++) {
      expected[i] = true;
    }
    for (auto page : range.unmapped) {
      // page of -1 is a sentinel as we cannot use the default 0 as sentinel.
      if (page == -1) {
        break;
      }
      expected[page] = false;
    }
    for (uint64_t i = 0; i < kNumPages; i++) {
      paddr_t paddr;
      uint mmu_flags;
      zx_status_t status = user_memory->aspace()->arch_aspace().Query(
          user_memory->base() + i * PAGE_SIZE, &paddr, &mmu_flags);
      EXPECT_EQ(expected[i] ? ZX_OK : ZX_ERR_NOT_FOUND, status);
    }
  }

  END_TEST;
}

// Test stream functionality in kernel objects.
static bool vmo_user_stream_size_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;

  // 4 page VMO.
  fbl::RefPtr<VmObjectPaged> vmo;
  ASSERT_OK(VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, 4 * PAGE_SIZE, &vmo));

  {
    Guard<CriticalMutex> guard{vmo->lock()};
    EXPECT_EQ(vmo->size_locked(), (uint64_t)4 * PAGE_SIZE);
    // Should not have an allocated stream size.
    auto result = vmo->user_content_size_locked();
    EXPECT_FALSE(result.has_value());
  }

  // Give VMO a user-defined stream size of 2 pages.
  fbl::RefPtr<ContentSizeManager> csm;
  auto csm_result = ContentSizeManager::Create(2 * PAGE_SIZE);
  ASSERT_OK(csm_result.status_value());
  csm = ktl::move(*csm_result);

  vmo->SetUserContentSize(csm);

  {
    Guard<CriticalMutex> guard{vmo->lock()};
    auto result = vmo->user_content_size_locked();
    ASSERT_TRUE(result.has_value());
    const uint64_t stream_size = result.value();
    EXPECT_EQ(stream_size, (uint64_t)2 * PAGE_SIZE);

    // VMO size should be unchanged.
    EXPECT_EQ(vmo->size_locked(), (uint64_t)4 * PAGE_SIZE);
  }

  END_TEST;
}

UNITTEST_START_TESTCASE(vmo_tests)
VM_UNITTEST(vmo_create_test)
VM_UNITTEST(vmo_create_maximum_size)
VM_UNITTEST(vmo_pin_test)
VM_UNITTEST(vmo_pin_contiguous_test)
VM_UNITTEST(vmo_multiple_pin_test)
VM_UNITTEST(vmo_multiple_pin_contiguous_test)
VM_UNITTEST(vmo_commit_test)
VM_UNITTEST(vmo_commit_compressed_pages_test)
VM_UNITTEST(vmo_unaligned_size_test)
VM_UNITTEST(vmo_reference_attribution_commit_test)
VM_UNITTEST(vmo_create_physical_test)
VM_UNITTEST(vmo_physical_pin_test)
VM_UNITTEST(vmo_create_contiguous_test)
VM_UNITTEST(vmo_contiguous_decommit_test)
VM_UNITTEST(vmo_contiguous_decommit_disabled_test)
VM_UNITTEST(vmo_contiguous_decommit_enabled_test)
VM_UNITTEST(vmo_precommitted_map_test)
VM_UNITTEST(vmo_demand_paged_map_test)
VM_UNITTEST(vmo_dropped_ref_test)
VM_UNITTEST(vmo_remap_test)
VM_UNITTEST(vmo_double_remap_test)
VM_UNITTEST(vmo_read_write_smoke_test)
VM_UNITTEST(vmo_cache_test)
VM_UNITTEST(vmo_lookup_test)
VM_UNITTEST(vmo_lookup_slice_test)
VM_UNITTEST(vmo_lookup_clone_test)
VM_UNITTEST(vmo_clone_removes_write_test)
VM_UNITTEST(vmo_clones_of_compressed_pages_test)
VM_UNITTEST(vmo_clone_kernel_mapped_compressed_test)
VM_UNITTEST(vmo_move_pages_on_access_test)
VM_UNITTEST(vmo_eviction_hints_test)
VM_UNITTEST(vmo_always_need_evicts_loaned_test)
VM_UNITTEST(vmo_eviction_hints_clone_test)
VM_UNITTEST(vmo_eviction_test)
VM_UNITTEST(vmo_validate_page_shares_test)
VM_UNITTEST(vmo_attribution_clones_test)
VM_UNITTEST(vmo_attribution_ops_test)
VM_UNITTEST(vmo_attribution_ops_contiguous_test)
VM_UNITTEST(vmo_attribution_pager_test)
VM_UNITTEST(vmo_attribution_evict_test)
VM_UNITTEST(vmo_attribution_dedup_test)
VM_UNITTEST(vmo_attribution_compression_test)
VM_UNITTEST(vmo_parent_merge_test)
VM_UNITTEST(vmo_lock_count_test)
VM_UNITTEST(vmo_discardable_states_test)
VM_UNITTEST(vmo_discard_test)
VM_UNITTEST(vmo_discard_failure_test)
VM_UNITTEST(vmo_discardable_counts_test)
VM_UNITTEST(vmo_lookup_compressed_pages_test)
VM_UNITTEST(vmo_write_does_not_commit_test)
VM_UNITTEST(vmo_stack_owned_loaned_pages_interval_test)
VM_UNITTEST(vmo_dirty_pages_test)
VM_UNITTEST(vmo_dirty_pages_writeback_test)
VM_UNITTEST(vmo_dirty_pages_with_hints_test)
VM_UNITTEST(vmo_pinning_backlink_test)
VM_UNITTEST(vmo_supply_compressed_pages_test)
VM_UNITTEST(vmo_zero_pinned_test)
VM_UNITTEST(vmo_pinned_wrapper_test)
VM_UNITTEST(vmo_dedup_dirty_test)
VM_UNITTEST(vmo_high_priority_reclaim_test)
VM_UNITTEST(vmo_snapshot_modified_test)
VM_UNITTEST(vmo_pin_race_loaned_test)
VM_UNITTEST(vmo_prefetch_compressed_pages_test)
VM_UNITTEST(vmo_skip_range_update_test)
VM_UNITTEST(vmo_user_stream_size_test)
UNITTEST_END_TESTCASE(vmo_tests, "vmo", "VmObject tests")

}  // namespace vm_unittest
