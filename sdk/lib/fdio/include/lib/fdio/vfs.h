// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_FDIO_VFS_H_
#define LIB_FDIO_VFS_H_

#include <stdint.h>
#include <zircon/availability.h>
#include <zircon/compiler.h>

__BEGIN_CDECLS

// bits compatible with POSIX stat
#define V_TYPE_MASK 0170000
#define V_TYPE_SOCK 0140000
#define V_TYPE_LINK 0120000
#define V_TYPE_FILE 0100000
#define V_TYPE_BDEV 0060000
#define V_TYPE_DIR 0040000
#define V_TYPE_CDEV 0020000
#define V_TYPE_PIPE 0010000

#define V_ISUID 0004000
#define V_ISGID 0002000
#define V_ISVTX 0001000
#define V_IRWXU 0000700
#define V_IRUSR 0000400
#define V_IWUSR 0000200
#define V_IXUSR 0000100
#define V_IRWXG 0000070
#define V_IRGRP 0000040
#define V_IWGRP 0000020
#define V_IXGRP 0000010
#define V_IRWXO 0000007
#define V_IROTH 0000004
#define V_IWOTH 0000002
#define V_IXOTH 0000001

#define VTYPE_TO_DTYPE(mode) (((mode) & V_TYPE_MASK) >> 12)
#define DTYPE_TO_VTYPE(type) (((type) & 15) << 12)

__END_CDECLS

#endif  // LIB_FDIO_VFS_H_
