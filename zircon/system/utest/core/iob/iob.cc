// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/errors.h>
#include <zircon/limits.h>
#include <zircon/rights.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls.h>
#include <zircon/system/public/zircon/process.h>
#include <zircon/system/public/zircon/syscalls/iob.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <cstdint>
#include <cstring>

#include <zxtest/zxtest.h>

const uint64_t kIoBufferEpRwMap = ZX_IOB_EP0_CAN_MAP_READ | ZX_IOB_EP0_CAN_MAP_WRITE |
                                  ZX_IOB_EP1_CAN_MAP_READ | ZX_IOB_EP1_CAN_MAP_WRITE;
const uint64_t kIoBufferEp0OnlyRwMap = ZX_IOB_EP0_CAN_MAP_READ | ZX_IOB_EP0_CAN_MAP_WRITE;
const uint64_t kIoBufferRdOnlyMap = ZX_IOB_EP0_CAN_MAP_READ | ZX_IOB_EP1_CAN_MAP_READ;

namespace {
TEST(Iob, Create) {
  zx_handle_t ep0, ep1;
  zx_iob_region_t config{
      .type = ZX_IOB_REGION_TYPE_PRIVATE,
      .access = kIoBufferEpRwMap,
      .size = ZX_PAGE_SIZE,
      .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
      .private_region =
          {
              .options = 0,
          },
  };
  EXPECT_OK(zx_iob_create(0, &config, 1, &ep0, &ep1));
  EXPECT_OK(zx_handle_close(ep0));
  EXPECT_OK(zx_handle_close(ep1));

  EXPECT_EQ(ZX_ERR_INVALID_ARGS, zx_iob_create(0, nullptr, 0, &ep0, &ep1));
  EXPECT_EQ(ZX_ERR_BAD_HANDLE, zx_handle_close(ep0));
  EXPECT_EQ(ZX_ERR_BAD_HANDLE, zx_handle_close(ep1));

  EXPECT_EQ(ZX_ERR_INVALID_ARGS, zx_iob_create(0, nullptr, 4, &ep0, &ep1));
  EXPECT_EQ(ZX_ERR_BAD_HANDLE, zx_handle_close(ep0));
  EXPECT_EQ(ZX_ERR_BAD_HANDLE, zx_handle_close(ep1));
}

TEST(Iob, CreateHuge) {
  zx_handle_t ep0, ep1;
  // Iobs will round up to the nearest page size. Make sure we don't overflow and wrap around.
  zx_iob_region_t config{
      .type = ZX_IOB_REGION_TYPE_PRIVATE,
      .access = kIoBufferEpRwMap,
      .size = 0xFFFF'FFFF'FFFF'FFFF,
      .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
      .private_region =
          {
              .options = 0,
          },
  };
  EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, _zx_iob_create(0, &config, 1, &ep0, &ep1));
}

TEST(Iob, BadVmOptions) {
  zx_handle_t ep0, ep1;
  zx_iob_region_t config{
      .type = ZX_IOB_REGION_TYPE_PRIVATE,
      .access = kIoBufferEpRwMap,
      .size = ZX_PAGE_SIZE,
      .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
      .private_region =
          {
              .options = 0xFFF,
          },
  };
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, _zx_iob_create(0, &config, 1, &ep0, &ep1));
}

TEST(Iob, PeerClosed) {
  zx_handle_t ep0, ep1;
  zx_iob_region_t config{
      .type = ZX_IOB_REGION_TYPE_PRIVATE,
      .access = kIoBufferEpRwMap,
      .size = ZX_PAGE_SIZE,
      .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
      .private_region =
          {
              .options = 0,
          },
  };
  EXPECT_OK(zx_iob_create(0, &config, 1, &ep0, &ep1));
  zx_signals_t observed;
  EXPECT_EQ(ZX_ERR_TIMED_OUT,
            zx_object_wait_one(ep0, ZX_IOB_PEER_CLOSED, ZX_TIME_INFINITE_PAST, &observed));
  EXPECT_EQ(0, observed);
  EXPECT_EQ(ZX_ERR_TIMED_OUT,
            zx_object_wait_one(ep1, ZX_IOB_PEER_CLOSED, ZX_TIME_INFINITE_PAST, &observed));
  EXPECT_EQ(0, observed);

  EXPECT_OK(zx_handle_close(ep0));
  EXPECT_OK(zx_object_wait_one(ep1, ZX_IOB_PEER_CLOSED, 0, &observed));
  EXPECT_EQ(ZX_IOB_PEER_CLOSED, observed);
  EXPECT_OK(zx_handle_close(ep1));

  EXPECT_OK(zx_iob_create(0, &config, 1, &ep0, &ep1));
  EXPECT_OK(zx_handle_close(ep1));
  EXPECT_OK(zx_object_wait_one(ep0, ZX_IOB_PEER_CLOSED, 0, &observed));
  EXPECT_EQ(ZX_IOB_PEER_CLOSED, observed);
  EXPECT_OK(zx_handle_close(ep0));
}

