// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <latch>
#include <stop_token>
#include <thread>

#include "dl-load-tests.h"
#include "startup-symbols.h"

namespace {

using dl::testing::DlTests;
TYPED_TEST_SUITE(DlTests, dl::testing::TestTypes);

using dl::testing::IsUndefinedSymbolErrMsg;
using dl::testing::RunFunction;

TYPED_TEST(DlTests, TlsDescStaticStartupModules) {
  const std::string kStaticTlsDescModuleFile = "static-tls-desc-module.so";

  EXPECT_EQ(gStaticTlsVar, kStaticTlsDataValue);

  this->ExpectRootModule(kStaticTlsDescModuleFile);

  auto open = this->DlOpen(kStaticTlsDescModuleFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value()) << open.error_value();

  auto sym = this->DlSym(open.value(), "get_static_tls_var");
  ASSERT_TRUE(sym.is_ok()) << sym.error_value();
  ASSERT_TRUE(sym.value());

  EXPECT_EQ(*RunFunction<int*>(sym.value()), kStaticTlsDataValue);

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());
}

TYPED_TEST(DlTests, TlsGetAddrStaticStartupModules) {
  const std::string kStaticTlsModuleFile = "static-tls-module.so";

  this->ExpectRootModule(kStaticTlsModuleFile);

  // Don't expect tls_get_addr() to return any useful value for relocations, but
  // expect that dlopen() will at least succeed when calling it.
  auto open = this->DlOpen(kStaticTlsModuleFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value()) << open.error_value();

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());
}

// Holds the names for the TLS module and test APIs.
struct TlsLoadedSymbolNames {
  const char* module;
  const char* data_symbol;
  const char* bss1_symbol;
  const char* weak_symbol;
};

// Number of threads for TLS Tests.
constexpr int kTlsTestNumThreads = 10;

// Module names for the different combinations of Traditional TLS/TLSDESC, and GD/LD.
constexpr const char* kTraditionalTlsGdModuleName = "tls-dep-module.so";
constexpr const char* kTlsDescGdModuleName = "tls-desc-dep-module.so";
constexpr const char* kTraditionalTlsLdModuleName = "tls-ld-dep-module.so";
constexpr const char* kTlsDescLdModuleName = "tls-desc-ld-dep-module.so";

// Symbol name differences between GD and LD versions of the module.
constexpr const char* kGdDataSymbolName = "get_tls_dep_data";
constexpr const char* kGdBss1SymbolName = "get_tls_dep_bss1";
constexpr const char* kGdWeakSymbolName = "get_tls_dep_weak";

constexpr const char* kLdDataSymbolName = "get_tls_ld_dep_data";
constexpr const char* kLdBss1SymbolName = "get_tls_ld_dep_bss1";

// Initial data values for get_tls_dep_data/get_tls_ld_dep_data
constexpr int kTlsGdDataInitialVal = 42;
constexpr int kTlsLdDataInitialVal = 23;

struct TlsTestCtx {
  // The tls_dep_data initial value: 42 for GD, 23 for LD.
  int tls_data_initial_val;
  // The bss1 initial value: Always 0.
  char bss1_initial_val = {0};
  // Are we testing the TLSDESC case?
  bool is_tlsdesc;
  // Are we testing the LD case?
  bool is_local_dynamic;
};

class TestThreadRunner {
 public:
  TestThreadRunner() = default;
  ~TestThreadRunner() = default;

  // Start worker threads, with specified workloads.
  //
  // Each worker has 3 basic phases: pre_task, task, and post_task.
  // In each phase, the worker runs the corresponding callback, where each
  // callback and synchronization is preceded by a check to stop_requested.
  //
  // The worker is expected to launch and run any pre_task before blocking.
  // This allows a worker with an empty pre_task to launch and then park itself
  // until the main thread is ready for the main task to continue.
  // After task() returns, the worker will again block until the main thread
  // allows it to complete, at which point it can run the post_task.
  template <typename PreTask, typename Task, typename PostTask>
  void StartWorkersWaiting(PreTask&& pre_task, Task&& task, PostTask&& post_task) {
    auto worker = [this, pre_task, task, post_task](std::stop_token stoken) {
      if (stoken.stop_requested()) {
        return;
      }
      pre_task();
      if (stoken.stop_requested()) {
        return;
      }
      WorkerWaitForMainReady();
      if (stoken.stop_requested()) {
        return;
      }
      task();
      if (stoken.stop_requested()) {
        return;
      }
      WorkerWaitForMainDone();
      if (stoken.stop_requested()) {
        return;
      }
      post_task();
    };

    for (std::jthread& thread : threads_) {
      thread = std::jthread(worker);
    }
  }

