// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/iso/iso_common.h"

#include <gtest/gtest.h>

namespace bt::iso {

TEST(IsoCommonTest, CigCisIdentifier) {
  CigCisIdentifier id1(0x10, 0x11);
  CigCisIdentifier id2(0x10, 0x12);
  CigCisIdentifier id3(0x11, 0x10);
  CigCisIdentifier id4(0x10, 0x10);
  CigCisIdentifier id5(0x10, 0x11);

  EXPECT_FALSE(id1 == id2);
  EXPECT_FALSE(id2 == id4);
  EXPECT_FALSE(id3 == id4);
  EXPECT_TRUE(id5 == id1);
  EXPECT_TRUE(id1 == id5);
  EXPECT_EQ(id1.cig_id(), 0x10);
  EXPECT_EQ(id1.cis_id(), 0x11);
}

}  // namespace bt::iso
