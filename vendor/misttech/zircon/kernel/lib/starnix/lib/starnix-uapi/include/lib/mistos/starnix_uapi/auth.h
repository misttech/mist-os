// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_AUTH_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_AUTH_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <stdint.h>

#include <fbl/vector.h>

namespace starnix_uapi {

// The owner and group of a file. Used as a parameter for functions that create files.
struct FsCred {
  uid_t uid;
  gid_t gid;

  // Define static method to create root FsCred
  static FsCred root() { return {0, 0}; }
};

struct Capabilities {
  uint64_t mask;
};

struct Credentials {
  uid_t uid;
  gid_t gid;
  uid_t euid;
  gid_t egid;
  uid_t saved_uid;
  gid_t saved_gid;
  //fbl::Vector<gid_t> groups;

  // See https://man7.org/linux/man-pages/man2/setfsuid.2.html
  uid_t fsuid;

  // See https://man7.org/linux/man-pages/man2/setfsgid.2.html
  gid_t fsgid;

  /// From https://man7.org/linux/man-pages/man7/capabilities.7.html
  ///
  /// > This is a limiting superset for the effective capabilities that the thread may assume. It
  /// > is also a limiting superset for the capabilities that may be added to the inheritable set
  /// > by a thread that does not have the CAP_SETPCAP capability in its effective set.
  ///
  /// > If a thread drops a capability from its permitted set, it can never reacquire that
  /// > capability (unless it execve(2)s either a set-user-ID-root program, or a program whose
  /// > associated file capabilities grant that capability).
  Capabilities cap_permitted;

  /// From https://man7.org/linux/man-pages/man7/capabilities.7.html
  ///
  /// > This is the set of capabilities used by the kernel to perform permission checks for the
  /// > thread.
  Capabilities cap_effective;

  /// From https://man7.org/linux/man-pages/man7/capabilities.7.html
  ///
  /// > This is a set of capabilities preserved across an execve(2).  Inheritable capabilities
  /// > remain inheritable when executing any program, and inheritable capabilities are added to
  /// > the permitted set when executing a program that has the corresponding bits set in the file
  /// > inheritable set.
  ///
  /// > Because inheritable capabilities are not generally preserved across execve(2) when running
  /// > as a non-root user, applications that wish to run helper programs with elevated
  /// > capabilities should consider using ambient capabilities, described below.
  Capabilities cap_inheritable;

  /// From https://man7.org/linux/man-pages/man7/capabilities.7.html
  ///
  /// > The capability bounding set is a mechanism that can be used to limit the capabilities that
  /// > are gained during execve(2).
  ///
  /// > Since Linux 2.6.25, this is a per-thread capability set. In older kernels, the capability
  /// > bounding set was a system wide attribute shared by all threads on the system.
  Capabilities cap_bounding;

  /// From https://man7.org/linux/man-pages/man7/capabilities.7.html
  ///
  /// > This is a set of capabilities that are preserved across an execve(2) of a program that is
  /// > not privileged.  The ambient capability set obeys the invariant that no capability can
  /// > ever be ambient if it is not both permitted and inheritable.
  ///
  /// > Executing a program that changes UID or GID due to the set-user-ID or set-group-ID bits
  /// > or executing a program that has any file capabilities set will clear the ambient set.
  Capabilities cap_ambient;

  /// From https://man7.org/linux/man-pages/man7/capabilities.7.html
  ///
  /// > Starting with kernel 2.6.26, and with a kernel in which file capabilities are enabled,
  /// > Linux implements a set of per-thread securebits flags that can be used to disable special
  /// > handling of capabilities for UID 0 (root).
  ///
  /// > The securebits flags can be modified and retrieved using the prctl(2)
  /// > PR_SET_SECUREBITS and PR_GET_SECUREBITS operations.  The CAP_SETPCAP capability is
  /// > required to modify the flags.
  // SecureBits securebits;

  /// impl Credentials

  // Creates a set of credentials with all possible permissions and capabilities.
  static Credentials root() { return with_ids(0, 0); }

  /// Creates a set of credentials with the given uid and gid. If the uid is 0, the credentials
  /// will grant superuser access.
  static Credentials with_ids(uid_t _uid, gid_t _gid) {
    // auto caps = uid == 0 ? Capabilities::all() : Capabilities::empty();
    return {.uid = _uid,
            .gid = _gid,
            .euid = _uid,
            .egid = _gid,
            .saved_uid = _uid,
            .saved_gid = _gid,
            .fsuid = _uid,
            .fsgid = _gid};
  }

  FsCred as_fscred() const { return {fsuid, fsgid}; }
};

}  // namespace starnix_uapi

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_AUTH_H_