TEST(Iob, RegionMap) {
  zx_handle_t ep0, ep1;
  uintptr_t region1_addr = 0, region2_addr = 0;
  zx_iob_region_t config[1]{{
      .type = ZX_IOB_REGION_TYPE_PRIVATE,
      .access = kIoBufferEpRwMap,
      .size = ZX_PAGE_SIZE,
      .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
      .private_region =
          {
              .options = 0,
          },
  }};

  ASSERT_OK(zx_iob_create(0, config, 1, &ep0, &ep1));

  zx_handle_t vmar = zx_vmar_root_self();
  EXPECT_OK(zx_vmar_map_iob(vmar, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0, 0, 0, ZX_PAGE_SIZE,
                            &region1_addr));
  ASSERT_NE(0, region1_addr);

  // If we write data to the mapped memory of one handle, we should be able to read it from the
  // mapped memory of the other handle
  const char* test_str = "ABCDEFG";
  char* data1 = reinterpret_cast<char*>(region1_addr);
  memcpy(data1, test_str, 1 + strlen(test_str));
  EXPECT_STREQ(data1, test_str);

  EXPECT_OK(zx_vmar_map_iob(vmar, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep1, 0, 0, ZX_PAGE_SIZE,
                            &region2_addr));
  char* data2 = reinterpret_cast<char*>(region2_addr);
  EXPECT_STREQ(data2, test_str);
  EXPECT_OK(zx_handle_close(ep0));
  EXPECT_OK(zx_handle_close(ep1));
}

TEST(Iob, MappingRights) {
  zx_handle_t ep0, ep1;
  uintptr_t out_addr = 0;
  constexpr size_t noPermissionsIdx = 0;
  constexpr size_t onlyEp0Idx = 1;
  constexpr size_t rdOnlyIdx = 2;
  // There are 3 factors that go into the resulting permissions of a mapped region:
  // - The options the region it was created with
  // - The handle rights of the iorb we have
  // - The handle rights of the vmar we have
  // - The VmOptions that the map call requests
  zx_iob_region_t config[3]{
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = 0,
          .size = ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      },
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEp0OnlyRwMap,
          .size = ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      },
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferRdOnlyMap,
          .size = ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      }};

  // Let's create some regions with varying r/w/map options
  ASSERT_OK(zx_iob_create(0, config, 3, &ep0, &ep1));

  // If the iorb handle doesn't have the correct rights, we shouldn't be able to map it
  zx_handle_t no_write_ep_handle;
  zx_handle_t no_read_write_ep_handle;
  zx_handle_t vmar = zx_vmar_root_self();

  ASSERT_OK(zx_handle_duplicate(ep0, ZX_DEFAULT_IOB_RIGHTS & ~ZX_RIGHT_WRITE, &no_write_ep_handle));
  ASSERT_OK(zx_handle_duplicate(ep0, ZX_DEFAULT_IOB_RIGHTS & ~ZX_RIGHT_WRITE & ~ZX_RIGHT_READ,
                                &no_read_write_ep_handle));

  // We shouldn't be able to map a region that didn't set map permissions.
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, zx_vmar_map_iob(vmar, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0,
                                                  noPermissionsIdx, 0, ZX_PAGE_SIZE, &out_addr));
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, zx_vmar_map_iob(vmar, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep1,
                                                  noPermissionsIdx, 0, ZX_PAGE_SIZE, &out_addr));

  // And if a region is set to only be mappable by one endpoint, ensure it is.
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, zx_vmar_map_iob(vmar, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep1,
                                                  onlyEp0Idx, 0, 0, &out_addr));
  EXPECT_EQ(ZX_OK, zx_vmar_map_iob(vmar, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0, onlyEp0Idx, 0,
                                   ZX_PAGE_SIZE, &out_addr));

  // We shouldn't be able to request more rights than the region has
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, zx_vmar_map_iob(vmar, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0,
                                                  rdOnlyIdx, 0, 0, &out_addr));
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, zx_vmar_map_iob(vmar, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep1,
                                                  rdOnlyIdx, 0, 0, &out_addr));

  EXPECT_OK(zx_handle_close(ep0));
  EXPECT_OK(zx_handle_close(ep1));
  EXPECT_OK(zx_handle_close(no_write_ep_handle));
  EXPECT_OK(zx_handle_close(no_read_write_ep_handle));
}

TEST(Iob, PeerClosedMappedReferences) {
  // We shouldn't see peer closed until mappings created by an endpoint are also closed
  zx_handle_t ep0, ep1;
  zx_iob_region_t config{
      .type = ZX_IOB_REGION_TYPE_PRIVATE,
      .access = kIoBufferEpRwMap,
      .size = ZX_PAGE_SIZE,
      .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
      .private_region =
          {
              .options = 0,
          },
  };
  EXPECT_OK(zx_iob_create(0, &config, 1, &ep0, &ep1));
  zx_handle_t vmar = zx_vmar_root_self();
  zx_vaddr_t addr1;
  EXPECT_OK(zx_vmar_map_iob(vmar, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0, 0, 0, ZX_PAGE_SIZE,
                            &addr1));
  ASSERT_NE(0, addr1);
  zx_vaddr_t addr2;
  EXPECT_OK(zx_vmar_map_iob(vmar, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0, 0, 0, ZX_PAGE_SIZE,
                            &addr2));
  ASSERT_NE(0, addr2);

  zx_handle_close(ep0);
  // We shouldn't get peer closed on ep1 just yet
  zx_signals_t observed;
  EXPECT_EQ(ZX_ERR_TIMED_OUT, zx_object_wait_one(ep1, ZX_IOB_PEER_CLOSED, 0, &observed));
  EXPECT_EQ(0, observed);

  EXPECT_OK(zx_vmar_unmap(vmar, addr1, ZX_PAGE_SIZE));
  EXPECT_EQ(ZX_ERR_TIMED_OUT, zx_object_wait_one(ep1, ZX_IOB_PEER_CLOSED, 0, &observed));
  EXPECT_EQ(0, observed);
  EXPECT_OK(zx_vmar_unmap(vmar, addr2, ZX_PAGE_SIZE));

  // But now we should
  EXPECT_OK(zx_object_wait_one(ep1, ZX_IOB_PEER_CLOSED, 0, &observed));
  EXPECT_EQ(ZX_IOB_PEER_CLOSED, observed);
  EXPECT_OK(zx_handle_close(ep1));
}

}  // namespace
