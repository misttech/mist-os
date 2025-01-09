// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/iob/blob-id-allocator.h>
#include <lib/zx/iob.h>
#include <lib/zx/process.h>
#include <lib/zx/result.h>
#include <lib/zx/vmar.h>
#include <zircon/errors.h>
#include <zircon/limits.h>
#include <zircon/process.h>
#include <zircon/rights.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/iob.h>
#include <zircon/syscalls/object.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <cstdint>
#include <cstring>

#include <zxtest/zxtest.h>

#include "../needs-next.h"

const uint64_t kIoBufferEpRwMap = ZX_IOB_ACCESS_EP0_CAN_MAP_READ | ZX_IOB_ACCESS_EP0_CAN_MAP_WRITE |
                                  ZX_IOB_ACCESS_EP1_CAN_MAP_READ | ZX_IOB_ACCESS_EP1_CAN_MAP_WRITE;
const uint64_t kIoBufferEp0OnlyRwMap =
    ZX_IOB_ACCESS_EP0_CAN_MAP_READ | ZX_IOB_ACCESS_EP0_CAN_MAP_WRITE;
const uint64_t kIoBufferRdOnlyMap = ZX_IOB_ACCESS_EP0_CAN_MAP_READ | ZX_IOB_ACCESS_EP1_CAN_MAP_READ;

NEEDS_NEXT_SYSCALL(zx_iob_allocate_id);

namespace {
// An RAII Helper used to make sure that we don't accidentally leak any mapped
// IOBs during testing.
class MappingHelper {
 public:
  ~MappingHelper() { Unmap(); }

  MappingHelper(const MappingHelper&) = delete;
  MappingHelper& operator=(const MappingHelper&) = delete;

  MappingHelper& operator=(MappingHelper&& other) {
    this->Unmap();
    std::swap(addr_, other.addr_);
    std::swap(region_len_, other.region_len_);
    return *this;
  }
  MappingHelper(MappingHelper&& other) { *this = std::move(other); }

  static zx::result<MappingHelper> Create(zx_vm_option_t options, size_t vmar_offset,
                                          const zx::iob& iob_handle, uint32_t region_index,
                                          uint64_t region_offset, size_t region_len) {
    zx_vaddr_t addr{0};
    zx_status_t res = zx::vmar::root_self()->map_iob(options, vmar_offset, iob_handle, region_index,
                                                     region_offset, region_len, &addr);
    if (res != ZX_OK) {
      return zx::error(res);
    }

    return zx::ok(MappingHelper{addr, region_len});
  }

  zx_status_t Unmap() {
    if (addr_ != 0) {
      zx_status_t res = zx::vmar::root_self()->unmap(addr_, region_len_);
      addr_ = 0;
      region_len_ = 0;
      return res;
    }
    return ZX_ERR_BAD_STATE;
  }

  zx_vaddr_t addr() const { return addr_; }
  size_t region_len() const { return region_len_; }

 private:
  MappingHelper(zx_vaddr_t addr, size_t region_len) : addr_(addr), region_len_(region_len) {}

  zx_vaddr_t addr_{0};
  size_t region_len_{0};
};

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
  zx::iob ep0, ep1;
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
  EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, zx::iob::create(0, &config, 1, &ep0, &ep1));
}

TEST(Iob, BadVmOptions) {
  zx::iob ep0, ep1;
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
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, zx::iob::create(0, &config, 1, &ep0, &ep1));
}

TEST(Iob, PeerClosed) {
  zx::iob ep0, ep1;
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
  EXPECT_OK(zx::iob::create(0, &config, 1, &ep0, &ep1));
  zx_signals_t observed;
  EXPECT_EQ(ZX_ERR_TIMED_OUT,
            ep0.wait_one(ZX_IOB_PEER_CLOSED, zx::time::infinite_past(), &observed));
  EXPECT_EQ(0, observed);
  EXPECT_EQ(ZX_ERR_TIMED_OUT,
            ep1.wait_one(ZX_IOB_PEER_CLOSED, zx::time::infinite_past(), &observed));
  EXPECT_EQ(0, observed);

  ep0.reset();
  EXPECT_OK(ep1.wait_one(ZX_IOB_PEER_CLOSED, zx::time{0}, &observed));
  EXPECT_EQ(ZX_IOB_PEER_CLOSED, observed);
  ep1.reset();

  EXPECT_OK(zx::iob::create(0, &config, 1, &ep0, &ep1));
  ep1.reset();
  EXPECT_OK(ep0.wait_one(ZX_IOB_PEER_CLOSED, zx::time{0}, &observed));
  EXPECT_EQ(ZX_IOB_PEER_CLOSED, observed);
  ep0.reset();
}

