// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_AUTH_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_AUTH_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/util/error_propagation.h>
#include <stdint.h>

#include <algorithm>

#include <fbl/vector.h>

#include <linux/securebits.h>

namespace starnix_uapi {

// The owner and group of a file. Used as a parameter for functions that create files.
struct FsCred {
  uid_t uid_;
  gid_t gid_;

  // Define static method to create root FsCred
  static FsCred root() { return {.uid_ = 0, .gid_ = 0}; }
};

struct Capabilities {
  uint64_t mask_;

  // impl Capabilities
  static Capabilities empty() { return Capabilities{.mask_ = 0}; }

  static Capabilities all() { return Capabilities{.mask_ = UINT64_MAX}; }

  Capabilities union_with(const Capabilities& caps) const {
    Capabilities new_caps = *this;
    new_caps.insert(caps);
    return new_caps;
  }

  Capabilities difference(const Capabilities& caps) const {
    Capabilities new_caps = *this;
    new_caps.remove(caps);
    return new_caps;
  }

  bool contains(const Capabilities& caps) const { return (*this & caps) == caps; }

  void insert(const Capabilities& caps) { *this |= caps; }

  void remove(const Capabilities& caps) { *this &= ~caps; }

  uint32_t as_abi_v1() const { return static_cast<uint32_t>(mask_); }

  static Capabilities from_abi_v1(uint32_t bits) {
    return Capabilities{.mask_ = static_cast<uint64_t>(bits)};
  }

  std::pair<uint32_t, uint32_t> as_abi_v3() const {
    return std::make_pair(static_cast<uint32_t>(mask_), static_cast<uint32_t>(mask_ >> 32));
  }

  static Capabilities from_abi_v3(std::pair<uint32_t, uint32_t> u32s) {
    return Capabilities{.mask_ = static_cast<uint64_t>(u32s.first) |
                                 (static_cast<uint64_t>(u32s.second) << 32)};
  }

  // impl std::convert::TryFrom<u64> for Capabilities
  static fit::result<Errno, Capabilities> try_from(uint64_t capability_num) {
    if (capability_num >= 64) {
      return fit::error(errno(EINVAL));
    }
    return fit::ok(Capabilities{.mask_ = 1ULL << capability_num});
  }

  // impl ops::BitAnd for Capabilities
  Capabilities operator&(const Capabilities& other) const {
    return Capabilities{.mask_ = mask_ & other.mask_};
  }

  // impl ops::BitAndAssign for Capabilities
  Capabilities& operator&=(const Capabilities& other) {
    mask_ &= other.mask_;
    return *this;
  }

  // impl ops::BitOr for Capabilities
  Capabilities operator|(const Capabilities& other) const {
    return Capabilities{.mask_ = mask_ | other.mask_};
  }

  // impl ops::BitOrAssign for Capabilities
  Capabilities& operator|=(const Capabilities& other) {
    mask_ |= other.mask_;
    return *this;
  }

  // impl ops::Not for Capabilities
  Capabilities operator~() const { return Capabilities{.mask_ = ~mask_}; }

