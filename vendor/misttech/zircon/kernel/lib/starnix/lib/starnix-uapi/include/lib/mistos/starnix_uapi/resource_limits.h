// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_RESOURCE_LIMITS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_RESOURCE_LIMITS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix_uapi/errors.h>

#include <fbl/intrusive_hash_table.h>
#include <ktl/array.h>
#include <ktl/move.h>
#include <ktl/optional.h>
#include <ktl/string_view.h>
#include <ktl/unique_ptr.h>

#include <linux/fs.h>
#include <linux/mqueue.h>
#include <linux/resource.h>

namespace starnix_uapi {

// A description of a resource.
struct ResourceDesc {
  // The name of the resource.
  ktl::string_view name;

  // The units in which limits on the resource are expressed.
  ktl::string_view unit;
};

enum class ResourceEnum : uint32_t {
  CPU,
  FSIZE,
  DATA,
  STACK,
  CORE,
  RSS,
  NPROC,
  NOFILE,
  MEMLOCK,
  AS,
  LOCKS,
  SIGPENDING,
  MSGQUEUE,
  NICE,
  RTPRIO,
  RTTIME,
};

struct Resource {
  constexpr static ktl::array<ResourceEnum, 16> ALL = {
      ResourceEnum::CPU,      ResourceEnum::FSIZE, ResourceEnum::DATA,   ResourceEnum::STACK,
      ResourceEnum::CORE,     ResourceEnum::RSS,   ResourceEnum::NPROC,  ResourceEnum::NOFILE,
      ResourceEnum::MEMLOCK,  ResourceEnum::AS,    ResourceEnum::LOCKS,  ResourceEnum::SIGPENDING,
      ResourceEnum::MSGQUEUE, ResourceEnum::NICE,  ResourceEnum::RTPRIO, ResourceEnum::RTTIME};

  static fit::result<Errno, ResourceEnum> from_raw(uint32_t raw) {
    switch (raw) {
      case RLIMIT_CPU:
        return fit::ok(ResourceEnum::CPU);
      case RLIMIT_FSIZE:
        return fit::ok(ResourceEnum::FSIZE);
      case RLIMIT_DATA:
        return fit::ok(ResourceEnum::DATA);
      case RLIMIT_STACK:
        return fit::ok(ResourceEnum::STACK);
      case RLIMIT_CORE:
        return fit::ok(ResourceEnum::CORE);
      case RLIMIT_RSS:
        return fit::ok(ResourceEnum::RSS);
      case RLIMIT_NPROC:
        return fit::ok(ResourceEnum::NPROC);
      case RLIMIT_NOFILE:
        return fit::ok(ResourceEnum::NOFILE);
      case RLIMIT_MEMLOCK:
        return fit::ok(ResourceEnum::MEMLOCK);
      case RLIMIT_AS:
        return fit::ok(ResourceEnum::AS);
      case RLIMIT_LOCKS:
        return fit::ok(ResourceEnum::LOCKS);
      case RLIMIT_SIGPENDING:
        return fit::ok(ResourceEnum::SIGPENDING);
      case RLIMIT_MSGQUEUE:
        return fit::ok(ResourceEnum::MSGQUEUE);
      case RLIMIT_NICE:
        return fit::ok(ResourceEnum::NICE);
      case RLIMIT_RTPRIO:
        return fit::ok(ResourceEnum::RTPRIO);
      case RLIMIT_RTTIME:
        return fit::ok(ResourceEnum::RTTIME);
      default:
        return fit::error(errno(EINVAL));
    }
  }

  ResourceDesc desc() {
    switch (value) {
      case ResourceEnum::CPU:
        return {.name = "Max cpu time"sv, .unit = "seconds"sv};
      case ResourceEnum::FSIZE:
        return {.name = "Max file size"sv, .unit = "bytes"sv};
      case ResourceEnum::DATA:
        return {.name = "Max data size"sv, .unit = "bytes"sv};
      case ResourceEnum::STACK:
        return {.name = "Max stack size"sv, .unit = "bytes"sv};
      case ResourceEnum::CORE:
        return {.name = "Max core file size"sv, .unit = "bytes"sv};
      case ResourceEnum::RSS:
        return {.name = "Max resident set"sv, .unit = "bytes"sv};
      case ResourceEnum::NPROC:
        return {.name = "Max processes"sv, .unit = "processes"sv};
      case ResourceEnum::NOFILE:
        return {.name = "Max open files"sv, .unit = "files"sv};
      case ResourceEnum::MEMLOCK:
        return {.name = "Max locked memory"sv, .unit = "bytes"sv};
      case ResourceEnum::AS:
        return {.name = "Max address space"sv, .unit = "bytes"sv};
      case ResourceEnum::LOCKS:
        return {.name = "Max file locks"sv, .unit = "bytes"sv};
      case ResourceEnum::SIGPENDING:
        return {.name = "Max pending signals"sv, .unit = "signals"sv};
      case ResourceEnum::MSGQUEUE:
        return {.name = "Max msgqueue size"sv, .unit = "bytes"sv};
      case ResourceEnum::NICE:
        return {.name = "Max nice priority"sv, .unit = ""sv};
      case ResourceEnum::RTPRIO:
        return {.name = "Max realtime priority"sv, .unit = ""sv};
      case ResourceEnum::RTTIME:
        return {.name = "Max realtime timeout"sv, .unit = "us"sv};
    }
  }

