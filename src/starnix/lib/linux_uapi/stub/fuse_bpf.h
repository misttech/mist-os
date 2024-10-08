// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_LIB_LINUX_UAPI_STUB_FUSE_BPF_H_
#define SRC_STARNIX_LIB_LINUX_UAPI_STUB_FUSE_BPF_H_

// Extension of the fuse_entry_out structure, used by fuse-bpf to specify the
// bpf parameters.
struct fuse_entry_out_extended {
  struct fuse_entry_out arg;
  struct fuse_entry_bpf_out bpf_arg;
};

_Static_assert(offsetof(struct fuse_entry_out_extended, bpf_arg) == sizeof(struct fuse_entry_out),
               "Struct requires packed");

#endif  // SRC_STARNIX_LIB_LINUX_UAPI_STUB_FUSE_BPF_H_
