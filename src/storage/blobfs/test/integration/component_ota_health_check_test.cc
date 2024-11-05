// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.update.verify/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/zx/channel.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <string>

#include <gtest/gtest.h>

#include "src/storage/blobfs/test/integration/blobfs_fixtures.h"

namespace blobfs {
namespace {

namespace fuv = fuchsia_update_verify;

class OtaHealthCheckServiceTest : public BlobfsTest {
 protected:
  fidl::WireSyncClient<fuv::ComponentOtaHealthCheck> ConnectToHealthCheckService() {
    auto client_end = component::ConnectAt<fuv::ComponentOtaHealthCheck>(fs().ServiceDirectory());
    EXPECT_EQ(client_end.status_value(), ZX_OK);
    return fidl::WireSyncClient<fuv::ComponentOtaHealthCheck>(std::move(*client_end));
  }
};

// This test mainly exists to ensure that the service is exported correctly. The business logic is
// exercised by other unit tests.
TEST_F(OtaHealthCheckServiceTest, EmptyFilesystemIsValid) {
  auto status = ConnectToHealthCheckService()->GetHealthStatus();
  ASSERT_EQ(status.status(), ZX_OK) << status.error();
}

}  // namespace
}  // namespace blobfs
