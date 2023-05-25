// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "usb/request-fidl.h"

#include <lib/fake-bti/bti.h>

#include <zxtest/zxtest.h>

namespace {

TEST(RequestFidlTest, EmptyRequestTest) {
  fuchsia_hardware_usb_request::Request request;
  usb::FidlRequest fidl_request(std::move(request));
}

TEST(RequestFidlTest, LengthTest) {
  fuchsia_hardware_usb_request::Request request;
  request.data()
      .emplace()
      .emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmoId(0))
      .offset(0)
      .size(16);
  request.data()
      ->emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmo(zx::vmo()))
      .offset(0)
      .size(16);
  usb::FidlRequest fidl_request(std::move(request));

  EXPECT_EQ(fidl_request.length(), 32);
}

TEST(RequestFidlTest, UnpinTest) {
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(16, 0, &vmo));
  fuchsia_hardware_usb_request::Request request;
  request.data()
      .emplace()
      .emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmo(std::move(vmo)))
      .offset(0)
      .size(16);
  request.data()
      ->emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmoId(1))
      .offset(0)
      .size(32);
  request.data()
      ->emplace_back()
      .buffer(
          fuchsia_hardware_usb_request::Buffer::WithData(std::vector<uint8_t>{0x0, 0x0, 0x0, 0x0}))
      .offset(0)
      .size(4);
  usb::FidlRequest fidl_request(std::move(request));

  zx::bti fake_bti;
  ASSERT_OK(fake_bti_create(fake_bti.reset_and_get_address()));

  EXPECT_OK(fidl_request.PhysMap(fake_bti));
  size_t actual;
  fake_bti_pinned_vmo_info_t info[2];
  EXPECT_OK(fake_bti_get_pinned_vmos(fake_bti.get(), info, 2, &actual));
  EXPECT_EQ(actual, 2);

  auto iter1 = fidl_request.phys_iter(0, zx_system_get_page_size());
  EXPECT_EQ((*iter1.begin()).second, 16);

  void* mapped;
  EXPECT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, zx::vmo(info[1].vmo),
                                       0, info[1].size, reinterpret_cast<uintptr_t*>(&mapped)));
  auto iter3 = fidl_request.phys_iter(2, zx_system_get_page_size());
  EXPECT_EQ((*iter3.begin()).second, 4);
  uint8_t expected_vals[] = {0xA, 0xB, 0xC, 0xC};
  memcpy(mapped, expected_vals, sizeof(expected_vals));
  EXPECT_OK(zx::vmar::root_self()->unmap(reinterpret_cast<uintptr_t>(mapped), info[1].size));

  EXPECT_OK(fidl_request.Unpin());
  EXPECT_OK(fake_bti_get_pinned_vmos(fake_bti.get(), nullptr, 0, &actual));
  EXPECT_EQ(actual, 0);
  EXPECT_BYTES_EQ((*fidl_request.request().data())[2].buffer()->data()->data(), expected_vals,
                  sizeof(expected_vals));
}

TEST(RequestFidlTest, VmoIdTest) {
  fuchsia_hardware_usb_request::Request request;
  request.data()
      .emplace()
      .emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmoId(0))
      .offset(0)
      .size(16);
  request.data()
      ->emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmoId(1))
      .offset(0)
      .size(16);
  request.data()
      .emplace()
      .emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmoId(2))
      .offset(0)
      .size(16);
  usb::FidlRequest fidl_request(std::move(request));

  zx::bti fake_bti;
  ASSERT_OK(fake_bti_create(fake_bti.reset_and_get_address()));

  EXPECT_OK(fidl_request.PhysMap(fake_bti));
  size_t actual;
  EXPECT_OK(fake_bti_get_pinned_vmos(fake_bti.get(), nullptr, 0, &actual));
  EXPECT_EQ(actual, 0);
}

