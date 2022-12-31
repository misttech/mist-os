// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zxdump/task.h>
#include <zircon/assert.h>

#ifdef __Fuchsia__
#include <lib/zx/job.h>
#include <lib/zx/process.h>
#include <lib/zx/thread.h>
#endif

#include "rights.h"

namespace zxdump {
namespace {

#ifdef __Fuchsia__
using LiveJob = zx::job;
using LiveProcess = zx::process;
#else
using LiveJob = LiveHandle;
using LiveProcess = LiveHandle;
#endif

constexpr size_t kMaxPropertySize = ZX_MAX_NAME_LEN;

constexpr Error kTaskNotFound = {
    .op_ = "task KOID not found",
    .status_ = ZX_ERR_NOT_FOUND,
};

template <typename T, T zx_info_handle_basic_t::*Member>
T GetHandleBasicInfo(const std::map<zx_object_info_topic_t, ByteView>& info) {
  if (auto found = info.find(ZX_INFO_HANDLE_BASIC); found != info.end()) {
    auto [topic, data] = *found;
    zx_info_handle_basic_t info;
    ZX_ASSERT(data.size() >= sizeof(info));
    memcpy(&info, data.data(), sizeof(info));
    return info.*Member;
  }
  // Only the superroot has no cached basic info.  It's a special case.
  return T{};
}

}  // namespace

zx_koid_t Object::koid() const {
  return GetHandleBasicInfo<zx_koid_t, &zx_info_handle_basic_t::koid>(info_);
}

zx_obj_type_t Object::type() const {
  return GetHandleBasicInfo<zx_obj_type_t, &zx_info_handle_basic_t::type>(info_);
}

fit::result<Error, ByteView> Object::get_info(zx_object_info_topic_t topic, size_t record_size) {
  if (info_.empty()) {
    // Only the superroot has no cached info at all.  It's a special case.
    return GetSuperrootInfo(topic);
  }
  auto found = info_.find(topic);
  if (found == info_.end()) {
    if (!live_) {
      return fit::error(Error{"zx_object_get_info", ZX_ERR_NOT_SUPPORTED});
    }

    // This interface cannot be transparently proxied!  We can always come
    // up with a buffer size that's big enough just by trying bigger sizes.
    // But short of searching the space of sizes empirically with get_info
    // attempts, there is no way to know what the correct exact size was.
    // The call can return a count of the amount of data that's actually
    // available, but only as a count of records, not a count of bytes.
    // The size of each record is just implicit in the topic.
    std::unique_ptr<std::byte[]> buffer;
    zx_status_t status;
    size_t actual = 0, avail = 0;
    size_t size = record_size == 0 ? sizeof(zx_info_handle_basic_t) / 2 : 0;
    do {
      if (record_size != 0 && record_size * avail > size) {
        size = record_size * avail;
      } else {
        size *= 2;
      }
      buffer = std::make_unique<std::byte[]>(size);
      status = live_.get_info(topic, buffer.get(), size, &actual, &avail);
    } while (status == ZX_ERR_BUFFER_TOO_SMALL || actual < avail);
    if (status != ZX_OK) {
      return fit::error(Error{"zx_object_get_info", status});
    }

    ByteView data{buffer.get(), size};
    if (found == info_.end()) {
      auto [it, unique] = info_.emplace(topic, data);
      ZX_DEBUG_ASSERT(unique);
      found = it;
    } else {
      found->second = data;
    }

    TakeBuffer(std::move(buffer));
  }
  return fit::ok(found->second);
}

fit::result<Error, ByteView> Object::get_property(uint32_t property) {
  auto found = properties_.find(property);
  if (found == properties_.end()) {
    if (!live_) {
      return fit::error(Error{"zx_object_get_property", ZX_ERR_NOT_SUPPORTED});
    }
    auto buffer = GetBuffer(kMaxPropertySize);
    if (zx_status_t status = live_.get_property(property, buffer, kMaxPropertySize);
        status != ZX_OK) {
      ZX_ASSERT(status != ZX_ERR_BUFFER_TOO_SMALL);
      return fit::error(Error{"zx_object_get_property", status});
    }
    auto [it, unique] = properties_.emplace(property, ByteView{buffer, kMaxPropertySize});
    ZX_DEBUG_ASSERT(unique);
    found = it;
  }
  return fit::ok(found->second);
}

fit::result<Error, ByteView> Thread::read_state(zx_thread_state_topic_t topic) {
  auto found = state_.find(topic);
  if (found == state_.end()) {
    return fit::error(Error{"zx_thread_read_state", ZX_ERR_NOT_SUPPORTED});
  }
  return fit::ok(found->second);
}

fit::result<Error, std::reference_wrapper<Object>> Object::get_child(zx_koid_t koid) {
  switch (type()) {
    case ZX_OBJ_TYPE_JOB:
      return static_cast<Job*>(this)->get_child(koid);
    case ZX_OBJ_TYPE_PROCESS:
      return static_cast<Process*>(this)->get_child(koid);
    default:
      return fit::error{Error{"zx_object_get_child", ZX_ERR_NOT_FOUND}};
  }
}

fit::result<Error, std::reference_wrapper<Object>> Object::find(zx_koid_t match) {
  if (koid() == match) {
    return fit::ok(std::ref(*this));
  }
  switch (this->type()) {
    case ZX_OBJ_TYPE_JOB:
      return static_cast<Job*>(this)->find(match);
    case ZX_OBJ_TYPE_PROCESS:
      return static_cast<Process*>(this)->find(match);
  }
  return fit::error{kTaskNotFound};
}

fit::result<Error, std::reference_wrapper<Task>> Job::find(zx_koid_t match) {
  if (koid() == match) {
    return fit::ok(std::ref(*this));
  }

  // First check our immediate child tasks.
  if (auto result = get_child(match); result.is_ok()) {
    Object& child = result.value();
    return fit::ok(std::ref(static_cast<Task&>(child)));
  } else if (result.error_value().status_ != ZX_ERR_NOT_FOUND) {
    return result.take_error();
  }

  // For a live job, processes() actively fills the processes_ list.
  if (auto result = processes(); result.is_error()) {
    return result.take_error();
  }

  // Recurse on the child processes.
  for (auto& [koid, process] : processes_) {
    ZX_DEBUG_ASSERT(koid != match || live());
    auto result = process.find(match);
    if (result.is_ok()) {
      return result;
    }
  }

  // For a live job, children() actively fills the children_ list.
  if (auto result = children(); result.is_error()) {
    return result.take_error();
  }

  // Recurse on the child jobs.
  for (auto& [koid, job] : children_) {
    ZX_DEBUG_ASSERT(koid != match || live());
    auto result = job.find(match);
    if (result.is_ok()) {
      return result;
    }
  }

  return fit::error{kTaskNotFound};
}

fit::result<Error, std::reference_wrapper<Task>> Process::find(zx_koid_t match) {
  if (koid() == match) {
    return fit::ok(std::ref(*this));
  }

  auto result = get_child(match);
  if (result.is_error()) {
    return result.take_error();
  }

  Object& thread = result.value();
  ZX_ASSERT(thread.type() == ZX_OBJ_TYPE_THREAD);
  return fit::ok(std::ref(static_cast<Thread&>(thread)));
}

}  // namespace zxdump