TEST(Iob, RegionMap) {
  zx::iob ep0, ep1;
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

  ASSERT_OK(zx::iob::create(0, config, 1, &ep0, &ep1));

  zx::result<MappingHelper> region1 =
      MappingHelper::Create(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0, 0, 0, ZX_PAGE_SIZE);
  ASSERT_OK(region1.status_value());
  ASSERT_NE(0, region1->addr());

  // If we write data to the mapped memory of one handle, we should be able to read it from the
  // mapped memory of the other handle
  const char* test_str = "ABCDEFG";
  char* data1 = reinterpret_cast<char*>(region1->addr());
  memcpy(data1, test_str, 1 + strlen(test_str));
  EXPECT_STREQ(data1, test_str);

  zx::result<MappingHelper> region2 =
      MappingHelper::Create(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep1, 0, 0, ZX_PAGE_SIZE);
  ASSERT_OK(region2.status_value());
  char* data2 = reinterpret_cast<char*>(region2->addr());
  EXPECT_STREQ(data2, test_str);
}

TEST(Iob, MappingRights) {
  zx::iob ep0, ep1;
  constexpr size_t noPermissionsIdx = 0;
  constexpr size_t onlyEp0Idx = 1;
  constexpr size_t rdOnlyIdx = 2;
  // There are 4 factors that go into the resulting permissions of a mapped region:
  // - The options the region it was created with
  // - The handle rights of the iob we have
  // - The handle rights of the vmar we have
  // - The VmOptions that the map call requests
  zx_iob_region_t config[3]{
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = ZX_IOB_ACCESS_EP0_CAN_MEDIATED_WRITE,
          .size = ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_ID_ALLOCATOR},
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
          .access = kIoBufferRdOnlyMap | ZX_IOB_ACCESS_EP0_CAN_MEDIATED_WRITE,
          .size = ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_ID_ALLOCATOR},
          .private_region =
              {
                  .options = 0,
              },
      }};

  // Let's create some regions with varying r/w/map options
  ASSERT_OK(zx::iob::create(0, config, 3, &ep0, &ep1));

  // If the iorb handle doesn't have the correct rights, we shouldn't be able to map it
  zx::iob no_write_ep_handle;
  zx::iob no_read_write_ep_handle;
  zx::unowned_vmar vmar = zx::vmar::root_self();

  ASSERT_OK(ep0.duplicate(ZX_DEFAULT_IOB_RIGHTS & ~ZX_RIGHT_WRITE, &no_write_ep_handle));
  ASSERT_OK(ep0.duplicate(ZX_DEFAULT_IOB_RIGHTS & ~ZX_RIGHT_WRITE & ~ZX_RIGHT_READ,
                          &no_read_write_ep_handle));

  // We shouldn't be able to map a region that didn't set map permissions.
  zx::result<MappingHelper> map1 = MappingHelper::Create(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0,
                                                         noPermissionsIdx, 0, ZX_PAGE_SIZE);
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, map1.status_value());

  zx::result<MappingHelper> map2 = MappingHelper::Create(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep1,
                                                         noPermissionsIdx, 0, ZX_PAGE_SIZE);
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, map2.status_value());

  // And if a region is set to only be mappable by one endpoint, ensure it is.
  zx::result<MappingHelper> map3 =
      MappingHelper::Create(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep1, onlyEp0Idx, 0, 0);
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, map3.status_value());

  zx::result<MappingHelper> map4 = MappingHelper::Create(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0,
                                                         onlyEp0Idx, 0, ZX_PAGE_SIZE);
  EXPECT_EQ(ZX_OK, map4.status_value());

  // We shouldn't be able to request more rights than the region has
  zx::result<MappingHelper> map5 =
      MappingHelper::Create(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0, rdOnlyIdx, 0, 0);
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, map5.status_value());

  zx::result<MappingHelper> map6 =
      MappingHelper::Create(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep1, rdOnlyIdx, 0, 0);
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, map6.status_value());
}