  template <typename PreTask, typename Task, typename PostTask>
  void StartWorkersNow(PreTask&& pre_task, Task&& task, PostTask&& post_task) {
    MainLetWorkersRun();
    StartWorkersWaiting(pre_task, task, post_task);
  }

  void RequestStop() { std::ranges::for_each(threads_, &std::jthread::request_stop); }

  void MainWaitForWorkerReady() const { worker_ready_.wait(); }

  void MainWaitForWorkerDone() const { worker_done_.wait(); }

  void MainLetWorkersRun() { main_ready_.count_down(); }

  void MainLetWorkersFinish() { main_done_.count_down(); }

  void WorkerWaitForMainReady() {
    worker_ready_.count_down();
    main_ready_.wait();
  }

  void WorkerWaitForMainDone() {
    worker_done_.count_down();
    main_done_.wait();
  }

 private:
  // Worker threads.
  std::array<std::jthread, kTlsTestNumThreads> threads_;
  // Blocks until the main thread is ready.
  std::latch main_ready_ = std::latch(1);
  // Blocks until the main thread is done.
  std::latch main_done_ = std::latch(1);
  // Blocks until all the worker threads are ready.
  std::latch worker_ready_ = std::latch(kTlsTestNumThreads);
  // Blocks until all the worker threads are done.
  std::latch worker_done_ = std::latch(kTlsTestNumThreads);
};

template <class Test>
class OpenModule {
  using SymbolMap = std::unordered_map<std::string, void*>;

 public:
  explicit OpenModule(Test& test_fixture) : fixture_(test_fixture) {}

  void InitModule(const char* file, int mode, std::initializer_list<const char*> lookup_symbols,
                  const char* canary_symbol = nullptr) {
    fixture_.ExpectRootModule(file);
    file_ = file;
    auto open = fixture_.DlOpen(file_, mode);
    ASSERT_TRUE(open.is_ok()) << file_ << ": " << open.error_value();
    handle_ = open.value();

    if (canary_symbol && !IsSymbolEnabledAtCompileTime(canary_symbol)) {
      return;
    }
    InitSymbols(lookup_symbols);
  }

  void InitSymbols(std::initializer_list<const char*> symbol_list) {
    for (const char* symbol : symbol_list) {
      auto sym = fixture_.DlSym(handle_, symbol);
      ASSERT_TRUE(sym.is_ok()) << file_ << ": " << symbol << ": " << sym.error_value();
      symbols_[symbol] = sym.value();
    }
  }

  bool IsSymbolEnabledAtCompileTime(const char* symbol) {
    auto sym = fixture_.DlSym(handle_, symbol);
    if (sym.is_error()) {
      EXPECT_THAT(sym.error_value().take_str(), IsUndefinedSymbolErrMsg(symbol, file_));
      skip_ = true;
    }
    return !skip_;
  }

  void CloseHandle() {
    if (handle_) {
      auto close = fixture_.DlClose(std::exchange(handle_, nullptr));
      EXPECT_TRUE(close.is_ok()) << close.error_value();
    }
  }

  ~OpenModule() { CloseHandle(); }

  bool Skip() { return skip_; }

  void* operator[](std::string_view name) const { return symbols_.at(std::string(name)); }

  template <typename T>
  std::optional<std::pair<T, T>> TryAccess(std::string_view getter_name) const {
    void* getter = symbols_.at(std::string(getter_name));
    if (T* ptr = RunFunction<T*>(getter)) {
      T first = *ptr;
      ++*ptr;
      T second = *RunFunction<T*>(getter);
      return std::make_pair(first, second);
    }
    EXPECT_EQ(RunFunction<T*>(getter), nullptr);
    return std::nullopt;
  }

 private:
  Test& fixture_;
  const char* file_ = nullptr;
  void* handle_ = nullptr;
  SymbolMap symbols_;
  bool skip_ = false;
};

