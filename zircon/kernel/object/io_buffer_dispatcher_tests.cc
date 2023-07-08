// Copyright 2019 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/unittest/unittest.h>
#include <zircon/errors.h>
#include <zircon/limits.h>
#include <zircon/rights.h>
#include <zircon/syscalls/iob.h>

#include <fbl/alloc_checker.h>
#include <kernel/attribution.h>
#include <object/handle.h>
#include <object/io_buffer_dispatcher.h>

namespace {

// Allocate/destroy many iobuffers. Ad hoc resource leak check.
bool TestCreateDestroyManyIoBuffers() {
  BEGIN_TEST;
  static constexpr int kMany = 10000;

  fbl::AllocChecker ac;

  for (unsigned region_count : {1, 7, 64}) {
    for (unsigned i = 0; i < kMany; i++) {
      KernelHandle<IoBufferDispatcher> dispatcher0, dispatcher1;
      zx_rights_t rights;
      IoBufferDispatcher::RegionArray regions{&ac, region_count};
      ASSERT_TRUE(ac.check());
      for (unsigned idx = 0; idx < region_count; idx++) {
        regions[idx] =
            zx_iob_region_t{.type = ZX_IOB_REGION_TYPE_PRIVATE,
                            .access = ZX_IOB_EP0_CAN_MAP_READ | ZX_IOB_EP0_CAN_MAP_WRITE,
                            .size = ZX_PAGE_SIZE,
                            .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
                            .private_region = {
                                .options = 0,
                            }};
      }
      auto status =
          IoBufferDispatcher::Create(0, regions, AttributionObject::GetKernelAttribution(),
                                     &dispatcher0, &dispatcher1, &rights);
      ASSERT_EQ(status, ZX_OK);
    }
  }

  END_TEST;
}
}  // namespace

UNITTEST_START_TESTCASE(iobuffer_dispatcher_tests)
UNITTEST("TestCreateDestroyManyIoBuffers", TestCreateDestroyManyIoBuffers)
UNITTEST_END_TESTCASE(iobuffer_dispatcher_tests, "iobuffer_dispatcher_tests",
                      "IoBufferDispatcher tests")