TEST(Iob, PeerClosedMappedReferences) {
  // We shouldn't see peer closed until mappings created by an endpoint are also closed
  zx::iob ep0, ep1;
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
  EXPECT_OK(zx::iob::create(0, &config, 1, &ep0, &ep1));
  zx::unowned_vmar vmar = zx::vmar::root_self();

  zx::result<MappingHelper> region1 =
      MappingHelper::Create(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0, 0, 0, ZX_PAGE_SIZE);
  ASSERT_OK(region1.status_value());
  ASSERT_NE(0, region1->addr());

  zx::result<MappingHelper> region2 =
      MappingHelper::Create(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0, 0, 0, ZX_PAGE_SIZE);
  ASSERT_OK(region2.status_value());
  ASSERT_NE(0, region2->addr());

  ep0.reset();
  // We shouldn't get peer closed on ep1 just yet
  zx_signals_t observed;
  EXPECT_EQ(ZX_ERR_TIMED_OUT, ep1.wait_one(ZX_IOB_PEER_CLOSED, zx::time{0}, &observed));
  EXPECT_EQ(0, observed);

  EXPECT_OK(region1->Unmap());
  EXPECT_EQ(ZX_ERR_TIMED_OUT, ep1.wait_one(ZX_IOB_PEER_CLOSED, zx::time{0}, &observed));
  EXPECT_EQ(0, observed);
  EXPECT_OK(region2->Unmap());

  // But now we should
  EXPECT_OK(ep1.wait_one(ZX_IOB_PEER_CLOSED, zx::time{0}, &observed));
  EXPECT_EQ(ZX_IOB_PEER_CLOSED, observed);
  ep1.reset();
}

TEST(Iob, GetInfoIob) {
  zx::iob ep0, ep1;
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

  ASSERT_OK(zx::iob::create(0, config, 3, &ep0, &ep1));

  zx_iob_region_info_t info[5];
  size_t actual;
  size_t available;
  ASSERT_OK(ep0.get_info(ZX_INFO_IOB_REGIONS, &info, sizeof(info), &actual, &available));
  EXPECT_EQ(actual, 3);
  EXPECT_EQ(available, 3);
  EXPECT_BYTES_EQ(&(info[0].region), &config[0], sizeof(zx_iob_region_t));
  EXPECT_BYTES_EQ(&(info[1].region), &config[1], sizeof(zx_iob_region_t));
  EXPECT_BYTES_EQ(&(info[2].region), &config[2], sizeof(zx_iob_region_t));

  ASSERT_OK(ep0.get_info(ZX_INFO_IOB_REGIONS, nullptr, 0, &actual, &available));
  EXPECT_EQ(actual, 0);
  EXPECT_EQ(available, 3);

  zx_iob_region_info_t info2[2];
  ASSERT_OK(ep0.get_info(ZX_INFO_IOB_REGIONS, &info2, sizeof(info2), &actual, &available));
  EXPECT_EQ(actual, 2);
  EXPECT_EQ(available, 3);
  EXPECT_BYTES_EQ(&(info[0].region), &config[0], sizeof(zx_iob_region_t));
  EXPECT_BYTES_EQ(&(info[1].region), &config[1], sizeof(zx_iob_region_t));
}

TEST(Iob, RegionInfoSwappedAccess) {
  zx::iob ep0, ep1;
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

  ASSERT_OK(zx::iob::create(0, config, 1, &ep0, &ep1));

  zx_iob_region_info_t ep0_info[1];
  zx_iob_region_info_t ep1_info[1];
  ASSERT_OK(ep0.get_info(ZX_INFO_IOB_REGIONS, ep0_info, sizeof(ep0_info), nullptr, nullptr));
  ASSERT_OK(ep1.get_info(ZX_INFO_IOB_REGIONS, ep1_info, sizeof(ep1_info), nullptr, nullptr));

  // We should see the same underlying memory object
  EXPECT_EQ(ep0_info[0].koid, ep1_info[0].koid);

  // But our view of the access bits should be swapped
  EXPECT_EQ(ep0_info[0].region.access,
            ZX_IOB_ACCESS_EP0_CAN_MAP_READ | ZX_IOB_ACCESS_EP0_CAN_MAP_WRITE);
  // ep1 will see itself as ep0, and the other endpoint as ep1
  EXPECT_EQ(ep1_info[0].region.access,
            ZX_IOB_ACCESS_EP1_CAN_MAP_READ | ZX_IOB_ACCESS_EP1_CAN_MAP_WRITE);
}