// A routine that exercises the fast path for TLS accesses.
//
// This test exercises the following sequence of events:
//  1. The initial thread is created with initial-exec TLS state.
//  2. dlopen adds dynamic TLS state and bumps DTV generation.
//  3. The initial thread uses dynamic TLS via the new DTV.
//  3. New threads are launched.
//  5. The new threads use dynamic TLS, via the fast path, and wait.
//  7. The initial thread calls dlclose on the loaded module.
//  8. The remaining threads complete, potentially accessing pre-existing TLS state.
//
// NOTE: Whether the slow path may also be used in this test depends on the
// implementation. For instance, at the time of writing, musl's dlopen doesn't
// update the calling thread's DTV and instead relies on the first access on the
// thread to use the slow path to call __tls_get_new. However, this test should
// only be relied upon for testing the fast path, because that is the only thing
// we can guarantee for all implementations.
template <class Test>
void DynamicTlsFastPath(Test& self, const TlsLoadedSymbolNames& names, const TlsTestCtx& ctx) {
  OpenModule mod(self);
  ASSERT_NO_FATAL_FAILURE(mod.InitModule(names.module, RTLD_NOW | RTLD_LOCAL,
                                         {names.data_symbol, names.bss1_symbol},
                                         names.data_symbol));

  if (mod.Skip()) {
    // If the module wasn't compiled to have the right type of TLS relocations,
    // then the symbols won't exist in the module, and we should skip the rest of
    // the test.
    GTEST_SKIP() << "Test module disabled at compile time.";
  }

  if (!ctx.is_local_dynamic) {
    // The get_dep_weak symbol is only defined for the GD case.
    mod.InitSymbols({names.weak_symbol});
  }

  auto access_tls_vars = [&names, &mod = std::as_const(mod), &ctx]() {
    EXPECT_THAT(mod.template TryAccess<int>(names.data_symbol),
                std::optional(std::pair{ctx.tls_data_initial_val, ctx.tls_data_initial_val + 1}));
    EXPECT_THAT(mod.template TryAccess<char>(names.bss1_symbol), std::optional(std::pair{0, 1}));

    if (!ctx.is_local_dynamic && ctx.is_tlsdesc) {
      // Only the TLSDESC case is guaranteed to return a nullptr for a missing weak symbol.
      EXPECT_EQ(RunFunction<int*>(mod[names.weak_symbol]), nullptr);
    }
  };

  // On the fast path, we access the TLS vars before launching new threads.
  access_tls_vars();

  TestThreadRunner tr;
  auto do_nothing = []() {};
  // TODO(https://fxbug.dev/376130102): do some other TLS access w/ initial module in the worker,
  // and then exit.

  tr.StartWorkersNow(do_nothing, access_tls_vars, do_nothing);
  tr.MainWaitForWorkerDone();

  // Now that the workers have finished, we want to close the module before
  // allowing all the other threads to finish, because we want to test that the
  // initially loaded module still works as expected after dlclose.
  mod.CloseHandle();

  tr.MainLetWorkersFinish();
  // TODO(https://fxbug.dev/376130102): access the initial TLS module, and then dlclose it.
}

TYPED_TEST(DlTests, TlsDescGlobalDynamicFastPath) {
  if constexpr (!TestFixture::kSupportsDynamicTls) {
    GTEST_SKIP() << "test requires TLS";
  }

  // TLS module details
  constexpr TlsLoadedSymbolNames kModuleNames = {
      .module = kTlsDescGdModuleName,
      .data_symbol = kGdDataSymbolName,
      .bss1_symbol = kGdBss1SymbolName,
      .weak_symbol = kGdWeakSymbolName,
  };

  TlsTestCtx ctx = {
      .tls_data_initial_val = kTlsGdDataInitialVal,
      .bss1_initial_val = 0,
      .is_tlsdesc = true,
      .is_local_dynamic = false,
  };
  DynamicTlsFastPath(*this, kModuleNames, ctx);
}

