// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/errors.h>
#include <zircon/limits.h>
#include <zircon/rights.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/object.h>
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

  zx_iob_region_t no_access_config{
      .type = ZX_IOB_REGION_TYPE_PRIVATE,
      .access = 0,
      .size = ZX_PAGE_SIZE,
      .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
      .private_region =
          {
              .options = 0,
          },
  };
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, zx_iob_create(0, &no_access_config, 1, &ep0, &ep1));
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
          .access = ZX_IOB_EP0_CAN_MEDIATED_READ,
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

TEST(Iob, GetInfoIob) {
  zx_handle_t ep0, ep1;
  zx_iob_region_t config[3]{
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEpRwMap,
          .size = ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      },
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEpRwMap,
          .size = 2 * ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      },
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEpRwMap,
          .size = 3 * ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      }};

  ASSERT_OK(zx_iob_create(0, config, 3, &ep0, &ep1));

  zx_iob_region_info_t info[5];
  size_t actual;
  size_t available;
  ASSERT_OK(zx_object_get_info(ep0, ZX_INFO_IOB_REGIONS, &info, sizeof(info), &actual, &available));
  EXPECT_EQ(actual, 3);
  EXPECT_EQ(available, 3);
  EXPECT_BYTES_EQ(&(info[0].region), &config[0], sizeof(zx_iob_region_t));
  EXPECT_BYTES_EQ(&(info[1].region), &config[1], sizeof(zx_iob_region_t));
  EXPECT_BYTES_EQ(&(info[2].region), &config[2], sizeof(zx_iob_region_t));

  ASSERT_OK(zx_object_get_info(ep0, ZX_INFO_IOB_REGIONS, nullptr, 0, &actual, &available));
  EXPECT_EQ(actual, 0);
  EXPECT_EQ(available, 3);

  zx_iob_region_info_t info2[2];
  ASSERT_OK(
      zx_object_get_info(ep0, ZX_INFO_IOB_REGIONS, &info2, sizeof(info2), &actual, &available));
  EXPECT_EQ(actual, 2);
  EXPECT_EQ(available, 3);
  EXPECT_BYTES_EQ(&(info[0].region), &config[0], sizeof(zx_iob_region_t));
  EXPECT_BYTES_EQ(&(info[1].region), &config[1], sizeof(zx_iob_region_t));

  EXPECT_OK(zx_handle_close(ep0));
  EXPECT_OK(zx_handle_close(ep1));
}

TEST(Iob, RegionInfoSwappedAccess) {
  zx_handle_t ep0, ep1;
  zx_iob_region_t config[3]{
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
  };

  ASSERT_OK(zx_iob_create(0, config, 1, &ep0, &ep1));

  zx_iob_region_info_t ep0_info[1];
  zx_iob_region_info_t ep1_info[1];
  ASSERT_OK(
      zx_object_get_info(ep0, ZX_INFO_IOB_REGIONS, ep0_info, sizeof(ep0_info), nullptr, nullptr));
  ASSERT_OK(
      zx_object_get_info(ep1, ZX_INFO_IOB_REGIONS, ep1_info, sizeof(ep1_info), nullptr, nullptr));

  // We should see the same underlying memory object
  EXPECT_EQ(ep0_info[0].koid, ep1_info[0].koid);

  // But our view of the access bits should be swapped
  EXPECT_EQ(ep0_info[0].region.access, ZX_IOB_EP0_CAN_MAP_READ | ZX_IOB_EP0_CAN_MAP_WRITE);
  // ep1 will see itself as ep0, and the other endpoint as ep1
  EXPECT_EQ(ep1_info[0].region.access, ZX_IOB_EP1_CAN_MAP_READ | ZX_IOB_EP1_CAN_MAP_WRITE);

  EXPECT_OK(zx_handle_close(ep0));
  EXPECT_OK(zx_handle_close(ep1));
}

TEST(Iob, GetInfoIobRegions) {
  zx_handle_t ep0, ep1;
  zx_iob_region_t config[3]{
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEpRwMap,
          .size = ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      },
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEpRwMap,
          .size = 2 * ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      },
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEpRwMap,
          .size = 3 * ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      }};

  ASSERT_OK(zx_iob_create(0, config, 3, &ep0, &ep1));

  zx_info_iob info;
  ASSERT_OK(zx_object_get_info(ep0, ZX_INFO_IOB, &info, sizeof(info), nullptr, nullptr));
  EXPECT_EQ(info.options, 0);
  EXPECT_EQ(info.region_count, 3);
  ASSERT_OK(zx_object_get_info(ep1, ZX_INFO_IOB, &info, sizeof(info), nullptr, nullptr));
  EXPECT_EQ(info.options, 0);
  EXPECT_EQ(info.region_count, 3);

  EXPECT_OK(zx_handle_close(ep0));
  EXPECT_OK(zx_handle_close(ep1));
}