TEST(Iob, GetInfoIobRegions) {
  zx::iob ep0, ep1;
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

  ASSERT_OK(zx::iob::create(0, config, 3, &ep0, &ep1));

  zx_info_iob info;
  ASSERT_OK(ep0.get_info(ZX_INFO_IOB, &info, sizeof(info), nullptr, nullptr));
  EXPECT_EQ(info.options, 0);
  EXPECT_EQ(info.region_count, 3);
  ASSERT_OK(ep1.get_info(ZX_INFO_IOB, &info, sizeof(info), nullptr, nullptr));
  EXPECT_EQ(info.options, 0);
  EXPECT_EQ(info.region_count, 3);
}

TEST(Iob, RoundedSizes) {
  // Check that iobs round up their requested size to the nearest page
  zx::iob ep0, ep1;
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

  ASSERT_OK(zx::iob::create(0, config, 3, &ep0, &ep1));

  zx_iob_region_info_t info[3];
  ASSERT_OK(ep0.get_info(ZX_INFO_IOB_REGIONS, &info, sizeof(info), nullptr, nullptr));
  EXPECT_EQ(info[0].region.size, ZX_PAGE_SIZE);
  EXPECT_EQ(info[1].region.size, 2 * ZX_PAGE_SIZE);
  EXPECT_EQ(info[2].region.size, ZX_PAGE_SIZE);
}

