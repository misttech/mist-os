// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/util.h"

#include <gtest/gtest.h>

namespace bt::hci_spec {
namespace {

TEST(UtilTest, LinkKeyTypeToString) {
  std::string link_key_type =
      LinkKeyTypeToString(hci_spec::LinkKeyType::kCombination);
  EXPECT_EQ("kCombination", link_key_type);

  link_key_type = LinkKeyTypeToString(hci_spec::LinkKeyType::kLocalUnit);
  EXPECT_EQ("kLocalUnit", link_key_type);

  link_key_type = LinkKeyTypeToString(hci_spec::LinkKeyType::kRemoteUnit);
  EXPECT_EQ("kRemoteUnit", link_key_type);

  link_key_type = LinkKeyTypeToString(hci_spec::LinkKeyType::kDebugCombination);
  EXPECT_EQ("kDebugCombination", link_key_type);

  link_key_type = LinkKeyTypeToString(
      hci_spec::LinkKeyType::kUnauthenticatedCombination192);
  EXPECT_EQ("kUnauthenticatedCombination192", link_key_type);

  link_key_type =
      LinkKeyTypeToString(hci_spec::LinkKeyType::kAuthenticatedCombination192);
  EXPECT_EQ("kAuthenticatedCombination192", link_key_type);

  link_key_type =
      LinkKeyTypeToString(hci_spec::LinkKeyType::kChangedCombination);
  EXPECT_EQ("kChangedCombination", link_key_type);

  link_key_type = LinkKeyTypeToString(
      hci_spec::LinkKeyType::kUnauthenticatedCombination256);
  EXPECT_EQ("kUnauthenticatedCombination256", link_key_type);

  link_key_type =
      LinkKeyTypeToString(hci_spec::LinkKeyType::kAuthenticatedCombination256);
  EXPECT_EQ("kAuthenticatedCombination256", link_key_type);

  // Unknown link key type
  link_key_type = LinkKeyTypeToString(static_cast<hci_spec::LinkKeyType>(0xFF));
  EXPECT_EQ("(Unknown)", link_key_type);
}

}  // namespace
}  // namespace bt::hci_spec