  bool operator==(const Capabilities& other) const { return mask_ == other.mask_; }
};

// Capability constants
static constexpr Capabilities kCapChown{.mask_ = 1ULL << CAP_CHOWN};
static constexpr Capabilities kCapDacOverride{.mask_ = 1ULL << CAP_DAC_OVERRIDE};
static constexpr Capabilities kCapDacReadSearch{.mask_ = 1ULL << CAP_DAC_READ_SEARCH};
static constexpr Capabilities kCapFowner{.mask_ = 1ULL << CAP_FOWNER};
static constexpr Capabilities kCapFsetid{.mask_ = 1ULL << CAP_FSETID};
static constexpr Capabilities kCapKill{.mask_ = 1ULL << CAP_KILL};
static constexpr Capabilities kCapSetgid{.mask_ = 1ULL << CAP_SETGID};
static constexpr Capabilities kCapSetuid{.mask_ = 1ULL << CAP_SETUID};
static constexpr Capabilities kCapSetpcap{.mask_ = 1ULL << CAP_SETPCAP};
static constexpr Capabilities kCapLinuxImmutable{.mask_ = 1ULL << CAP_LINUX_IMMUTABLE};
static constexpr Capabilities kCapNetBindService{.mask_ = 1ULL << CAP_NET_BIND_SERVICE};
static constexpr Capabilities kCapNetBroadcast{.mask_ = 1ULL << CAP_NET_BROADCAST};
static constexpr Capabilities kCapNetAdmin{.mask_ = 1ULL << CAP_NET_ADMIN};
static constexpr Capabilities kCapNetRaw{.mask_ = 1ULL << CAP_NET_RAW};
static constexpr Capabilities kCapIpcLock{.mask_ = 1ULL << CAP_IPC_LOCK};
static constexpr Capabilities kCapIpcOwner{.mask_ = 1ULL << CAP_IPC_OWNER};
static constexpr Capabilities kCapSysModule{.mask_ = 1ULL << CAP_SYS_MODULE};
static constexpr Capabilities kCapSysRawio{.mask_ = 1ULL << CAP_SYS_RAWIO};
static constexpr Capabilities kCapSysChroot{.mask_ = 1ULL << CAP_SYS_CHROOT};
static constexpr Capabilities kCapSysPtrace{.mask_ = 1ULL << CAP_SYS_PTRACE};
static constexpr Capabilities kCapSysPacct{.mask_ = 1ULL << CAP_SYS_PACCT};
static constexpr Capabilities kCapSysAdmin{.mask_ = 1ULL << CAP_SYS_ADMIN};
static constexpr Capabilities kCapSysBoot{.mask_ = 1ULL << CAP_SYS_BOOT};
static constexpr Capabilities kCapSysNice{.mask_ = 1ULL << CAP_SYS_NICE};
static constexpr Capabilities kCapSysResource{.mask_ = 1ULL << CAP_SYS_RESOURCE};
static constexpr Capabilities kCapSysTime{.mask_ = 1ULL << CAP_SYS_TIME};
static constexpr Capabilities kCapSysTtyConfig{.mask_ = 1ULL << CAP_SYS_TTY_CONFIG};
static constexpr Capabilities kCapMknod{.mask_ = 1ULL << CAP_MKNOD};
static constexpr Capabilities kCapLease{.mask_ = 1ULL << CAP_LEASE};
static constexpr Capabilities kCapAuditWrite{.mask_ = 1ULL << CAP_AUDIT_WRITE};
static constexpr Capabilities kCapAuditControl{.mask_ = 1ULL << CAP_AUDIT_CONTROL};
static constexpr Capabilities kCapSetfcap{.mask_ = 1ULL << CAP_SETFCAP};
static constexpr Capabilities kCapMacOverride{.mask_ = 1ULL << CAP_MAC_OVERRIDE};
static constexpr Capabilities kCapMacAdmin{.mask_ = 1ULL << CAP_MAC_ADMIN};
static constexpr Capabilities kCapSyslog{.mask_ = 1ULL << CAP_SYSLOG};
static constexpr Capabilities kCapWakeAlarm{.mask_ = 1ULL << CAP_WAKE_ALARM};
static constexpr Capabilities kCapBlockSuspend{.mask_ = 1ULL << CAP_BLOCK_SUSPEND};
static constexpr Capabilities kCapAuditRead{.mask_ = 1ULL << CAP_AUDIT_READ};
static constexpr Capabilities kCapPerfmon{.mask_ = 1ULL << CAP_PERFMON};
static constexpr Capabilities kCapBpf{.mask_ = 1ULL << CAP_BPF};
static constexpr Capabilities kCapCheckpointRestore{.mask_ = 1ULL << CAP_CHECKPOINT_RESTORE};

enum class SecureBitsEnum : uint32_t {
  KEEP_CAPS = 1 << SECURE_KEEP_CAPS,
  KEEP_CAPS_LOCKED = 1 << SECURE_KEEP_CAPS_LOCKED,
  NO_SETUID_FIXUP = 1 << SECURE_NO_SETUID_FIXUP,
  NO_SETUID_FIXUP_LOCKED = 1 << SECURE_NO_SETUID_FIXUP_LOCKED,
  NOROOT = 1 << SECURE_NOROOT,
  NOROOT_LOCKED = 1 << SECURE_NOROOT_LOCKED,
  NO_CAP_AMBIENT_RAISE = 1 << SECURE_NO_CAP_AMBIENT_RAISE,
  NO_CAP_AMBIENT_RAISE_LOCKED = 1 << SECURE_NO_CAP_AMBIENT_RAISE_LOCKED
};

using SecureBits = Flags<SecureBitsEnum>;

struct Credentials {
  uid_t uid_;
  gid_t gid_;
  uid_t euid_;
  gid_t egid_;
  uid_t saved_uid_;
  gid_t saved_gid_;
  fbl::Vector<gid_t> groups_;

  // See https://man7.org/linux/man-pages/man2/setfsuid.2.html
  uid_t fsuid_;

  // See https://man7.org/linux/man-pages/man2/setfsgid.2.html
  gid_t fsgid_;