TEST(RequestFidlTest, VmoTest) {
  zx::vmo vmo1, vmo2;
  ASSERT_OK(zx::vmo::create(16, 0, &vmo1));
  ASSERT_OK(zx::vmo::create(32, 0, &vmo2));
  fuchsia_hardware_usb_request::Request request;
  request.data()
      .emplace()
      .emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmo(std::move(vmo1)))
      .offset(0)
      .size(16);
  request.data()
      ->emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmo(std::move(vmo2)))
      .offset(0)
      .size(32);
  usb::FidlRequest fidl_request(std::move(request));

  zx::bti fake_bti;
  ASSERT_OK(fake_bti_create(fake_bti.reset_and_get_address()));

  EXPECT_OK(fidl_request.PhysMap(fake_bti));
  size_t actual;
  EXPECT_OK(fake_bti_get_pinned_vmos(fake_bti.get(), nullptr, 0, &actual));
  EXPECT_EQ(actual, 2);

  auto iter1 = fidl_request.phys_iter(0, zx_system_get_page_size());
  EXPECT_EQ((*iter1.begin()).second, 16);

  auto iter2 = fidl_request.phys_iter(1, zx_system_get_page_size());
  EXPECT_EQ((*iter2.begin()).second, 32);
}

TEST(RequestFidlTest, DataTest) {
  fuchsia_hardware_usb_request::Request request;
  uint8_t expected1[] = {0xF, 0xE, 0xD, 0xC, 0xB, 0xA, 0x9, 0x8,
                         0x7, 0x6, 0x5, 0x4, 0x3, 0x2, 0x1, 0x0};
  request.data()
      .emplace()
      .emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithData(
          std::vector<uint8_t>(std::begin(expected1), std::end(expected1))))
      .offset(0)
      .size(16);
  uint8_t expected2[] = {0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8, 0x9, 0xA,
                         0xB, 0xC, 0xD, 0xE, 0xF, 0x0, 0x1, 0x2, 0x3, 0x4, 0x5,
                         0x6, 0x7, 0x8, 0x9, 0xA, 0xB, 0xC, 0xD, 0xE, 0xF};
  request.data()
      ->emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithData(
          std::vector<uint8_t>(std::begin(expected2), std::end(expected2))))
      .offset(0)
      .size(32);
  usb::FidlRequest fidl_request(std::move(request));

  zx::bti fake_bti;
  ASSERT_OK(fake_bti_create(fake_bti.reset_and_get_address()));

  EXPECT_OK(fidl_request.PhysMap(fake_bti));
  size_t actual;
  fake_bti_pinned_vmo_info_t info[2];
  EXPECT_OK(fake_bti_get_pinned_vmos(fake_bti.get(), info, 2, &actual));
  EXPECT_EQ(actual, 2);

  void* mapped;
  EXPECT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, zx::vmo(info[0].vmo),
                                       0, info[0].size, reinterpret_cast<uintptr_t*>(&mapped)));
  auto iter1 = fidl_request.phys_iter(0, zx_system_get_page_size());
  EXPECT_BYTES_EQ(mapped, expected1, sizeof(expected1));
  EXPECT_EQ((*iter1.begin()).second, 16);
  EXPECT_OK(zx::vmar::root_self()->unmap(reinterpret_cast<uintptr_t>(mapped), info[0].size));

  EXPECT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, zx::vmo(info[1].vmo),
                                       0, info[1].size, reinterpret_cast<uintptr_t*>(&mapped)));
  auto iter2 = fidl_request.phys_iter(1, zx_system_get_page_size());
  EXPECT_BYTES_EQ(mapped, expected2, sizeof(expected2));
  EXPECT_EQ((*iter2.begin()).second, 32);
  EXPECT_OK(zx::vmar::root_self()->unmap(reinterpret_cast<uintptr_t>(mapped), info[1].size));
}