TEST(Iob, RoundedSizes) {
  // Check that iobs round up their requested size to the nearest page
  zx_handle_t ep0, ep1;
  zx_iob_region_t config[3]{
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEpRwMap,
          .size = ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      },
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEpRwMap,
          .size = ZX_PAGE_SIZE + 1,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      },
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEpRwMap,
          .size = ZX_PAGE_SIZE - 1,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      }};

  ASSERT_OK(zx_iob_create(0, config, 3, &ep0, &ep1));

  zx_iob_region_info_t info[3];
  ASSERT_OK(zx_object_get_info(ep0, ZX_INFO_IOB_REGIONS, &info, sizeof(info), nullptr, nullptr));
  EXPECT_EQ(info[0].region.size, ZX_PAGE_SIZE);
  EXPECT_EQ(info[1].region.size, 2 * ZX_PAGE_SIZE);
  EXPECT_EQ(info[2].region.size, ZX_PAGE_SIZE);

  EXPECT_OK(zx_handle_close(ep0));
  EXPECT_OK(zx_handle_close(ep1));
}

TEST(Iob, GetSetNames) {
  zx_handle_t ep0, ep1;
  zx_iob_region_t config[3]{
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEpRwMap,
          .size = ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      },
  };
  ASSERT_OK(zx_iob_create(0, config, 1, &ep0, &ep1));

  // If we set the name from ep0,  we should see it from ep1
  const char* iob_name = "TestIob";
  EXPECT_OK(zx_object_set_property(ep0, ZX_PROP_NAME, iob_name, 8));

  char name_buffer[ZX_MAX_NAME_LEN];
  EXPECT_OK(zx_object_get_property(ep0, ZX_PROP_NAME, name_buffer, ZX_MAX_NAME_LEN));
  EXPECT_STREQ(name_buffer, iob_name);
  EXPECT_OK(zx_object_get_property(ep1, ZX_PROP_NAME, name_buffer, ZX_MAX_NAME_LEN));
  EXPECT_STREQ(name_buffer, iob_name);

  const char* iob_name2 = "TestIob2";
  EXPECT_OK(zx_object_set_property(ep1, ZX_PROP_NAME, iob_name2, 9));
  EXPECT_OK(zx_object_get_property(ep0, ZX_PROP_NAME, name_buffer, ZX_MAX_NAME_LEN));
  EXPECT_STREQ(name_buffer, iob_name2);
  EXPECT_OK(zx_object_get_property(ep1, ZX_PROP_NAME, name_buffer, ZX_MAX_NAME_LEN));
  EXPECT_STREQ(name_buffer, iob_name2);

  // The Underlying vmos should also have their name set
  size_t avail;
  ASSERT_OK(
      zx_object_get_info(zx_process_self(), ZX_INFO_PROCESS_VMOS, nullptr, 0, nullptr, &avail));

  zx_info_vmo_t vmo_infos[avail];
  ASSERT_OK(zx_object_get_info(zx_process_self(), ZX_INFO_PROCESS_VMOS, vmo_infos,
                               sizeof(zx_info_vmo_t) * avail, nullptr, nullptr));

  bool found_vmo = false;
  for (zx_info_vmo_t vmo_info : vmo_infos) {
    if (0 == strcmp("TestIob2", vmo_info.name)) {
      EXPECT_TRUE(vmo_info.flags & ZX_INFO_VMO_VIA_IOB_HANDLE);
      found_vmo = true;
      break;
    }
  }
  EXPECT_TRUE(found_vmo);
}

