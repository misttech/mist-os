// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_JOB_POLICY_H_
#define ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_JOB_POLICY_H_

#include <stdint.h>
#include <zircon/syscalls/policy.h>
#include <zircon/types.h>

#include <kernel/timer.h>
#include <ktl/array.h>
#include <ktl/bit.h>
#include <ktl/numeric.h>
#include <ktl/tuple.h>

constexpr size_t kJobPolicyLastPolicy = ZX_POL_MAX - 1;
// An additional bit for policy override.
constexpr size_t kJobPolicyBitsPerPolicy = ktl::bit_width(ZX_POL_ACTION_MAX - 1) + 1;
constexpr size_t kJobPolicyBitsPerBucket = std::numeric_limits<uint64_t>::digits;
constexpr size_t kJobPolicyJobPolicyBuckets =
    fbl::round_up(kJobPolicyLastPolicy * kJobPolicyBitsPerPolicy, kJobPolicyBitsPerBucket) /
    kJobPolicyBitsPerBucket;

// This class provides a the encoded bit fields of each policy, by performing proper
// offsetting into the underlying storage bucket and bit range.
//
// JobPolicyCollection objects are thread-compatible. In particular, const instances are immutable.
// This guarantee is important because const instances of JobPolicy may be accessed concurrently by
// multiple threads without any synchronization.
class JobPolicyCollection {
  using Storage = ktl::array<uint64_t, kJobPolicyJobPolicyBuckets>;

 public:
  // Individual policy bits.
  class Policy {
   public:
    constexpr uint32_t action() const {
      auto [bucket, bit_offset] = policy_range();
      return static_cast<uint32_t>(((storage_[bucket] >> (bit_offset)) & kActionMask) >> 1);
    }

    constexpr void set_action(uint32_t action) {
      ZX_ASSERT_MSG(action < ZX_POL_ACTION_MAX,
                    "Attempting to set policy entry(%zu)'s action to %u and max is %u",
                    policy_offset_ / kJobPolicyBitsPerPolicy, action, ZX_POL_ACTION_MAX);

      auto [bucket, bit_offset] = policy_range();
      // `action` must be set to the three most significant bits of the 4 bit bitfield.
      storage_[bucket] = (storage_[bucket] & ~(kActionMask << bit_offset)) |
                         (static_cast<uint64_t>(action) << (bit_offset + 1));
    }

    constexpr bool override() const {
      auto [bucket, bit_offset] = policy_range();
      // Bit is flipped such that default value is 0.
      return !static_cast<bool>((storage_[bucket] >> (bit_offset)) & kOverrideMask);
    }

    constexpr void set_override(bool override) {
      // Override bit can either be allow(0) or deny(1), so the flag is flipped for transforming
      // into the proper bit flag.
      auto [bucket, bit_offset] = policy_range();
      // Bit is flipped such that default value is 0.
      storage_[bucket] = (storage_[bucket] & ~(kOverrideMask << bit_offset)) |
                         (static_cast<uint64_t>(!override) << (bit_offset));
    }

   private:
    friend JobPolicyCollection;

    // Enforce the invariant of 4 bits per policy.
    static_assert(kJobPolicyBitsPerPolicy == 4, "Must update `kActionMask` and `kOverrideMask`.");

    static constexpr uint64_t kPolicyBitMask = (uint64_t{1} << (kJobPolicyBitsPerPolicy)) - 1;
    static constexpr uint64_t kOverrideMask = 0x1;
    static constexpr uint64_t kActionMask = kPolicyBitMask & ~kOverrideMask;

    constexpr Policy(Storage& storage, size_t policy_offset)
        : storage_(storage), policy_offset_(policy_offset) {}

    constexpr ktl::tuple<uint64_t, uint64_t> policy_range() const {
      return {policy_offset_ / kJobPolicyBitsPerBucket, policy_offset_ % kJobPolicyBitsPerBucket};
    }

    Storage& storage_;
    size_t policy_offset_ = 0;
  };

