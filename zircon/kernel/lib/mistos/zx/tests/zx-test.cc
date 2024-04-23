// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <assert.h>
#include <lib/fit/defer.h>
#include <lib/mistos/zx/debuglog.h>
#include <lib/mistos/zx/event.h>
#include <lib/mistos/zx/job.h>
#include <lib/mistos/zx/object.h>
#include <lib/mistos/zx/process.h>
#include <lib/mistos/zx/resource.h>
#include <lib/mistos/zx/thread.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/limits.h>
#include <zircon/types.h>

#include <utility>

#include <object/job_dispatcher.h>
#include <vm/vm_object.h>
#include <zxtest/zxtest.h>

namespace {

template <class T>
zx_status_t validate_handle(T handle) {
  return handle ? ZX_OK : ZX_ERR_BAD_HANDLE;
}

/*template zx_status_t validate_handle<typename zx::object_traits<zx::event>::StorageType>(
    zx::object_traits<zx::event>::StorageType handle);*/

TEST(ZxTestCase, HandleInvalid) {
  zx::handle handle;
  // A default constructed handle is invalid.
  ASSERT_EQ(handle.release(), nullptr);
}

TEST(ZxTestCase, DISABLED_HandleClose) {
  // TODO (Herrera) : fix impl to replicate this behaviour
  KernelHandle<EventDispatcher> raw_event;
  zx_rights_t rights;

  ASSERT_OK(EventDispatcher::Create(0, &raw_event, &rights));
  ASSERT_OK(validate_handle(raw_event.dispatcher()));
  { zx::event handle(raw_event.dispatcher()); }
  // Make sure the handle was closed.
  ASSERT_EQ(validate_handle(raw_event.dispatcher()), ZX_ERR_BAD_HANDLE);
}

TEST(ZxTestCase, HandleMove) {
  zx::event event;
  // Check move semantics.
  ASSERT_OK(zx::event::create(0u, &event));
  zx::event handle(std::move(event));
  ASSERT_EQ(event.release(), nullptr);
  ASSERT_OK(validate_handle(handle.get()));
}

TEST(ZxTestCase, GetInfo) {
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(1, 0u, &vmo));

  // zx::vmo is just an easy object to create; this is really a test of zx::object_base.
  const zx::object_base<zx::object_traits<zx::vmo>::StorageType>& object = vmo;
  zx_info_handle_count_t info;
  EXPECT_OK(object.get_info(ZX_INFO_HANDLE_COUNT, &info, sizeof(info), nullptr, nullptr));
  EXPECT_EQ(info.handle_count, 1);
}

TEST(ZxTestCase, SetGetProperty) {
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(1, 0u, &vmo));

  // zx::vmo is just an easy object to create; this is really a test of zx::object_base.
  const char name[] = "a great maximum length vmo name";
  const zx::object_base<zx::object_traits<zx::vmo>::StorageType>& object = vmo;
  EXPECT_OK(object.set_property(ZX_PROP_NAME, name, sizeof(name)));

  char read_name[ZX_MAX_NAME_LEN];
  EXPECT_OK(object.get_property(ZX_PROP_NAME, read_name, sizeof(read_name)));
  EXPECT_STREQ(name, read_name);
}

TEST(ZxTestCase, Event) {
  zx::event event;
  ASSERT_OK(zx::event::create(0u, &event));
  ASSERT_OK(validate_handle(event.get()));
  // TODO(cpu): test more.
}

// TEST(ZxTestCase, Socket)

TEST(ZxTestCase, Vmar) {
  zx::vmar vmar;
  const size_t size = PAGE_SIZE;
  uintptr_t addr;
  ASSERT_OK(zx::vmar::root_self()->allocate(ZX_VM_CAN_MAP_READ, 0u, size, &vmar, &addr));
  ASSERT_NOT_NULL(vmar.get());
  ASSERT_OK(vmar.destroy());
  // TODO(teisenbe): test more.
}

template <typename T>
void IsValidHandle(const T& p) {
  ASSERT_TRUE(static_cast<bool>(p), "invalid handle");
}

TEST(ZxTestCase, ThreadSelf) {
  zx::thread thread;
  // This will fail when running from kernel
  // ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::thread>(*zx::thread::self()));
}

TEST(ZxTestCase, ThreadCreateEmptyProcess) {
  zx::thread thread;
  const char* name = "test thread";
  ASSERT_NOT_OK(zx::thread::create(zx::process(), name, sizeof(name), 0u, &thread));
}

TEST(ZxTestCase, ProcessSelf) {
  zx::process process;
  // This will fail when running from kernel
  // ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::process>(*zx::process::self()));
}

TEST(ZxTestCase, DISABLED_ProcessAndThreadCreate) {
  zx::process process;
  zx::thread thread;
  zx::vmar vmar;
  const char* pname = "test process";
  const char* tname = "test thread";
  ASSERT_OK(zx::process::create(*zx::unowned_job{zx::job::default_job()}, pname, sizeof(pname), 0,
                                &process, &vmar));
  EXPECT_TRUE(process.is_valid());
  EXPECT_TRUE(vmar.is_valid());

  ASSERT_OK(zx::thread::create(process, tname, sizeof(tname), 0u, &thread));
  EXPECT_TRUE(thread.is_valid());
}

TEST(ZxTestCase, VmarRootSelf) {
  ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::vmar>(*zx::vmar::root_self()));

  // This does not compile:
  // const zx::vmar root_self = zx::vmar::root_self();
}

TEST(ZxTestCase, JobDefault) {
  ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::job>(*zx::job::default_job()));

  // This does not compile:
  // const zx::job default_job = zx::job::default_job();
}