TEST(Iob, GetSetNames) {
  zx::iob ep0, ep1;
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
  ASSERT_OK(zx::iob::create(0, config, 1, &ep0, &ep1));

  // If we set the name from ep0,  we should see it from ep1
  const char* iob_name = "TestIob";
  EXPECT_OK(ep0.set_property(ZX_PROP_NAME, iob_name, 8));

  char name_buffer[ZX_MAX_NAME_LEN];
  EXPECT_OK(ep0.get_property(ZX_PROP_NAME, name_buffer, ZX_MAX_NAME_LEN));
  EXPECT_STREQ(name_buffer, iob_name);
  EXPECT_OK(ep1.get_property(ZX_PROP_NAME, name_buffer, ZX_MAX_NAME_LEN));
  EXPECT_STREQ(name_buffer, iob_name);

  const char* iob_name2 = "TestIob2";
  EXPECT_OK(ep1.set_property(ZX_PROP_NAME, iob_name2, 9));
  EXPECT_OK(ep0.get_property(ZX_PROP_NAME, name_buffer, ZX_MAX_NAME_LEN));
  EXPECT_STREQ(name_buffer, iob_name2);
  EXPECT_OK(ep1.get_property(ZX_PROP_NAME, name_buffer, ZX_MAX_NAME_LEN));
  EXPECT_STREQ(name_buffer, iob_name2);

  // The Underlying vmos should also have their name set
  size_t avail;
  ASSERT_OK(
      zx_object_get_info(zx_process_self(), ZX_INFO_PROCESS_VMOS, nullptr, 0, nullptr, &avail));

  auto vmo_infos = std::make_unique<zx_info_vmo_t[]>(avail);
  ASSERT_OK(zx_object_get_info(zx_process_self(), ZX_INFO_PROCESS_VMOS, vmo_infos.get(),
                               sizeof(zx_info_vmo_t) * avail, nullptr, nullptr));

  bool found_vmo = false;
  for (size_t i = 0; i < avail; ++i) {
    zx_info_vmo_t& vmo_info = vmo_infos[i];
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
  zx::iob ep0, ep1;
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

  // Create the IOB and set the name we will use to identify the VMOs it creates.
  ASSERT_OK(zx::iob::create(0, config, 3, &ep0, &ep1));
  ep0.set_property(ZX_PROP_NAME, "TestIob", 8);

  // Introduce a helper lambda used to find the VMOs which should have been created when we created
  // our IOB.  We will use this to verify all of the expected VMOs were created, and that the proper
  // ones go away when we close each endpoint.
  bool saw_ep0_1_page, saw_ep0_2_page, saw_ep0_3_page;
  bool saw_ep1_1_page, saw_ep1_2_page, saw_ep1_3_page;
  auto FindTestVmos = [&]() -> void {
    saw_ep0_1_page = false;
    saw_ep0_2_page = false;
    saw_ep0_3_page = false;
    saw_ep1_1_page = false;
    saw_ep1_2_page = false;
    saw_ep1_3_page = false;

    size_t vmo_count{0};
    std::unique_ptr<zx_info_vmo_t[]> vmo_infos;
    ASSERT_OK(zx_object_get_info(zx_process_self(), ZX_INFO_PROCESS_VMOS, nullptr, 0, nullptr,
                                 &vmo_count));
    if (vmo_count) {
      vmo_infos = std::make_unique<zx_info_vmo_t[]>(vmo_count);
      ASSERT_OK(zx_object_get_info(zx_process_self(), ZX_INFO_PROCESS_VMOS, vmo_infos.get(),
                                   sizeof(zx_info_vmo_t) * vmo_count, nullptr, nullptr));

      for (size_t i = 0; i < vmo_count; ++i) {
        zx_info_vmo_t& vmo_info = vmo_infos[i];
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
    }
  };

  // Enumerate all of the VMOs in this process and find our test VMOs.  We have not closed any
  // endpoints yet, should be able to find all of them.
  FindTestVmos();
  EXPECT_TRUE(saw_ep0_1_page);
  EXPECT_TRUE(saw_ep0_2_page);
  EXPECT_TRUE(saw_ep0_3_page);
  EXPECT_TRUE(saw_ep1_1_page);
  EXPECT_TRUE(saw_ep1_2_page);
  EXPECT_TRUE(saw_ep1_3_page);

  // Now close Ep0.  We have not mapped any of these VMOs, so we expect all of the Ep0 VMOs to
  // disappear, while the Ep1 VMOs remain.
  ep0.reset();
  FindTestVmos();
  EXPECT_FALSE(saw_ep0_1_page);
  EXPECT_FALSE(saw_ep0_2_page);
  EXPECT_FALSE(saw_ep0_3_page);
  EXPECT_TRUE(saw_ep1_1_page);
  EXPECT_TRUE(saw_ep1_2_page);
  EXPECT_TRUE(saw_ep1_3_page);

  // Now close Ep1 as well.  All of our test VMOs should be gone after this.
  ep1.reset();
  FindTestVmos();
  EXPECT_FALSE(saw_ep0_1_page);
  EXPECT_FALSE(saw_ep0_2_page);
  EXPECT_FALSE(saw_ep0_3_page);
  EXPECT_FALSE(saw_ep1_1_page);
  EXPECT_FALSE(saw_ep1_2_page);
  EXPECT_FALSE(saw_ep1_3_page);
}

TEST(Iob, GetInfoProcessMaps) {
  zx::iob ep0, ep1;
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

  ASSERT_OK(zx::iob::create(0, config, 1, &ep0, &ep1));
  zx::unowned_vmar vmar = zx::vmar::root_self();
  size_t num_mappings_before;
  ASSERT_OK(zx::process::self()->get_info(ZX_INFO_PROCESS_MAPS, nullptr, 0, nullptr,
                                          &num_mappings_before));
  zx::result<MappingHelper> region =
      MappingHelper::Create(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0, 0, 0, ZX_PAGE_SIZE);
  ASSERT_OK(region.status_value());
  ASSERT_NE(0, region->addr());

  size_t num_mappings_after;
  ASSERT_OK(zx::process::self()->get_info(ZX_INFO_PROCESS_MAPS, nullptr, 0, nullptr,
                                          &num_mappings_after));

  EXPECT_EQ(num_mappings_after, num_mappings_before + 1);

  // Allocate an array with double the capacity of mappings we saw. This should ensure that if our
  // allocation itself triggers a mapping or two, there should still be plenty of space available.
  auto map_infos = std::make_unique<zx_info_maps_t[]>(num_mappings_after * 2);
  size_t num_map_infos = 0;
  ASSERT_OK(zx::process::self()->get_info(ZX_INFO_PROCESS_MAPS, map_infos.get(),
                                          sizeof(zx_info_vmo_t) * num_mappings_after * 2, nullptr,
                                          &num_map_infos));
  // Validate that we were able to fit all the mappings to ensure we
  //   1. cannot have missed our target IOB mapping
  //   2. the loop below will not do an array overrun.
  ASSERT_LE(num_map_infos, num_mappings_after * 2);

  zx_iob_region_info_t ep0_info[1];
  ASSERT_OK(ep0.get_info(ZX_INFO_IOB_REGIONS, ep0_info, sizeof(ep0_info), nullptr, nullptr));

  zx_koid_t iob_koid = ep0_info[0].koid;
  bool saw_iob_mapping = false;
  for (size_t i = 0; i < num_map_infos; ++i) {
    zx_info_maps_t& mapping = map_infos[i];
    if (mapping.u.mapping.vmo_koid == iob_koid) {
      EXPECT_EQ(mapping.size, ZX_PAGE_SIZE);
      saw_iob_mapping = true;
      break;
    }
  }
  EXPECT_TRUE(saw_iob_mapping);
}

TEST(Iob, IdAllocatorMediatedAccess) {
  NEEDS_NEXT_SKIP(zx_iob_allocate_id);

  constexpr std::array<std::byte, 10> kBlob{std::byte{'a'}};
  constexpr zx_iob_allocate_id_options_t kOptions = 0;
  constexpr uint32_t kIdAllocatorIdx = 0;

  // Endpoint 0 will be mapped, while endpoint 1 will facilitate mediated
  // allocations.
  zx::iob ep0, ep1;
  zx_iob_region_t config[]{
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEp0OnlyRwMap | ZX_IOB_ACCESS_EP1_CAN_MEDIATED_WRITE,
          .size = ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_ID_ALLOCATOR},
          .private_region = {.options = 0},
      },
  };

  ASSERT_OK(zx::iob::create(0, config, std::size(config), &ep0, &ep1));

  zx::result<MappingHelper> mapped_region =
      MappingHelper::Create(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0, 0, 0, ZX_PAGE_SIZE);
  EXPECT_OK(mapped_region.status_value());
  ASSERT_NE(mapped_region->addr(), 0u);
  cpp20::span<std::byte> bytes{reinterpret_cast<std::byte*>(mapped_region->addr()), ZX_PAGE_SIZE};

  // Since mediated access is enabled, the region should already be initialized.
  iob::BlobIdAllocator allocator(bytes);

  // Check that mediated allocation works.
  for (uint64_t expected_id = 0; expected_id < 100; ++expected_id) {
    EXPECT_EQ(expected_id, allocator.next_id());

    uint32_t id;
    if (expected_id % 2 == 0) {
      auto result = allocator.Allocate(kBlob);
      ASSERT_TRUE(result.is_ok());
      id = result.value();
    } else {
      ASSERT_OK(zx_iob_allocate_id(ep1.get(), kOptions, kIdAllocatorIdx, kBlob.data(), kBlob.size(),
                                   &id));
    }
    EXPECT_EQ(expected_id, id);
  }
  EXPECT_EQ(100, allocator.next_id());
}

