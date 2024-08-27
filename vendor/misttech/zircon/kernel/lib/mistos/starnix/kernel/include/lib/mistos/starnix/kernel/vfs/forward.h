// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FORWARD_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FORWARD_H_

#include <lib/mistos/util/weak_wrapper.h>

#include <fbl/ref_ptr.h>

namespace starnix {

class DirEntry;
using DirEntryHandle = fbl::RefPtr<DirEntry>;

class FileObject;
class FileOps;
using FileHandle = fbl::RefPtr<FileObject>;
using WeakFileHandle = util::WeakPtr<FileObject>;

class FileSystem;
using FileSystemHandle = fbl::RefPtr<FileSystem>;

class FdTable;
class FsContext;
class FsNode;
struct FsNodeInfo;
class FsNodeOps;
using FsNodeHandle = fbl::RefPtr<FsNode>;
using WeakFsNodeHandle = util::WeakPtr<FsNode>;

struct LookupContext;

class Mount;
using MountHandle = fbl::RefPtr<Mount>;

struct NamespaceNode;

class Pipe;
using PipeHandle = fbl::RefPtr<Pipe>;

struct SymlinkTarget;

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FORWARD_H_
