// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <utility>

#include <zxtest/zxtest.h>

#include "src/storage/lib/vfs/cpp/remote_dir.h"

namespace {

TEST(RemoteDir, ApiTest) {
  auto endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();

  const fidl::UnownedClientEnd unowned_client = endpoints.client.borrow();
  auto dir = fbl::MakeRefCounted<fs::RemoteDir>(std::move(endpoints.client));

  // get attributes
  zx::result<fs::VnodeAttributes> attr = dir->GetAttributes();
  ASSERT_TRUE(attr.is_ok());
  EXPECT_EQ(V_TYPE_DIR | V_IRUSR, attr->mode);

  // get remote properties
  ASSERT_TRUE(dir->IsRemote());
  EXPECT_EQ(dir->client_end(), unowned_client);
}

}  // namespace