TYPED_TEST(DlTests, TlsGetAddrGlobalDynamicFastPath) {
  if constexpr (!TestFixture::kSupportsDynamicTls) {
    GTEST_SKIP() << "test requires TLS";
  }

  // TLS module details
  constexpr TlsLoadedSymbolNames kModuleNames = {
      .module = kTraditionalTlsGdModuleName,
      .data_symbol = kGdDataSymbolName,
      .bss1_symbol = kGdBss1SymbolName,
      .weak_symbol = kGdWeakSymbolName,
  };

  TlsTestCtx ctx = {
      .tls_data_initial_val = kTlsGdDataInitialVal,
      .bss1_initial_val = 0,
      .is_tlsdesc = false,
      .is_local_dynamic = false,
  };

  DynamicTlsFastPath(*this, kModuleNames, ctx);
}

TYPED_TEST(DlTests, TlsDescLocalDynamicFastPath) {
  if constexpr (!TestFixture::kSupportsDynamicTls) {
    GTEST_SKIP() << "test requires TLS";
  }

  // TLS module details
  constexpr TlsLoadedSymbolNames kModuleNames = {
      .module = kTlsDescLdModuleName,
      .data_symbol = kLdDataSymbolName,
      .bss1_symbol = kLdBss1SymbolName,
      .weak_symbol = nullptr,
  };

  TlsTestCtx ctx = {
      .tls_data_initial_val = kTlsLdDataInitialVal,
      .bss1_initial_val = 0,
      .is_tlsdesc = true,
      .is_local_dynamic = true,
  };

  DynamicTlsFastPath(*this, kModuleNames, ctx);
}

TYPED_TEST(DlTests, TlsGetAddrLocalDynamicFastPath) {
  if constexpr (!TestFixture::kSupportsDynamicTls) {
    GTEST_SKIP() << "test requires TLS";
  }

  // TLS module details
  constexpr TlsLoadedSymbolNames kModuleNames = {
      .module = kTraditionalTlsLdModuleName,
      .data_symbol = kLdDataSymbolName,
      .bss1_symbol = kLdBss1SymbolName,
      .weak_symbol = nullptr,
  };

  TlsTestCtx ctx = {
      .tls_data_initial_val = kTlsLdDataInitialVal,
      .bss1_initial_val = 0,
      .is_tlsdesc = false,
      .is_local_dynamic = true,
  };

  DynamicTlsFastPath(*this, kModuleNames, ctx);
}

// A routine that exercises the slow path for TLS accesses.
//
// This test exercises the following sequence of events:
//  1. The initial thread is created with initial-exec TLS state.
//  2. New threads are launched with the same initial TLS state.
//  3. The new threads are parked until all threads are ready.
//  4. dlopen adds dynamic TLS state and bumps DTV generation.
//  5. The new threads use dynamic TLS, via the slow path, and wait.
//  6. The main thread accesses dynamic TLS.
//  7. The module is dlclosed.
//  8. The remaining threads complete, potentially accessing any pre-existing TLS state.
template <class Test>
void DynamicTlsSlowPath(Test& self, const TlsLoadedSymbolNames& names, const TlsTestCtx& ctx) {
  OpenModule mod(self);

  auto access_tls_vars = [&names, &mod = std::as_const(mod), &ctx]() {
    EXPECT_THAT(mod.template TryAccess<int>(names.data_symbol),
                std::optional(std::pair{ctx.tls_data_initial_val, ctx.tls_data_initial_val + 1}));
    EXPECT_THAT(mod.template TryAccess<char>(names.bss1_symbol), std::optional(std::pair{0, 1}));
    if (!ctx.is_local_dynamic && ctx.is_tlsdesc) {
      // Only the TLSDESC case is guaranteed to return a nullptr for a missing weak symbol.
      EXPECT_EQ(RunFunction<int*>(mod[names.weak_symbol]), nullptr);
    }
  };

  auto do_nothing = []() {};
  TestThreadRunner tr;
  tr.StartWorkersWaiting(do_nothing, access_tls_vars, do_nothing);
  // TODO(https://fxbug.dev/376130102): do some other tls access w/ initial module in the worker,
  // and then exit.

  // First synchronization (wait until workers are ready).
  tr.MainWaitForWorkerReady();

  ASSERT_NO_FATAL_FAILURE(mod.InitModule(names.module, RTLD_NOW | RTLD_LOCAL,
                                         {names.data_symbol, names.bss1_symbol}, names.module));
  if (mod.Skip()) {
    tr.RequestStop();
    tr.MainLetWorkersRun();
    // If the module wasn't compiled to have the right type of TLS relocations,
    // then the symbols won't exist in the module, and we should skip the rest
    // of the test.
    GTEST_SKIP() << "Test module disabled at compile time.";
  }

  if (!ctx.is_local_dynamic) {
    // The get_dep_weak symbol is only defined for the GD case.
    mod.InitSymbols({names.weak_symbol});
  }

  // Let the worker threads start, and wait for them to complete.
  tr.MainLetWorkersRun();
  tr.MainWaitForWorkerDone();

  access_tls_vars();

  // We're done w/ TLS accesses to the test module, so its safe to close it.
  mod.CloseHandle();

  // Allow workers to finish any remaining work, and then exit.
  tr.MainLetWorkersFinish();
  // TODO(https://fxbug.dev/376130102): access the initial TLS module, and then dlclose it.
}