  /// From https://man7.org/linux/man-pages/man7/capabilities.7.html
  ///
  /// > This is a limiting superset for the effective capabilities that the thread may assume. It
  /// > is also a limiting superset for the capabilities that may be added to the inheritable set
  /// > by a thread that does not have the CAP_SETPCAP capability in its effective set.
  ///
  /// > If a thread drops a capability from its permitted set, it can never reacquire that
  /// > capability (unless it execve(2)s either a set-user-ID-root program, or a program whose
  /// > associated file capabilities grant that capability).
  Capabilities cap_permitted_;

  /// From https://man7.org/linux/man-pages/man7/capabilities.7.html
  ///
  /// > This is the set of capabilities used by the kernel to perform permission checks for the
  /// > thread.
  Capabilities cap_effective_;

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
  Capabilities cap_inheritable_;

  /// From https://man7.org/linux/man-pages/man7/capabilities.7.html
  ///
  /// > The capability bounding set is a mechanism that can be used to limit the capabilities that
  /// > are gained during execve(2).
  ///
  /// > Since Linux 2.6.25, this is a per-thread capability set. In older kernels, the capability
  /// > bounding set was a system wide attribute shared by all threads on the system.
  Capabilities cap_bounding_;

  /// From https://man7.org/linux/man-pages/man7/capabilities.7.html
  ///
  /// > This is a set of capabilities that are preserved across an execve(2) of a program that is
  /// > not privileged.  The ambient capability set obeys the invariant that no capability can
  /// > ever be ambient if it is not both permitted and inheritable.
  ///
  /// > Executing a program that changes UID or GID due to the set-user-ID or set-group-ID bits
  /// > or executing a program that has any file capabilities set will clear the ambient set.
  Capabilities cap_ambient_;

  /// From https://man7.org/linux/man-pages/man7/capabilities.7.html
  ///
  /// > Starting with kernel 2.6.26, and with a kernel in which file capabilities are enabled,
  /// > Linux implements a set of per-thread securebits flags that can be used to disable special
  /// > handling of capabilities for UID 0 (root).
  ///
  /// > The securebits flags can be modified and retrieved using the prctl(2)
  /// > PR_SET_SECUREBITS and PR_GET_SECUREBITS operations.  The CAP_SETPCAP capability is
  /// > required to modify the flags.
  SecureBits securebits_;

  /// impl Credentials

  // Creates a set of credentials with all possible permissions and capabilities.
  static Credentials root() { return with_ids(0, 0); }

  /// Creates a set of credentials with the given uid and gid. If the uid is 0, the credentials
  /// will grant superuser access.
  static Credentials with_ids(uid_t uid, gid_t gid) {
    auto caps = uid == 0 ? Capabilities::all() : Capabilities::empty();
    return Credentials(uid, gid, uid, gid, uid, gid, fbl::Vector<gid_t>(), uid, gid, caps, caps,
                       Capabilities::empty(), Capabilities::all(), Capabilities::empty(),
                       SecureBits::empty());
  }

  /// Compares the user ID of `self` to that of `other`.
  ///
  /// Used to check whether a task can signal another.
  ///
  /// From https://man7.org/linux/man-pages/man2/kill.2.html:
  ///
  /// > For a process to have permission to send a signal, it must either be
  /// > privileged (under Linux: have the CAP_KILL capability in the user
  /// > namespace of the target process), or the real or effective user ID of
  /// > the sending process must equal the real or saved set- user-ID of the
  /// > target process.
  ///
  /// Returns true if the credentials are considered to have the same user ID.
  bool has_same_uid(const Credentials& other) const {
    return euid_ == other.saved_uid_ || euid_ == other.uid_ || uid_ == other.uid_ ||
           uid_ == other.saved_uid_;
  }

  bool is_superuser() const { return euid_ == 0; }

  bool is_in_group(gid_t gid) const {
    return egid_ == gid || std::ranges::find(groups_, gid) != groups_.end();
  }

  fit::result<Errno> check_access(starnix_uapi::Access access, uid_t node_uid, gid_t node_gid,
                                  FileMode mode) const {
    auto mode_bits = mode.bits();
    uint32_t mode_rwx_bits;
    if (has_capability(kCapDacOverride)) {
      if (mode.is_dir()) {
        mode_rwx_bits = 0007;
      } else {
        // At least one of the EXEC bits must be set to execute files.
        mode_rwx_bits =
            0006 | ((mode_bits & 0100) >> 6) | ((mode_bits & 0010) >> 3) | (mode_bits & 0001);
      }
    } else if (fsuid_ == node_uid) {
      mode_rwx_bits = (mode_bits & 0700) >> 6;
    } else if (is_in_group(node_gid)) {
      mode_rwx_bits = (mode_bits & 0070) >> 3;
    } else {
      mode_rwx_bits = mode_bits & 0007;
    }

    if ((mode_rwx_bits & access.rwx_bits()) != access.rwx_bits()) {
      return fit::error(errno(EACCES));
    }
    return fit::ok();
  }