  bool operator==(const Resource& other) const { return value == other.value; }
  bool operator!=(const Resource& other) const { return value != other.value; }

  ResourceEnum value;
};

// Define INFINITE_LIMIT and other constants
const rlimit INFINITE_LIMIT = {.rlim_cur = RLIM_INFINITY, .rlim_max = RLIM_INFINITY};

// Most default limit values are the same that are used in GVisor, see
// https://github.com/google/gvisor/blob/master/pkg/abi/linux/limits.go .

const unsigned long NPROC_LIMIT = 0x1FFFFFFF;

// GVisor sets defaults for `SIGPENDING` to 0, but that's incorrect since it would block all
// real-time signals. Set it to `max_threads / 2` (same as `NPROC_LIMIT`).
const unsigned long SIGPENDING_LIMIT = 0x1FFFFFFF;

// Define DEFAULT_LIMITS as std::array
const std::array<std::pair<Resource, rlimit>, 7> DEFAULT_LIMITS = {
    {{{ResourceEnum::STACK}, {.rlim_cur = _STK_LIM, .rlim_max = RLIM_INFINITY}},
     {{ResourceEnum::CORE}, {.rlim_cur = 0, .rlim_max = RLIM_INFINITY}},
     {{ResourceEnum::NPROC}, {.rlim_cur = NPROC_LIMIT, .rlim_max = NPROC_LIMIT}},
     {{ResourceEnum::NOFILE}, {.rlim_cur = INR_OPEN_CUR, .rlim_max = INR_OPEN_MAX}},
     {{ResourceEnum::MEMLOCK}, {.rlim_cur = MLOCK_LIMIT, .rlim_max = MLOCK_LIMIT}},
     {{ResourceEnum::SIGPENDING}, {.rlim_cur = SIGPENDING_LIMIT, .rlim_max = SIGPENDING_LIMIT}},
     {{ResourceEnum::MSGQUEUE}, {.rlim_cur = MQ_BYTES_MAX, .rlim_max = MQ_BYTES_MAX}}}};

class ResourceLimits {
 public:
  ResourceLimits() {
    for (auto& [resouce, limit] : DEFAULT_LIMITS) {
      fbl::AllocChecker ac;
      ktl::unique_ptr<Hashable> hashable(new (&ac) Hashable{});
      ZX_ASSERT(ac.check());
      hashable->key_ = resouce;
      hashable->value_ = limit;
      values_.insert(ktl::move(hashable));
    }
  }

  ResourceLimits& operator=(const ResourceLimits& other) {
    // Manually insert each element from the original to the copy
    for (const auto& pair : other.values_) {
      fbl::AllocChecker ac;
      ktl::unique_ptr<Hashable> hashable(new (&ac) Hashable{});
      ZX_ASSERT(ac.check());
      hashable->key_ = pair.key_;
      hashable->value_ = pair.value_;
      values_.insert_or_replace(ktl::move(hashable));
    }
    return *this;
  }

  rlimit get(Resource resource) const {
    auto it = values_.find(resource);
    if (it != values_.end()) {
      return (*it).value_;
    } else {
      return INFINITE_LIMIT;
    }
  }

  void set(Resource resource, rlimit value) {
    fbl::AllocChecker ac;
    ktl::unique_ptr<Hashable> hashable(new (&ac) Hashable{});
    ZX_ASSERT(ac.check());
    hashable->key_ = resource;
    hashable->value_ = value;
    values_.insert_or_replace(std::move(hashable));
  }

  fit::result<Errno, rlimit> get_and_set(Resource resource, ktl::optional<rlimit> maybe_new_limit,
                                         bool can_increase_rlimit) {
    auto old_limit = get(resource);
    if (maybe_new_limit.has_value()) {
      auto new_limit = maybe_new_limit.value();
      if (new_limit.rlim_max > old_limit.rlim_max && !can_increase_rlimit) {
        return fit::error(errno(EPERM));
      }
      fbl::AllocChecker ac;
      ktl::unique_ptr<Hashable> hashable(new (&ac) Hashable{});
      ZX_ASSERT(ac.check());
      hashable->key_ = resource;
      hashable->value_ = new_limit;
      values_.insert_or_replace(std::move(hashable));
    }
    return fit::ok(old_limit);
  }

 private:
  // An intrusive data structure wrapping a rlimit, required be stored
  // in a fbl::HashTable.
  //
  struct Hashable : public fbl::SinglyLinkedListable<ktl::unique_ptr<Hashable>> {
    // Required to instantiate fbl::DefaultKeyedObjectTraits.
    Resource GetKey() const { return key_; }

    // Required to instantiate fbl::DefaultHashTraits.
    static size_t GetHash(Resource key) { return static_cast<size_t>(key.value); }

    Resource key_;
    rlimit value_;
  };

  fbl::HashTable<Resource, ktl::unique_ptr<Hashable>> values_;
};

}  // namespace starnix_uapi

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_RESOURCE_LIMITS_H_