TYPED_TEST(DlTests, TlsDescGlobalDynamicSlowPath) {
  if constexpr (!TestFixture::kSupportsDynamicTls) {
    GTEST_SKIP() << "test requires TLS";
  }

  // TLS module details
  constexpr TlsLoadedSymbolNames kModuleNames = {
      .module = kTlsDescGdModuleName,
      .data_symbol = kGdDataSymbolName,
      .bss1_symbol = kGdBss1SymbolName,
      .weak_symbol = kGdWeakSymbolName,
  };

  TlsTestCtx ctx = {
      .tls_data_initial_val = kTlsGdDataInitialVal,
      .bss1_initial_val = 0,
      .is_tlsdesc = true,
      .is_local_dynamic = false,
  };

  DynamicTlsSlowPath(*this, kModuleNames, ctx);
}

TYPED_TEST(DlTests, TlsGetAddrGlobalDynamicSlowPath) {
  if constexpr (!TestFixture::kSupportsDynamicTls) {
    GTEST_SKIP() << "test requires TLS";
  }

  // TLS module details
  constexpr TlsLoadedSymbolNames kModuleNames = {
      .module = kTraditionalTlsGdModuleName,
      .data_symbol = kGdDataSymbolName,
      .bss1_symbol = kGdBss1SymbolName,
      .weak_symbol = kGdWeakSymbolName,
  };

  TlsTestCtx ctx = {
      .tls_data_initial_val = kTlsGdDataInitialVal,
      .bss1_initial_val = 0,
      .is_tlsdesc = false,
      .is_local_dynamic = false,
  };

  DynamicTlsSlowPath(*this, kModuleNames, ctx);
}

TYPED_TEST(DlTests, TlsDescLocalDynamicSlowPath) {
  if constexpr (!TestFixture::kSupportsDynamicTls) {
    GTEST_SKIP() << "test requires TLS";
  }

  // TLS module details
  constexpr TlsLoadedSymbolNames kModuleNames = {
      .module = kTlsDescLdModuleName,
      .data_symbol = kLdDataSymbolName,
      .bss1_symbol = kLdBss1SymbolName,
      .weak_symbol = nullptr,
  };

  TlsTestCtx ctx = {
      .tls_data_initial_val = kTlsLdDataInitialVal,
      .bss1_initial_val = 0,
      .is_tlsdesc = true,
      .is_local_dynamic = true,
  };

  DynamicTlsSlowPath(*this, kModuleNames, ctx);
}

TYPED_TEST(DlTests, TlsGetAddrLocalDynamicSlowPath) {
  if constexpr (!TestFixture::kSupportsDynamicTls) {
    GTEST_SKIP() << "test requires TLS";
  }

  // TLS module details
  constexpr TlsLoadedSymbolNames kModuleNames = {
      .module = kTraditionalTlsLdModuleName,
      .data_symbol = kLdDataSymbolName,
      .bss1_symbol = kLdBss1SymbolName,
      .weak_symbol = nullptr,
  };

  TlsTestCtx ctx = {
      .tls_data_initial_val = kTlsLdDataInitialVal,
      .bss1_initial_val = 0,
      .is_tlsdesc = false,
      .is_local_dynamic = true,
  };

  DynamicTlsSlowPath(*this, kModuleNames, ctx);
}

}  // namespace
