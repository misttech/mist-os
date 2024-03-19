// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Tests that ZXIO constants are synchronized with fuchsia.io FIDL constants.

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/zxio/types.h>

namespace {

namespace fio = fuchsia_io;

using fio::NodeProtocolKinds;
static_assert(ZXIO_NODE_PROTOCOL_CONNECTOR == static_cast<uint64_t>(NodeProtocolKinds::kConnector));
static_assert(ZXIO_NODE_PROTOCOL_DIRECTORY == static_cast<uint64_t>(NodeProtocolKinds::kDirectory));
static_assert(ZXIO_NODE_PROTOCOL_FILE == static_cast<uint64_t>(NodeProtocolKinds::kFile));
static_assert(ZXIO_NODE_PROTOCOL_SYMLINK == static_cast<uint64_t>(NodeProtocolKinds::kSymlink));

using fio::Operations;
static_assert(ZXIO_OPERATION_CONNECT == static_cast<uint64_t>(Operations::kConnect));
static_assert(ZXIO_OPERATION_READ_BYTES == static_cast<uint64_t>(Operations::kReadBytes));
static_assert(ZXIO_OPERATION_WRITE_BYTES == static_cast<uint64_t>(Operations::kWriteBytes));
static_assert(ZXIO_OPERATION_EXECUTE == static_cast<uint64_t>(Operations::kExecute));
static_assert(ZXIO_OPERATION_GET_ATTRIBUTES == static_cast<uint64_t>(Operations::kGetAttributes));
static_assert(ZXIO_OPERATION_UPDATE_ATTRIBUTES ==
              static_cast<uint64_t>(Operations::kUpdateAttributes));
static_assert(ZXIO_OPERATION_ENUMERATE == static_cast<uint64_t>(Operations::kEnumerate));
static_assert(ZXIO_OPERATION_TRAVERSE == static_cast<uint64_t>(Operations::kTraverse));
static_assert(ZXIO_OPERATION_MODIFY_DIRECTORY ==
              static_cast<uint64_t>(Operations::kModifyDirectory));
static_assert(ZXIO_OPERATION_ALL == static_cast<uint64_t>(Operations::kMask));

#if __Fuchsia_API_level__ >= 19
using fio::CreationMode;
static_assert(ZXIO_CREATION_MODE_NEVER == static_cast<uint32_t>(CreationMode::kNever));
static_assert(ZXIO_CREATION_MODE_NEVER_DEPRECATED ==
              static_cast<uint32_t>(CreationMode::kNeverDeprecated));
static_assert(ZXIO_CREATION_MODE_ALLOW_EXISTING ==
              static_cast<uint32_t>(CreationMode::kAllowExisting));
static_assert(ZXIO_CREATION_MODE_ALWAYS == static_cast<uint32_t>(CreationMode::kAlways));
#else
using fio::OpenMode;
static_assert(ZXIO_CREATION_MODE_NEVER_DEPRECATED ==
              static_cast<uint32_t>(OpenMode::kOpenExisting));
static_assert(ZXIO_CREATION_MODE_ALLOW_EXISTING == static_cast<uint32_t>(OpenMode::kMaybeCreate));
static_assert(ZXIO_CREATION_MODE_ALWAYS == static_cast<uint32_t>(OpenMode::kAlwaysCreate));
#endif

}  // namespace