  /// Returns whether or not the task has the given capability.
  bool has_capability(Capabilities capability) const { return cap_effective_.contains(capability); }

  FsCred as_fscred() const { return {.uid_ = fsuid_, .gid_ = fsgid_}; }

  // C++
  Credentials() : securebits_(SecureBits::empty()) {}

  Credentials(uid_t uid, gid_t gid, uid_t euid, gid_t egid, uid_t saved_uid, gid_t saved_gid,
              fbl::Vector<gid_t> groups, uid_t fsuid, gid_t fsgid, Capabilities cap_permitted,
              Capabilities cap_effective, Capabilities cap_inheritable, Capabilities cap_bounding,
              Capabilities cap_ambient, SecureBits securebits)
      : uid_(uid),
        gid_(gid),
        euid_(euid),
        egid_(egid),
        saved_uid_(saved_uid),
        saved_gid_(saved_gid),
        fsuid_(fsuid),
        fsgid_(fsgid),
        cap_permitted_(cap_permitted),
        cap_effective_(cap_effective),
        cap_inheritable_(cap_inheritable),
        cap_bounding_(cap_bounding),
        cap_ambient_(cap_ambient),
        securebits_(securebits) {
    fbl::AllocChecker ac;
    groups_.reserve(groups.size(), &ac);
    ZX_ASSERT(ac.check());
    for (const auto& group : groups) {
      groups_.push_back(group, &ac);
      ZX_ASSERT(ac.check());
    }
  }

  Credentials(const Credentials& other)
      : uid_(other.uid_),
        gid_(other.gid_),
        euid_(other.euid_),
        egid_(other.egid_),
        saved_uid_(other.saved_uid_),
        saved_gid_(other.saved_gid_),
        fsuid_(other.fsuid_),
        fsgid_(other.fsgid_),
        cap_permitted_(other.cap_permitted_),
        cap_effective_(other.cap_effective_),
        cap_inheritable_(other.cap_inheritable_),
        cap_bounding_(other.cap_bounding_),
        cap_ambient_(other.cap_ambient_),
        securebits_(other.securebits_) {
    fbl::AllocChecker ac;
    groups_.reserve(other.groups_.size(), &ac);
    ZX_ASSERT(ac.check());
    for (const auto& group : other.groups_) {
      groups_.push_back(group, &ac);
      ZX_ASSERT(ac.check());
    }
  }

  Credentials& operator=(const Credentials& other) {
    if (this != &other) {
      uid_ = other.uid_;
      gid_ = other.gid_;
      euid_ = other.euid_;
      egid_ = other.egid_;
      saved_uid_ = other.saved_uid_;
      saved_gid_ = other.saved_gid_;
      fsuid_ = other.fsuid_;
      fsgid_ = other.fsgid_;
      cap_permitted_ = other.cap_permitted_;
      cap_effective_ = other.cap_effective_;
      cap_inheritable_ = other.cap_inheritable_;
      cap_bounding_ = other.cap_bounding_;
      cap_ambient_ = other.cap_ambient_;
      securebits_ = other.securebits_;

      fbl::AllocChecker ac;
      groups_.reset();
      groups_.reserve(other.groups_.size(), &ac);
      ZX_ASSERT(ac.check());
      for (const auto& group : other.groups_) {
        groups_.push_back(group, &ac);
        ZX_ASSERT(ac.check());
      }
    }
    return *this;
  }
};

}  // namespace starnix_uapi

template <>
constexpr Flag<starnix_uapi::SecureBitsEnum> Flags<starnix_uapi::SecureBitsEnum>::FLAGS[] = {
    {starnix_uapi::SecureBitsEnum::KEEP_CAPS},
    {starnix_uapi::SecureBitsEnum::KEEP_CAPS_LOCKED},
    {starnix_uapi::SecureBitsEnum::NO_SETUID_FIXUP},
    {starnix_uapi::SecureBitsEnum::NO_SETUID_FIXUP_LOCKED},
    {starnix_uapi::SecureBitsEnum::NOROOT},
    {starnix_uapi::SecureBitsEnum::NOROOT_LOCKED},
    {starnix_uapi::SecureBitsEnum::NO_CAP_AMBIENT_RAISE},
    {starnix_uapi::SecureBitsEnum::NO_CAP_AMBIENT_RAISE_LOCKED},
};

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_AUTH_H_