TEST(RequestFidlTest, MixedTest) {
  zx::vmo vmo1, vmo2;
  ASSERT_OK(zx::vmo::create(16, 0, &vmo1));
  ASSERT_OK(zx::vmo::create(32, 0, &vmo2));
  fuchsia_hardware_usb_request::Request request;
  request.data()
      .emplace()
      .emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmoId(3))
      .offset(0)
      .size(16);
  request.data()
      ->emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmo(std::move(vmo1)))
      .offset(0)
      .size(16);
  request.data()
      ->emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmoId(7))
      .offset(0)
      .size(16);
  request.data()
      ->emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmo(std::move(vmo2)))
      .offset(0)
      .size(32);
  usb::FidlRequest fidl_request(std::move(request));

  zx::bti fake_bti;
  ASSERT_OK(fake_bti_create(fake_bti.reset_and_get_address()));

  EXPECT_OK(fidl_request.PhysMap(fake_bti));
  size_t actual;
  EXPECT_OK(fake_bti_get_pinned_vmos(fake_bti.get(), nullptr, 0, &actual));
  EXPECT_EQ(actual, 2);

  auto iter1 = fidl_request.phys_iter(1, zx_system_get_page_size());
  EXPECT_EQ((*iter1.begin()).second, 16);

  auto iter2 = fidl_request.phys_iter(3, zx_system_get_page_size());
  EXPECT_EQ((*iter2.begin()).second, 32);
}

TEST(RequestFidlTest, PoolTest) {
  usb::FidlRequestPool pool;
  EXPECT_TRUE(pool.Empty());

  pool.Add(usb::FidlRequest(usb::EndpointType::BULK));
  EXPECT_TRUE(pool.Full());
  usb::FidlRequest control(usb::EndpointType::CONTROL);
  control.add_vmo_id(9, 2, 0).add_vmo_id(1, 4, 0);
  pool.Add(std::move(control));
  EXPECT_TRUE(pool.Full());

  {
    auto req = pool.Get();
    EXPECT_TRUE(req.has_value());
    EXPECT_FALSE(pool.Empty());
    EXPECT_FALSE(pool.Full());
    EXPECT_EQ(req->request().information()->Which(),
              fuchsia_hardware_usb_request::RequestInfo::Tag::kBulk);

    pool.Put(std::move(*req));
    EXPECT_TRUE(pool.Full());
  }

  {
    auto req = pool.Get();
    EXPECT_TRUE(req.has_value());
    EXPECT_FALSE(pool.Empty());
    EXPECT_FALSE(pool.Full());
    EXPECT_EQ(req->request().information()->Which(),
              fuchsia_hardware_usb_request::RequestInfo::Tag::kControl);
    EXPECT_EQ(req->request().data()->size(), 2);
    EXPECT_EQ(req->request().data()->at(0).buffer()->vmo_id().value(), 9);
    EXPECT_EQ(req->request().data()->at(0).size(), 2);
    EXPECT_EQ(req->request().data()->at(0).offset(), 0);
    EXPECT_EQ(req->request().data()->at(1).buffer()->vmo_id().value(), 1);
    EXPECT_EQ(req->request().data()->at(1).size(), 4);
    EXPECT_EQ(req->request().data()->at(1).offset(), 0);
  }

  {
    auto req = pool.Remove();
    EXPECT_TRUE(req.has_value());
    EXPECT_TRUE(pool.Empty());

    pool.Put(std::move(*req));
    EXPECT_TRUE(pool.Full());
  }

  {
    auto req = pool.Remove();
    EXPECT_TRUE(req.has_value());
    EXPECT_TRUE(pool.Empty());
  }

  {
    auto req = pool.Remove();
    EXPECT_FALSE(req.has_value());
  }

  // Make sure that we are able to destruct even with requests sitting in the pool.
  pool.Add(usb::FidlRequest(usb::EndpointType::BULK));
  EXPECT_TRUE(pool.Full());
}

}  // namespace
