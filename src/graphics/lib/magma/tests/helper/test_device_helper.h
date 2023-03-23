// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_LIB_MAGMA_TESTS_HELPER_TEST_DEVICE_HELPER_H_
#define SRC_GRAPHICS_LIB_MAGMA_TESTS_HELPER_TEST_DEVICE_HELPER_H_

#include <fidl/fuchsia.device/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/magma/magma.h>
#include <lib/zx/channel.h>

#include <filesystem>

#include <gtest/gtest.h>

namespace magma {
class TestDeviceBase {
 public:
  explicit TestDeviceBase(std::string device_name) { InitializeFromFileName(device_name.c_str()); }

  explicit TestDeviceBase(uint64_t vendor_id) { InitializeFromVendorId(vendor_id); }

  TestDeviceBase() = default;

  void InitializeFromFileName(const char* device_name) {
    auto client = component::Connect<fuchsia_device::Controller>(device_name);
    ASSERT_TRUE(client.is_ok());
    device_controller_ = client->borrow();
    EXPECT_EQ(MAGMA_STATUS_OK, magma_device_import(client->TakeChannel().release(), &device_));
  }

  void InitializeFromVendorId(uint64_t id) {
    for (auto& p : std::filesystem::directory_iterator("/dev/class/gpu")) {
      InitializeFromFileName(p.path().c_str());
      uint64_t vendor_id;
      magma_status_t magma_status =
          magma_device_query(device_, MAGMA_QUERY_VENDOR_ID, NULL, &vendor_id);
      if (magma_status == MAGMA_STATUS_OK && vendor_id == id) {
        return;
      }

      magma_device_release(device_);
      device_ = 0;
    }
    GTEST_FAIL();
  }

  // Get a channel to the parent device, so we can rebind the driver to it. This
  // requires sandbox access to /dev/sys.
  fidl::ClientEnd<fuchsia_device::Controller> GetParentDevice() {
    char path[fuchsia_device::wire::kMaxDevicePathLen + 1];
    auto res = fidl::WireCall(device_controller_)->GetTopologicalPath();

    EXPECT_EQ(ZX_OK, res.status());
    EXPECT_TRUE(res->is_ok());

    auto& response = *res->value();
    EXPECT_LE(response.path.size(), fuchsia_device::wire::kMaxDevicePathLen);

    memcpy(path, response.path.data(), response.path.size());
    path[response.path.size()] = 0;
    // Remove everything after the final slash.
    *strrchr(path, '/') = 0;

    auto parent = component::Connect<fuchsia_device::Controller>(path);

    EXPECT_EQ(ZX_OK, parent.status_value());
    return std::move(*parent);
  }

  static fidl::ClientEnd<fuchsia_device::Controller> GetParentDeviceFromId(uint64_t id) {
    magma::TestDeviceBase test_base(id);
    return test_base.GetParentDevice();
  }

  static void RebindParentDeviceFromId(uint64_t id, const std::string& url_suffix = "") {
    fidl::ClientEnd parent = GetParentDeviceFromId(id);
    RebindDevice(parent);
  }

  static void RebindDevice(fidl::UnownedClientEnd<fuchsia_device::Controller> device,
                           const std::string& url_suffix = "") {
    fidl::WireResult result =
        fidl::WireCall(device)->Rebind(fidl::StringView::FromExternal(url_suffix));
    ASSERT_EQ(ZX_OK, result.status());
    ASSERT_TRUE(result->is_ok()) << zx_status_get_string(result->error_value());
  }

  const fidl::UnownedClientEnd<fuchsia_device::Controller>& channel() { return device_controller_; }

  magma_device_t device() const { return device_; }

  uint32_t GetDeviceId() const {
    uint64_t value;
    magma_status_t status = magma_device_query(device_, MAGMA_QUERY_DEVICE_ID, nullptr, &value);
    if (status != MAGMA_STATUS_OK)
      return 0;
    return static_cast<uint32_t>(value);
  }

  uint32_t GetVendorId() const {
    uint64_t value;
    magma_status_t status = magma_device_query(device_, MAGMA_QUERY_VENDOR_ID, nullptr, &value);
    if (status != MAGMA_STATUS_OK)
      return 0;
    return static_cast<uint32_t>(value);
  }

  bool IsIntelGen12() {
    if (GetVendorId() != 0x8086)
      return false;

    switch (GetDeviceId()) {
      case 0x9A40:
      case 0x9A49:
        return true;
    }
    return false;
  }

  ~TestDeviceBase() {
    if (device_)
      magma_device_release(device_);
  }

 private:
  magma_device_t device_ = 0;
  fidl::UnownedClientEnd<fuchsia_device::Controller> device_controller_{ZX_HANDLE_INVALID};
};

}  // namespace magma

#endif  // SRC_GRAPHICS_LIB_MAGMA_TESTS_HELPER_TEST_DEVICE_HELPER_H_
