// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ZXIO_POSIX_MODE_H_
#define LIB_ZXIO_POSIX_MODE_H_

#include <lib/zxio/types.h>

__BEGIN_CDECLS

// Approximates POSIX mode bits based on the `protocols` and `abilities` of a `fuchsia.io/Node`.
// Only some fuchsia.io servers support POSIX attributes, which can be set/retrieved as part of
// `fuchsia.io/MutableNodeAttributes`. For servers/filesystems that do not support POSIX attributes,
// this function can be used to *approximate* the equivalent POSIX mode bits (type and permissions).
//
// `protocols` is used to derive the file's type (e.g. `S_IFREG` or `S_IFDIR`), and a combination
// of `protocols` and `abilities` is used to derive the owner permission bits (`S_I*USR`).
//
// **NOTE**: Only owner bits (`S_IRUSR` / `S_IWUSR` / `S_IXUSR`) are set on the resulting mode.
// TODO(https://fxbug.dev/324111518): Remove this when it is no longer required by fdio.
uint32_t zxio_get_posix_mode(zxio_node_protocols_t protocols, zxio_abilities_t abilities);

__END_CDECLS

#endif  // LIB_ZXIO_POSIX_MODE_H_
