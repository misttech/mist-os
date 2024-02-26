// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/wire.h>
#include <zircon/errors.h>

#include <type_traits>
#include <utility>

#include <zxtest/zxtest.h>

#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace {

namespace fio = fuchsia_io;

TEST(Rights, ReadOnly) {
  // clang-format off
  EXPECT_TRUE (fs::Rights::ReadOnly().read,    "Bad value for Rights::ReadOnly().read");
  EXPECT_FALSE(fs::Rights::ReadOnly().write,   "Bad value for Rights::ReadOnly().write");
  EXPECT_FALSE(fs::Rights::ReadOnly().execute, "Bad value for Rights::ReadOnly().execute");
  // clang-format on
}

TEST(Rights, WriteOnly) {
  // clang-format off
  EXPECT_FALSE(fs::Rights::WriteOnly().read,    "Bad value for Rights::WriteOnly().read");
  EXPECT_TRUE (fs::Rights::WriteOnly().write,   "Bad value for Rights::WriteOnly().write");
  EXPECT_FALSE(fs::Rights::WriteOnly().execute, "Bad value for Rights::WriteOnly().execute");
  // clang-format on
}

TEST(Rights, ReadWrite) {
  // clang-format off
  EXPECT_TRUE (fs::Rights::ReadWrite().read,    "Bad value for Rights::ReadWrite().read");
  EXPECT_TRUE (fs::Rights::ReadWrite().write,   "Bad value for Rights::ReadWrite().write");
  EXPECT_FALSE(fs::Rights::ReadWrite().execute, "Bad value for Rights::ReadWrite().execute");
  // clang-format on
}

TEST(Rights, ReadExec) {
  // clang-format off
  EXPECT_TRUE (fs::Rights::ReadExec().read,    "Bad value for Rights::ReadExec().read");
  EXPECT_FALSE(fs::Rights::ReadExec().write,   "Bad value for Rights::ReadExec().write");
  EXPECT_TRUE (fs::Rights::ReadExec().execute, "Bad value for Rights::ReadExec().execute");
  // clang-format on
}

TEST(Rights, WriteExec) {
  // clang-format off
  EXPECT_FALSE(fs::Rights::WriteExec().read,    "Bad value for Rights::WriteExec().read");
  EXPECT_TRUE (fs::Rights::WriteExec().write,   "Bad value for Rights::WriteExec().write");
  EXPECT_TRUE (fs::Rights::WriteExec().execute, "Bad value for Rights::WriteExec().execute");
  // clang-format on
}

TEST(Rights, All) {
  // clang-format off
  EXPECT_TRUE (fs::Rights::All().read,    "Bad value for Rights::All().read");
  EXPECT_TRUE (fs::Rights::All().write,   "Bad value for Rights::All().write");
  EXPECT_TRUE (fs::Rights::All().execute, "Bad value for Rights::All().execute");
  // clang-format on
}

class DummyVnode : public fs::Vnode {
 public:
  DummyVnode() = default;
};

#define EXPECT_RESULT_OK(expr) EXPECT_TRUE((expr).is_ok())
#define EXPECT_RESULT_ERROR(error_val, expr) \
  EXPECT_TRUE((expr).is_error());            \
  EXPECT_EQ(error_val, (expr).status_value())

TEST(VnodeConnectionOptions, ValidateOptionsForDirectory) {
  class TestDirectory : public DummyVnode {
   public:
    fuchsia_io::NodeProtocolKinds GetProtocols() const final {
      return fuchsia_io::NodeProtocolKinds::kDirectory;
    }
  };

  TestDirectory vnode;
  EXPECT_RESULT_OK(vnode.ValidateOptions(
      fs::VnodeConnectionOptions::FromIoV1Flags(fio::wire::OpenFlags::kDirectory)));
  EXPECT_RESULT_ERROR(ZX_ERR_NOT_FILE,
                      vnode.ValidateOptions(fs::VnodeConnectionOptions::FromIoV1Flags(
                          fio::wire::OpenFlags::kNotDirectory)));
}

TEST(VnodeConnectionOptions, ValidateOptionsForService) {
  class TestConnector : public DummyVnode {
   public:
    fuchsia_io::NodeProtocolKinds GetProtocols() const final {
      return fuchsia_io::NodeProtocolKinds::kConnector;
    }
  };

  TestConnector vnode;
  EXPECT_RESULT_ERROR(ZX_ERR_NOT_DIR,
                      vnode.ValidateOptions(fs::VnodeConnectionOptions::FromIoV1Flags(
                          fio::wire::OpenFlags::kDirectory)));
  EXPECT_RESULT_OK(vnode.ValidateOptions(
      fs::VnodeConnectionOptions::FromIoV1Flags(fio::wire::OpenFlags::kNotDirectory)));
}

TEST(VnodeConnectionOptions, ValidateOptionsForFile) {
  class TestFile : public DummyVnode {
   public:
    fuchsia_io::NodeProtocolKinds GetProtocols() const final {
      return fuchsia_io::NodeProtocolKinds::kFile;
    }
  };

  TestFile vnode;
  EXPECT_RESULT_ERROR(ZX_ERR_NOT_DIR,
                      vnode.ValidateOptions(fs::VnodeConnectionOptions::FromIoV1Flags(
                          fio::wire::OpenFlags::kDirectory)));
  EXPECT_RESULT_OK(vnode.ValidateOptions(
      fs::VnodeConnectionOptions::FromIoV1Flags(fio::wire::OpenFlags::kNotDirectory)));
}

}  // namespace
