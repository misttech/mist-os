// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/boot-shim/reboot-reason.h"

#include <lib/boot-shim/boot-shim.h>
#include <lib/fit/defer.h>
#include <lib/zbitl/image.h>

#include <array>

#include <zxtest/zxtest.h>

namespace {

template <typename Zbi, typename Pred>
bool HasZbiItem(Zbi&& zbi, Pred&& pred) {
  auto cleanup = fit::defer([&zbi]() { zbi.ignore_error(); });
  return std::any_of(zbi.begin(), zbi.end(),
                     [pred](auto item) -> bool { return pred(*item.header, item.payload); });
}

TEST(RebootReasonItemTest, NoRebootReason) {
  constexpr std::string_view kCmdline = "   foo-bar=not-reboot-reason   ";

  std::array<std::byte, 512> image_buffer;
  zbitl::Image<std::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  boot_shim::BootShim<boot_shim::RebootReasonItem> shim("test-shim", stdout);
  shim.Get<boot_shim::RebootReasonItem>().Init(kCmdline, shim.shim_name());

  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  ASSERT_FALSE(HasZbiItem(image, [](const zbi_header_t& header, zbitl::ByteView payload) {
    return header.type == ZBI_TYPE_HW_REBOOT_REASON;
  }));
}

TEST(RebootReasonItemTest, EmptyRebootReason) {
  constexpr std::string_view kCmdline = "androidboot.bootreason=";

  std::array<std::byte, 512> image_buffer;
  zbitl::Image<std::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  boot_shim::BootShim<boot_shim::RebootReasonItem> shim("test-shim", stdout);
  shim.Get<boot_shim::RebootReasonItem>().Init(kCmdline, shim.shim_name());

  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  ASSERT_FALSE(HasZbiItem(image, [](const zbi_header_t& header, zbitl::ByteView payload) {
    return header.type == ZBI_TYPE_HW_REBOOT_REASON;
  }));
}

TEST(RebootReasonItemTest, UnknownReason) {
  constexpr std::string_view kCmdline = "androidboot.bootreason=foo-bar";

  std::array<std::byte, 512> image_buffer;
  zbitl::Image<std::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  boot_shim::BootShim<boot_shim::RebootReasonItem> shim("test-shim", stdout);
  shim.Get<boot_shim::RebootReasonItem>().Init(kCmdline, shim.shim_name());

  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  ASSERT_FALSE(HasZbiItem(image, [](const zbi_header_t& header, zbitl::ByteView payload) {
    return header.type == ZBI_TYPE_HW_REBOOT_REASON;
  }));
}

TEST(RebootReasonItemTest, Warm) {
  constexpr std::string_view kCmdline = "androidboot.bootreason=warm";

  std::array<std::byte, 512> image_buffer;
  zbitl::Image<std::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  boot_shim::BootShim<boot_shim::RebootReasonItem> shim("test-shim", stdout);
  shim.Get<boot_shim::RebootReasonItem>().Init(kCmdline, shim.shim_name());

  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  ASSERT_TRUE(HasZbiItem(image, [](const zbi_header_t& header, zbitl::ByteView payload) {
    EXPECT_GE(payload.size_bytes(), sizeof(zbi_hw_reboot_reason_t));
    auto* reason = reinterpret_cast<const zbi_hw_reboot_reason_t*>(payload.data());
    return header.type == ZBI_TYPE_HW_REBOOT_REASON &&
           (payload.size_bytes() >= sizeof(zbi_hw_reboot_reason_t)) &&
           *reason == ZBI_HW_REBOOT_REASON_WARM;
  }));
}

TEST(RebootReasonItemTest, Hard) {
  constexpr std::string_view kCmdline = "androidboot.bootreason=hard";

  std::array<std::byte, 512> image_buffer;
  zbitl::Image<std::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  boot_shim::BootShim<boot_shim::RebootReasonItem> shim("test-shim", stdout);
  shim.Get<boot_shim::RebootReasonItem>().Init(kCmdline, shim.shim_name());

  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  ASSERT_TRUE(HasZbiItem(image, [](const zbi_header_t& header, zbitl::ByteView payload) {
    EXPECT_GE(payload.size_bytes(), sizeof(zbi_hw_reboot_reason_t));
    auto* reason = reinterpret_cast<const zbi_hw_reboot_reason_t*>(payload.data());
    return header.type == ZBI_TYPE_HW_REBOOT_REASON &&
           (payload.size_bytes() >= sizeof(zbi_hw_reboot_reason_t)) &&
           *reason == ZBI_HW_REBOOT_REASON_WARM;
  }));
}

TEST(RebootReasonItemTest, Cold) {
  constexpr std::string_view kCmdline = "androidboot.bootreason=cold";

  std::array<std::byte, 512> image_buffer;
  zbitl::Image<std::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  boot_shim::BootShim<boot_shim::RebootReasonItem> shim("test-shim", stdout);
  shim.Get<boot_shim::RebootReasonItem>().Init(kCmdline, shim.shim_name());

  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  ASSERT_TRUE(HasZbiItem(image, [](const zbi_header_t& header, zbitl::ByteView payload) {
    EXPECT_GE(payload.size_bytes(), sizeof(zbi_hw_reboot_reason_t));
    auto* reason = reinterpret_cast<const zbi_hw_reboot_reason_t*>(payload.data());
    return header.type == ZBI_TYPE_HW_REBOOT_REASON &&
           (payload.size_bytes() >= sizeof(zbi_hw_reboot_reason_t)) &&
           *reason == ZBI_HW_REBOOT_REASON_COLD;
  }));
}

TEST(RebootReasonItemTest, Watchdog) {
  constexpr std::string_view kCmdline = "androidboot.bootreason=watchdog";

  std::array<std::byte, 512> image_buffer;
  zbitl::Image<std::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  boot_shim::BootShim<boot_shim::RebootReasonItem> shim("test-shim", stdout);
  shim.Get<boot_shim::RebootReasonItem>().Init(kCmdline, shim.shim_name());

  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  ASSERT_TRUE(HasZbiItem(image, [](const zbi_header_t& header, zbitl::ByteView payload) {
    EXPECT_GE(payload.size_bytes(), sizeof(zbi_hw_reboot_reason_t));
    auto* reason = reinterpret_cast<const zbi_hw_reboot_reason_t*>(payload.data());
    return header.type == ZBI_TYPE_HW_REBOOT_REASON &&
           (payload.size_bytes() >= sizeof(zbi_hw_reboot_reason_t)) &&
           *reason == ZBI_HW_REBOOT_REASON_WATCHDOG;
  }));
}

TEST(RebootReasonItemTest, ParsingAtMultiplePositions) {
  constexpr std::array kCmdlines = {
      "androidboot.bootreason=watchdog",
      "androidboot.bootreason=watchdog ",
      " androidboot.bootreason=watchdog",
      " androidboot.bootreason=watchdog,subreason",
      " androidboot.bootreason=watchdog,subreason,detail1,detail2",
      " androidboot.bootreason=watchdog,subreason,detail",
  };

  std::array<std::byte, 512> image_buffer;
  zbitl::Image<std::span<std::byte>> image(image_buffer);
  for (auto cmdline : kCmdlines) {
    ASSERT_TRUE(image.clear().is_ok());

    boot_shim::BootShim<boot_shim::RebootReasonItem> shim("test-shim", stdout);
    shim.Get<boot_shim::RebootReasonItem>().Init(cmdline, shim.shim_name());

    ASSERT_TRUE(shim.AppendItems(image).is_ok());

    ASSERT_TRUE(HasZbiItem(image, [](const zbi_header_t& header, zbitl::ByteView payload) {
      EXPECT_GE(payload.size_bytes(), sizeof(zbi_hw_reboot_reason_t));
      auto* reason = reinterpret_cast<const zbi_hw_reboot_reason_t*>(payload.data());
      return header.type == ZBI_TYPE_HW_REBOOT_REASON &&
             (payload.size_bytes() >= sizeof(zbi_hw_reboot_reason_t)) &&
             *reason == ZBI_HW_REBOOT_REASON_WATCHDOG;
    }));
  }
}

}  // namespace