TEST(ZxTestCase, Unowned) {
  // Create a handle to test with.
  zx::event handle;
  ASSERT_OK(zx::event::create(0, &handle));
  ASSERT_OK(validate_handle(handle.get()));

  // Verify that unowned<T>(zx_handle_t) doesn't close handle on teardown.
  {
    zx::unowned<zx::event> unowned(handle.get());
    EXPECT_EQ(unowned->get(), handle.get());
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned));
  }
  ASSERT_OK(validate_handle(handle.get()));

  // Verify that unowned<T>(const T&) doesn't close handle on teardown.
  {
    zx::unowned<zx::event> unowned(handle);
    EXPECT_EQ(unowned->get(), handle.get());
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned));
  }
  ASSERT_OK(validate_handle(handle.get()));

  // Verify that unowned<T>(const unowned<T>&) doesn't close on teardown.
  {
    zx::unowned<zx::event> unowned(handle);
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned));

    zx::unowned<zx::event> unowned2(unowned);
    EXPECT_EQ(unowned->get(), unowned2->get());
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned2));
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned));
  }
  ASSERT_OK(validate_handle(handle.get()));

  // Verify copy-assignment from unowned<> to unowned<> doesn't close.
  {
    zx::unowned<zx::event> unowned(handle);
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned));

    zx::unowned<zx::event> unowned2;
    ASSERT_FALSE(unowned2->is_valid());

    const zx::unowned<zx::event>& assign_ref = unowned2 = unowned;
    EXPECT_EQ(assign_ref->get(), unowned2->get());
    EXPECT_EQ(unowned->get(), unowned2->get());
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned2));
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned));
  }
  ASSERT_OK(validate_handle(handle.get()));

  // Verify move from unowned<> to unowned<> doesn't close on teardown.
  {
    zx::unowned<zx::event> unowned(handle);
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned));

    zx::unowned<zx::event> unowned2(static_cast<zx::unowned<zx::event>&&>(unowned));
    EXPECT_EQ(unowned2->get(), handle.get());
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned2));
    EXPECT_FALSE(unowned->is_valid());
  }
  ASSERT_OK(validate_handle(handle.get()));

  // Verify move-assignment from unowned<> to unowned<> doesn't close.
  {
    zx::unowned<zx::event> unowned(handle);
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned));

    zx::unowned<zx::event> unowned2;
    ASSERT_FALSE(unowned2->is_valid());

    const zx::unowned<zx::event>& assign_ref = unowned2 =
        static_cast<zx::unowned<zx::event>&&>(unowned);
    EXPECT_EQ(assign_ref->get(), unowned2->get());
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned2));
    EXPECT_FALSE(unowned->is_valid());
  }
  ASSERT_OK(validate_handle(handle.get()));

  // Verify move-assignment into non-empty unowned<>  doesn't close.
  {
    zx::unowned<zx::event> unowned(handle);
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned));

    zx::unowned<zx::event> unowned2(handle);
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned2));

    unowned2 = static_cast<zx::unowned<zx::event>&&>(unowned);
    EXPECT_EQ(unowned2->get(), handle.get());
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned2));
    EXPECT_FALSE(unowned->is_valid());
  }
  ASSERT_OK(validate_handle(handle.get()));

#if 0
  // Explicitly verify dereference operator allows methods to be called.
  {
    zx::unowned<zx::event> unowned(handle);
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned));

    const zx::event& event_ref = *unowned;
    zx::event duplicate;
    EXPECT_OK(event_ref.duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate));
  }
  ASSERT_OK(validate_handle(handle.get()));

  // Explicitly verify member access operator allows methods to be called.
  {
    zx::unowned<zx::event> unowned(handle);
    ASSERT_NO_FATAL_FAILURE(IsValidHandle<zx::event>(*unowned));

    zx::event duplicate;
    EXPECT_OK(unowned->duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate));
  }
  ASSERT_OK(validate_handle(handle.get()));
#endif
}

TEST(ZxTestCase, Unowned2) {
  zx::event handle;
  ASSERT_OK(zx::event::create(0, &handle));
  ASSERT_OK(validate_handle(handle.get()));
  { const zx::unowned_event event{handle}; }
  EXPECT_TRUE(handle.is_valid());
}

TEST(ZxTestCase, VmoContentSize) {
  zx::vmo vmo;
  constexpr uint32_t options = 0;
  constexpr uint64_t initial_size = 8 * 1024;
  ASSERT_OK(zx::vmo::create(initial_size, options, &vmo));

  uint64_t retrieved_size = 0;
  ASSERT_OK(vmo.get_prop_content_size(&retrieved_size));
  EXPECT_EQ(retrieved_size, initial_size);
  retrieved_size = 0;

  constexpr uint64_t new_size = 500;
  EXPECT_OK(vmo.set_prop_content_size(new_size));

  ASSERT_OK(vmo.get_prop_content_size(&retrieved_size));
  EXPECT_EQ(retrieved_size, new_size);
  retrieved_size = 0;

  ASSERT_OK(vmo.get_property(ZX_PROP_VMO_CONTENT_SIZE, &retrieved_size, sizeof(retrieved_size)));
  EXPECT_EQ(retrieved_size, new_size);

  zx::unowned_vmo uvmo(vmo);
  ASSERT_OK(uvmo->get_property(ZX_PROP_VMO_CONTENT_SIZE, &retrieved_size, sizeof(retrieved_size)));
  EXPECT_EQ(retrieved_size, new_size);
}

TEST(ZxTestCase, DebugLog) {
  zx::resource parent;
  zx::resource res;
  ASSERT_OK(zx::resource::create(parent, 0, 0, 0, nullptr, 0, &res));

  zx::debuglog log;
  ASSERT_OK(zx::debuglog::create(res, 0, &log));
  EXPECT_OK(log.write(0, "Hello!", sizeof("Hello!")));
}

}  // namespace