/// Iob regions count towards a process's vmo allocations.
TEST(Iob, GetInfoProcessVmos) {
  zx_handle_t ep0, ep1;
  zx_iob_region_t config[3]{
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
          .access = kIoBufferEp0OnlyRwMap,
          .size = 2 * ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      },
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEp0OnlyRwMap,
          .size = 3 * ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region =
              {
                  .options = 0,
              },
      }};

  size_t before_vmo_count;
  ASSERT_OK(zx_object_get_info(zx_process_self(), ZX_INFO_PROCESS_VMOS, nullptr, 0, nullptr,
                               &before_vmo_count));
  ASSERT_OK(zx_iob_create(0, config, 3, &ep0, &ep1));
  size_t after_vmo_count;
  ASSERT_OK(zx_object_get_info(zx_process_self(), ZX_INFO_PROCESS_VMOS, nullptr, 0, nullptr,
                               &after_vmo_count));

  zx_object_set_property(ep0, ZX_PROP_NAME, "TestIob", 8);

  // The vmos are counted twice: once for each handle
  EXPECT_EQ(before_vmo_count + 6, after_vmo_count);

  zx_info_vmo_t vmo_infos[after_vmo_count];
  ASSERT_OK(zx_object_get_info(zx_process_self(), ZX_INFO_PROCESS_VMOS, vmo_infos,
                               sizeof(zx_info_vmo_t) * after_vmo_count, nullptr, nullptr));

  bool saw_ep0_1_page = false;
  bool saw_ep0_2_page = false;
  bool saw_ep0_3_page = false;
  bool saw_ep1_1_page = false;
  bool saw_ep1_2_page = false;
  bool saw_ep1_3_page = false;

  for (zx_info_vmo_t vmo_info : vmo_infos) {
    if (0 == strcmp("TestIob", vmo_info.name)) {
      switch (vmo_info.size_bytes) {
        case ZX_PAGE_SIZE * 1:
          if ((vmo_info.handle_rights & ZX_RIGHT_READ) == 0) {
            // This is ep1 and we shouldn't have WRITE rights either
            EXPECT_FALSE(vmo_info.handle_rights & ZX_RIGHT_WRITE);
            EXPECT_FALSE(saw_ep1_1_page);
            saw_ep1_1_page = true;
          } else {
            // This is ep0 and we should have write rights
            EXPECT_EQ(ZX_RIGHT_WRITE, vmo_info.handle_rights & ZX_RIGHT_WRITE);
            EXPECT_FALSE(saw_ep0_1_page);
            saw_ep0_1_page = true;
          }
          break;
        case ZX_PAGE_SIZE * 2:
          if ((vmo_info.handle_rights & ZX_RIGHT_READ) == 0) {
            EXPECT_FALSE(vmo_info.handle_rights & ZX_RIGHT_WRITE);
            EXPECT_FALSE(saw_ep1_2_page);
            saw_ep1_2_page = true;
          } else {
            EXPECT_EQ(ZX_RIGHT_WRITE, vmo_info.handle_rights & ZX_RIGHT_WRITE);
            EXPECT_FALSE(saw_ep0_2_page);
            saw_ep0_2_page = true;
          }
          break;
        case ZX_PAGE_SIZE * 3:
          if ((vmo_info.handle_rights & ZX_RIGHT_READ) == 0) {
            EXPECT_FALSE(vmo_info.handle_rights & ZX_RIGHT_WRITE);
            EXPECT_FALSE(saw_ep1_3_page);
            saw_ep1_3_page = true;
          } else {
            EXPECT_EQ(ZX_RIGHT_WRITE, vmo_info.handle_rights & ZX_RIGHT_WRITE);
            EXPECT_FALSE(saw_ep0_3_page);
            saw_ep0_3_page = true;
          }
          break;
      }
    }
  }
  EXPECT_TRUE(saw_ep0_1_page);
  EXPECT_TRUE(saw_ep0_2_page);
  EXPECT_TRUE(saw_ep0_3_page);
  EXPECT_TRUE(saw_ep1_1_page);
  EXPECT_TRUE(saw_ep1_2_page);
  EXPECT_TRUE(saw_ep1_3_page);

  EXPECT_OK(zx_handle_close(ep0));
  size_t after_close_one;
  ASSERT_OK(zx_object_get_info(zx_process_self(), ZX_INFO_PROCESS_VMOS, nullptr, 0, nullptr,
                               &after_close_one));
  EXPECT_EQ(before_vmo_count + 3, after_close_one);

  EXPECT_OK(zx_handle_close(ep1));
  size_t after_close_two;
  ASSERT_OK(zx_object_get_info(zx_process_self(), ZX_INFO_PROCESS_VMOS, nullptr, 0, nullptr,
                               &after_close_two));
  EXPECT_EQ(before_vmo_count, after_close_two);
}

TEST(Iob, GetInfoProcessMaps) {
  zx_handle_t ep0, ep1;
  zx_iob_region_t config[3]{{
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
  size_t num_mappings_before;
  ASSERT_OK(zx_object_get_info(zx_process_self(), ZX_INFO_PROCESS_MAPS, nullptr, 0, nullptr,
                               &num_mappings_before));
  uint64_t region_addr;
  EXPECT_OK(zx_vmar_map_iob(vmar, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0, 0, 0, ZX_PAGE_SIZE,
                            &region_addr));
  ASSERT_NE(0, region_addr);

  size_t num_mappings_after;
  ASSERT_OK(zx_object_get_info(zx_process_self(), ZX_INFO_PROCESS_MAPS, nullptr, 0, nullptr,
                               &num_mappings_after));

  EXPECT_EQ(num_mappings_after, num_mappings_before + 1);

  zx_info_maps_t map_infos[num_mappings_after];
  ASSERT_OK(zx_object_get_info(zx_process_self(), ZX_INFO_PROCESS_MAPS, map_infos,
                               sizeof(zx_info_vmo_t) * num_mappings_after, nullptr, nullptr));

  zx_iob_region_info_t ep0_info[1];
  ASSERT_OK(
      zx_object_get_info(ep0, ZX_INFO_IOB_REGIONS, ep0_info, sizeof(ep0_info), nullptr, nullptr));

  zx_koid_t iob_koid = ep0_info[0].koid;
  bool saw_iob_mapping = false;
  for (auto mapping : map_infos) {
    if (mapping.u.mapping.vmo_koid == iob_koid) {
      EXPECT_EQ(mapping.size, ZX_PAGE_SIZE);
      saw_iob_mapping = true;
      break;
    }
  }
  EXPECT_TRUE(saw_iob_mapping);
}

}  // namespace