  bool operator==(const JobPolicyCollection& other) const { return storage_ == other.storage_; }
  bool operator!=(const JobPolicyCollection& other) const { return !operator==(other); }

  constexpr Policy operator[](size_t policy) {
    ZX_ASSERT_MSG(policy < ZX_POL_MAX, "Attempting to retrieve policy entry for %zu and max is %u",
                  policy, ZX_POL_MAX);

    // This is a synthetic policy, that doesn't require bits.
    ZX_ASSERT(policy != ZX_POL_NEW_ANY);
    // `ZX_POL_NEW_ANY` is a synthetic policy in terms of required storage. Its job is to apply the
    // policy action and override to ALL new object policies.
    if (policy > ZX_POL_NEW_ANY) {
      policy--;
    }
    return Policy{storage_, policy * kJobPolicyBitsPerPolicy};
  }

 private:
  Storage storage_ = {};
};

// JobPolicy is a value type that provides a space-efficient encoding of the policies defined in the
// policy.h public header.
//
// JobPolicy encodes two kinds of policy, basic and timer slack.
//
// Basic policy is logically an array of zx_policy_basic elements. For example:
//
//   zx_policy_basic policy[] = {
//      { ZX_POL_BAD_HANDLE, ZX_POL_ACTION_KILL },
//      { ZX_POL_NEW_CHANNEL, ZX_POL_ACTION_ALLOW },
//      { ZX_POL_NEW_FIFO, ZX_POL_ACTION_ALLOW_EXCEPTION },
//      { ZX_POL_VMAR_WX, ZX_POL_ACTION_KILL }}
//
// Timer slack policy defines the type and minimum amount of slack that will be applied to timer
// and deadline events.
class JobPolicy {
 public:
  JobPolicy() = delete;
  JobPolicy(const JobPolicy& parent);
  static JobPolicy CreateRootPolicy();

  // Merge array |policy| of length |count| into this object.
  //
  // |mode| controls what happens when the policies in |policy| and this object intersect. |mode|
  // must be one of:
  //
  // ZX_JOB_POL_RELATIVE - Conflicting policies are ignored and will not cause the call to fail.
  //
  // ZX_JOB_POL_ABSOLUTE - If any of the policies in |policy| conflict with those in this object,
  //   the call will fail with an error and this object will not be modified.
  //
  zx_status_t AddBasicPolicy(uint32_t mode, const zx_policy_basic_v2_t* policy, size_t count);

  // Returns the action (e.g. ZX_POL_ACTION_ALLOW) for the specified |condition|.
  //
  // This method asserts if |policy| is invalid, and returns ZX_POL_ACTION_DENY for all other
  // failure modes.
  uint32_t QueryBasicPolicy(uint32_t condition) const;

  // Returns if the action for the specified condition can be overriden, so it returns
  // ZX_POL_OVERRIDE_ALLOW or ZX_POL_OVERRIDE_DENY.
  uint32_t QueryBasicPolicyOverride(uint32_t condition) const;

  // Sets the timer slack policy.
  //
  // |slack.amount| must be >= 0.
  void SetTimerSlack(TimerSlack slack);

  // Returns the timer slack policy.
  TimerSlack GetTimerSlack() const;

  bool operator==(const JobPolicy& rhs) const;
  bool operator!=(const JobPolicy& rhs) const;

  // Increment the kcounter for the given |action| and |condition|.
  //
  // action must be < ZX_POL_ACTION_MAX and condition must be < ZX_POL_MAX.
  //
  // For example: IncrementCounter(ZX_POL_ACTION_KILL, ZX_POL_NEW_CHANNEL);
  static void IncrementCounter(uint32_t action, uint32_t condition);

 private:
  JobPolicy(JobPolicyCollection collection, const TimerSlack& slack);
  // Remember, JobPolicy is a value type so think carefully before increasing its size.
  //
  // Const instances of JobPolicy must be immutable to ensure thread-safety.
  JobPolicyCollection collection_;
  TimerSlack slack_{TimerSlack::none()};
};

#endif  // ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_JOB_POLICY_H_