TEST(Iob, IdAllocatorMediatedErrors) {
  NEEDS_NEXT_SKIP(zx_iob_allocate_id);

  constexpr std::array<std::byte, 10> kBlob{std::byte{'a'}};
  constexpr zx_iob_allocate_id_options_t kOptions = 0;
  constexpr uint32_t kIdAllocatorIdx = 0;

  // Endpoint 0 will be mapped, while endpoint 1 will facilitate mediated
  // allocations.
  zx::iob ep0, ep1;
  zx_iob_region_t config[]{
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEp0OnlyRwMap | ZX_IOB_ACCESS_EP1_CAN_MEDIATED_WRITE,
          .size = ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_ID_ALLOCATOR},
          .private_region = {.options = 0},
      },
      {
          .type = ZX_IOB_REGION_TYPE_PRIVATE,
          .access = kIoBufferEpRwMap,
          .size = ZX_PAGE_SIZE,
          .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
          .private_region = {.options = 0},
      },
  };

  ASSERT_OK(zx::iob::create(0, config, std::size(config), &ep0, &ep1));

  zx::result<MappingHelper> region =
      MappingHelper::Create(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0, 0, 0, ZX_PAGE_SIZE);
  ASSERT_OK(region.status_value());
  ASSERT_NE(region->addr(), 0u);
  cpp20::span<std::byte> bytes{reinterpret_cast<std::byte*>(region->addr()), ZX_PAGE_SIZE};

  // Since mediated access is enabled, the region should already be initialized.
  iob::BlobIdAllocator allocator(bytes);

  // ZX_ERR_OUT_OF_RANGE (region index too large)
  {
    uint32_t id;
    zx_status_t status =
        zx_iob_allocate_id(ep1.get(), kOptions, 2, kBlob.data(), kBlob.size(), &id);
    EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, status);
  }

  // ZX_ERR_WRONG_TYPE (region not of ID_ALLOCATOR discipline)
  {
    uint32_t id;
    zx_status_t status =
        zx_iob_allocate_id(ep1.get(), kOptions, 1, kBlob.data(), kBlob.size(), &id);
    EXPECT_EQ(ZX_ERR_WRONG_TYPE, status);
  }

  // ZX_ERR_INVALID_ARGS (invalid options)
  {
    uint32_t id;
    zx_status_t status =
        zx_iob_allocate_id(ep1.get(), -1, kIdAllocatorIdx, kBlob.data(), kBlob.size(), &id);
    EXPECT_EQ(ZX_ERR_INVALID_ARGS, status);
  }

  // ZX_ERR_ACCESS_DENIED (endpoint not mediated-writable)
  {
    uint32_t id;
    zx_status_t status =
        zx_iob_allocate_id(ep0.get(), kOptions, kIdAllocatorIdx, kBlob.data(), kBlob.size(), &id);
    EXPECT_EQ(ZX_ERR_ACCESS_DENIED, status);
  }

  // ZX_ERR_NO_MEMORY (no memory left in container)
  {
    using AllocateError = iob::BlobIdAllocator::AllocateError;

    fit::result<AllocateError, uint32_t> result = fit::ok(0);
    while (result.is_ok()) {
      result = allocator.Allocate(kBlob);
    }
    ASSERT_EQ(AllocateError::kOutOfMemory, result.error_value());

    uint32_t id;
    zx_status_t status =
        zx_iob_allocate_id(ep1.get(), kOptions, kIdAllocatorIdx, kBlob.data(), kBlob.size(), &id);
    EXPECT_EQ(ZX_ERR_NO_MEMORY, status);
  }

  // ZX_ERR_IO_DATA_INTEGRITY (corrupted memory)
  {
    struct Header {
      uint32_t next_id;
      uint32_t blob_head;
    };
    Header* header = reinterpret_cast<Header*>(bytes.data());
    header->next_id = -1;

    uint32_t id;
    zx_status_t status =
        zx_iob_allocate_id(ep1.get(), kOptions, kIdAllocatorIdx, kBlob.data(), kBlob.size(), &id);
    EXPECT_EQ(ZX_ERR_IO_DATA_INTEGRITY, status);
  }

  // The whole region should be pinned, so decommitting should result in
  // ZX_ERR_BAD_STATE.
  EXPECT_EQ(ZX_ERR_BAD_STATE, zx::vmar::root_self()->op_range(
                                  ZX_VMAR_OP_DECOMMIT, reinterpret_cast<zx_vaddr_t>(bytes.data()),
                                  bytes.size(), nullptr, 0));
}

TEST(Iob, MapWhenPeerClosed) {
  zx::iob ep0, ep1;
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
  ASSERT_OK(zx::iob::create(0, &config, 1, &ep0, &ep1));
  ep1.reset();

  zx::result<MappingHelper> region =
      MappingHelper::Create(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, ep0, 0, 0, ZX_PAGE_SIZE);
  EXPECT_OK(region.status_value());
}

}  // namespace
