// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/container.h>
#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/mapped-vmo-file.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/note.h>
#include <lib/elfldltl/symbol.h>
#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/elfldltl/testing/get-test-data.h>
#include <lib/elfldltl/vmar-loader.h>
#include <lib/fit/defer.h>
#include <lib/symbolizer-markup/writer.h>
#include <lib/zx/exception.h>
#include <lib/zx/process.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <string.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls/debug.h>
#include <zircon/syscalls/exception.h>
#include <zircon/testonly-syscalls.h>
#include <zircon/threads.h>

#include <mutex>
#include <optional>
#include <thread>

#include <runtime/thread.h>
#include <zxtest/zxtest.h>

#include "../needs-next.h"
#include "arch-register-state.h"

NEEDS_NEXT_SYSCALL(zx_restricted_bind_state);
NEEDS_NEXT_SYSCALL(zx_restricted_enter);
NEEDS_NEXT_SYSCALL(zx_restricted_kick);
NEEDS_NEXT_SYSCALL(zx_restricted_unbind_state);

extern "C" zx_status_t restricted_enter_wrapper(uint32_t options, uintptr_t vector_table,
                                                zx_restricted_reason_t* reason_code);
extern "C" void restricted_exit(uintptr_t context, zx_restricted_reason_t reason_code);
extern "C" void load_fpu_registers(void* in);
extern "C" void store_fpu_registers(void* out);

// These constants ensure that there is enough space mapped at the correct
// addresses for thread local storage and atomics used with restricted mode
// in the fixture below.
static const uint32_t kRestrictedAtomicCount = 2;
static const uint32_t kRestrictedThreadCount = 32;

namespace {

class RestrictedSymbols {
 public:
  RestrictedSymbols() = default;

  template <class Elf>
  bool Init(elfldltl::LocalVmarLoader& loader, const elfldltl::SymbolInfo<Elf>& symbol_info) {
    for (const auto& name : kSymbolNames) {
      const auto* sym = elfldltl::SymbolName(name).Lookup(symbol_info);
      if (!sym) {
        ADD_FAILURE() << "failed to lookup symbol " << name;
        return false;
      }
      symbol_addrs_[name] = sym->value() + loader.load_bias();
    }
    return true;
  }

  uint64_t addr_of(const std::string& symbol) const { return symbol_addrs_.at(symbol); }

 private:
  static constexpr std::array kSymbolNames = {
      "syscall_bounce",    "syscall_bounce_post_syscall",
      "exception_bounce",  "exception_bounce_exception_address",
      "wait_then_syscall", "store_one"};
  std::unordered_map<std::string, uint64_t> symbol_addrs_;
};

// The build system informs us of how many blobs exist and what machine they map
// to and if they have a maximum memory limit.
typedef struct RestrictedBlobInfo {
  std::string_view name;
  elfldltl::ElfMachine machine;
  uint64_t max_load_address;
} RestrictedBlobInfo;
constexpr std::array<RestrictedBlobInfo, RESTRICTED_BLOB_COUNT> kRestrictedBlobs{
    RESTRICTED_BLOB_INFO};

// This class encapsulates loading a single restricted blob.
class RestrictedBlob {
 public:
  void Initialize(const RestrictedBlobInfo& info) { LoadRestrictedBlob(info); }

  const RestrictedSymbols& restricted_symbols() const { return restricted_symbols_; }

 private:
  static const zx::vmo& elf_vmo(const RestrictedBlobInfo& blob_info) {
    // This must only be loaded once because we can only fetch it from bootfs once.
    // This is because bootfs transfers data to callers instead of copying it.
    auto machine = blob_info.machine;
    if (elf_vmos_.contains(machine)) {
      return elf_vmos_[machine];
    }
    elf_vmos_[machine] = elfldltl::testing::GetTestLibVmo(blob_info.name);
    EXPECT_TRUE(elf_vmos_[machine]);
    return elf_vmos_[machine];
  }

  static auto LogSink() {
    return zxtest::Runner::GetInstance()->mutable_reporter()->mutable_log_sink();
  }

  static void Log(std::string_view str) {
    LogSink()->Write("%.*s", static_cast<int>(str.size()), str.data());
  }

  void LoadRestrictedBlob(const RestrictedBlobInfo& blob_info) {
    // Map the VMO into the test harness's address space for convenience of extracting the build ID.
    elfldltl::MappedVmoFile file;
    {
      auto result = file.Init(elf_vmo(blob_info).borrow());
      ASSERT_TRUE(result.is_ok()) << "loading " << blob_info.name << ": " << result.status_string();
    }

    // We use the ELF machine enum as the blob id to aid with debugging.
    unsigned int id = static_cast<unsigned int>(blob_info.machine);

    auto diag = elfldltl::testing::ExpectOkDiagnostics();

    // This lambda actually loads the ELF binary into the restricted address space.
    auto load = [this, blob_info, id, &file, &diag]<class Ehdr, class Phdr>(
                    const Ehdr& ehdr, std::span<const Phdr> phdrs) -> bool {
      using Elf = typename Ehdr::ElfLayout;
      using size_type = typename Elf::size_type;
      using Dyn = typename Elf::Dyn;
      using LoadInfo = elfldltl::LoadInfo<Elf, elfldltl::StdContainer<std::vector>::Container>;

      // Perform basic header checks. Since we may be loading for an architecture that is not the
      // same as the host architecture, we have to pass nullopt to the machine argument.
      EXPECT_TRUE(ehdr.Loadable(diag, std::nullopt)) << blob_info.name << " not loadable";

      // This will collect the build ID from the file, for the symbolizer markup.
      std::span<const std::byte> build_id;
      auto build_id_observer = [&build_id](const auto& note) -> fit::result<fit::failed, bool> {
        if (!note.IsBuildId()) {
          // This is a different note, so keep looking.
          return fit::ok(true);
        }
        build_id = note.desc;
        // Tell the caller not to call again for another note.
        return fit::ok(false);
      };

      // Get all the essentials from the phdrs: load info, the build ID note, and
      // the PT_DYNAMIC phdr.
      LoadInfo load_info;
      std::optional<Phdr> dyn_phdr;
      EXPECT_TRUE(elfldltl::DecodePhdrs(
          diag, phdrs, load_info.GetPhdrObserver(static_cast<size_type>(loader_.page_size())),
          elfldltl::PhdrFileNoteObserver(Elf{}, file, elfldltl::NoArrayFromFile<std::byte>{},
                                         build_id_observer),
          elfldltl::PhdrDynamicObserver<Elf>(dyn_phdr)));

      // Attempt to allocate the ELF vmo below any machine-required maximum.
      if (blob_info.max_load_address != 0) {
        // It's worth noting that there is nothing magic about starting and
        // incrementing by 0x20000. It avoids starting too low and increments
        // by several times what's required to store the restricted blobs.
        // The alternative would be refactoring Allocate() to take an upper
        // limit or using a much smaller offset to search for an available
        // restricted_blob-sized space.
        size_t vmar_offset = 0x20000;
        while (!loader_.Allocate(diag, load_info, vmar_offset)) {
          printf("searching for valid ELF allocation offset: %lu\n", vmar_offset);
          vmar_offset = vmar_offset + 0x20000;
          if (vmar_offset >= blob_info.max_load_address) {
            ADD_FAILURE() << "failed to allocate restricted addressable memory for "
                          << blob_info.name;
            return false;
          }
        }
      }

      if (!loader_.Load(diag, load_info, elf_vmo(blob_info).borrow())) {
        ADD_FAILURE() << "cannot load " << blob_info.name;
        return false;
      }

      // Log symbolizer markup context for the test module to ease debugging.
      symbolizer_markup::Writer markup_writer{Log};
      load_info.SymbolizerContext(
          markup_writer, id, blob_info.name, build_id,
          static_cast<size_type>(load_info.vaddr_start() + loader_.load_bias()));

      // Read the PT_DYNAMIC, which leads to symbol information.
      cpp20::span<const Dyn> dyn;
      {
        EXPECT_TRUE(dyn_phdr) << blob_info.name << " has no PT_DYNAMIC";
        if (!dyn_phdr) {
          return false;
        }
        auto read_dyn =
            loader_.memory().ReadArray<Dyn>(dyn_phdr->vaddr, dyn_phdr->filesz / sizeof(Dyn));
        EXPECT_TRUE(read_dyn) << blob_info.name << " PT_DYNAMIC not read";
        if (read_dyn) {
          dyn = *read_dyn;
        }
      }

      // Decode PT_DYNAMIC just enough to get the symbols.
      elfldltl::SymbolInfo<Elf> symbol_info;
      EXPECT_TRUE(elfldltl::DecodeDynamic(diag, loader_.memory(), dyn,
                                          elfldltl::DynamicSymbolInfoObserver(symbol_info)));

      // Load the symbols we need for restricted mode tests.
      return restricted_symbols_.Init(loader_, symbol_info);
    };

    // This reads the ELF header just enough to dispatch to an instantiation of
    // the lambda for the specific ELF format found (accepting all four formats).
    auto phdr_allocator = [&diag]<typename T>(size_t count) {
      return elfldltl::ContainerArrayFromFile<elfldltl::StdContainer<std::vector>::Container<T>>(
          diag, "impossible")(count);
    };

    EXPECT_TRUE(elfldltl::WithLoadHeadersFromFile(diag, file, phdr_allocator, load,
                                                  elfldltl::ElfData::kNative, blob_info.machine));
  }

  // Stores the addresses of the symbols in the ELF binary that are used in tests.
  RestrictedSymbols restricted_symbols_;

  // Loads (and unloads) the ELF binary used in restricted mode. By making this
  // a member variable, we ensure that the ELF binary's lifetime is the
  // same as the symbol table. Note that loader_.Commit() is never called, and this
  // is what ensures the unmapping on destruction.
  elfldltl::LocalVmarLoader loader_;

  // Stores the ELF VMOs that can only be loaded once from bootfs.
  static std::unordered_map<elfldltl::ElfMachine, zx::vmo> elf_vmos_;
};

std::unordered_map<elfldltl::ElfMachine, zx::vmo> RestrictedBlob::elf_vmos_{};

class RestrictedMode : public zxtest::TestWithParam<RestrictedBlobInfo> {
 public:
  static void SetUpTestSuite() {
    restricted_blobs_ = std::make_unique<
        std::unordered_map<elfldltl::ElfMachine, std::unique_ptr<RestrictedBlob>>>();
    for (const auto info : kRestrictedBlobs) {
      std::unique_ptr<RestrictedBlob> blob = std::make_unique<RestrictedBlob>();
      blob->Initialize(info);
      restricted_blobs_->insert({info.machine, std::move(blob)});
    }
  }

  static void TearDownTestSuite() { restricted_blobs_.reset(); }

  // This function tests if machine-specific restricted mode is not supported
  // based on if it fails on what should be a valid state configuration.
  void ArchUnsupportedCheck() {
    NEEDS_NEXT_SKIP(zx_restricted_bind_state);
    NEEDS_NEXT_SKIP(zx_restricted_enter);

    // If there is no matching blob, it has been removed for lack of support.
    auto blob_entry = restricted_blobs_->find(restricted_blob_info_.machine);
    if (blob_entry == restricted_blobs_->end()) {
      ZXTEST_SKIP() << "hardware does not support blob:" << restricted_blob_info_.name;
      return;
    }

    // Do not test blobs that share a machine with the host machine.
    if (restricted_blob_info_.machine == elfldltl::ElfMachine::kNative) {
      return;
    }

    zx::vmo vmo;
    // Failures here will fail the test.
    ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
    auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

    // Configure the initial register state.
    auto state = ArchRegisterStateFactory::Create(restricted_blob_info_.machine);
    state->InitializeRegisters(GetTlsAddress(0));

    // Use a known-good pc with a minimal code path before returning.
    state->set_pc(restricted_symbols().addr_of("exception_bounce_exception_address"));

    // Write the state to the state VMO.
    ASSERT_OK(vmo.write(&state->restricted_state(), 0, sizeof(state->restricted_state())));

    zx_restricted_reason_t reason_code = 99;
    // Enter restricted mode with reasonable args that should return with a
    // reason_code.
    //
    // If the machine register state is unsupported, it will return ZX_ERR_BAD_STATE.
    if (restricted_enter_wrapper(0, reinterpret_cast<uintptr_t>(&restricted_exit), &reason_code) ==
        ZX_ERR_BAD_STATE) {
      auto blob = std::move(blob_entry->second);
      // Only test once, then remove the blob so we skip all other tests.
      restricted_blobs_->erase(blob_entry);
      blob.reset();
      ZXTEST_SKIP() << "hardware does not support blob:" << restricted_blob_info_.name;
    }
    // Ensure the reason code was changed.  If we failed with a different error,
    // there's still a problem.
    ASSERT_NE(reason_code, 99);
  }

  void SetUp() override {
    // Configure the machine which will be used for selecting the correct
    // register state.
    restricted_blob_info_ = GetParam();

    // This function allocates spaces for storing atomic ints and
    // TLS data that is shared with restricted mode.  It should be
    // mapped to a location that is reachable.
    MapSharedStorage(kRestrictedAtomicCount, kRestrictedThreadCount);

    // ELF architectures that differ from the host must be tested
    // for support because some processors do not support running
    // alternate architectures.
    ArchUnsupportedCheck();
  }

  void TearDown() override {
    zx::vmar::root_self()->unmap(thread_storage_base_, kThreadStorageSize);
    zx::vmar::root_self()->unmap(atomic_storage_base_, kAtomicStorageSize);
  }

  std::atomic_int* GetAtomicInt(unsigned int index) {
    assert(index < atomic_count_);
    std::atomic_int* ai = reinterpret_cast<std::atomic_int*>(atomic_storage_base_) + index;
    // Always reset when collected.
    *ai = 0;
    return ai;
  }

  TlsStorage* GetTlsAddress(uint64_t thread_id) {
    assert(thread_id < thread_count_);
    TlsStorage* tls_base =
        reinterpret_cast<TlsStorage*>(thread_storage_base_ + (kTlsStorageSize * thread_id));
    return tls_base;
  }

  std::unique_ptr<ArchRegisterState> GetArchRegisterState() {
    return ArchRegisterStateFactory::Create(restricted_blob_info_.machine);
  }

  size_t register_bytes() const {
    return restricted_blob_info_.machine == elfldltl::ElfMachine::kNative ? sizeof(uint64_t)
                                                                          : sizeof(uint32_t);
  }

 protected:
  // Returns the correct symbols for the current machine.
  const RestrictedSymbols& restricted_symbols() const {
    auto machine = restricted_blob_info_.machine;
    return restricted_blobs_->at(machine)->restricted_symbols();
  }

  // Storage size for the shared storage discussed below.
  static const uint32_t kAtomicStorageSize;
  static const uint32_t kThreadStorageSize;

  // This function allocates and maps storage for objects that are shared
  // between restricted mode and normal mode.  If the target machine requires
  // an upper limit on where shared objects may be allocated, it will be
  // enforced here.
  void MapSharedStorage(unsigned int atomics, unsigned int threads) {
    atomic_count_ = atomics;
    thread_count_ = threads;
    ASSERT_LT(sizeof(std::atomic_int) * atomics, kAtomicStorageSize);
    ASSERT_LT(kTlsStorageSize * threads, kThreadStorageSize);
    ASSERT_OK(zx::vmo::create(kAtomicStorageSize, 0, &atomic_storage_vmo_));
    ASSERT_OK(zx::vmo::create(kThreadStorageSize, 0, &thread_storage_vmo_));

    auto options = ZX_VM_PERM_READ | ZX_VM_PERM_WRITE;
    auto offset = restricted_blob_info_.max_load_address;
    if (offset != 0) {
      options |= ZX_VM_OFFSET_IS_UPPER_LIMIT;
      zx_info_vmar_t vmar_info = {};
      ASSERT_OK(zx::vmar::root_self()->get_info(ZX_INFO_VMAR, &vmar_info, sizeof(vmar_info),
                                                nullptr, nullptr));
      ASSERT_GE(offset, vmar_info.base);
      // Subtract the base from the absolute offset to get the relative offset
      // needed for zx_vmar_map().
      offset -= vmar_info.base;
      // Align it to the nearest page.
      offset -= offset % zx_system_get_page_size();
    }

    ASSERT_EQ(ZX_OK, zx::vmar::root_self()->map(options, offset, atomic_storage_vmo_, 0,
                                                kAtomicStorageSize, &atomic_storage_base_));
    ASSERT_EQ(ZX_OK, zx::vmar::root_self()->map(options, offset, thread_storage_vmo_, 0,
                                                kThreadStorageSize, &thread_storage_base_));
    if (offset != 0) {
      ASSERT_LT(reinterpret_cast<uint64_t>(atomic_storage_base_),
                restricted_blob_info_.max_load_address);
      ASSERT_LT(reinterpret_cast<uint64_t>(thread_storage_base_),
                restricted_blob_info_.max_load_address);
    }
  }

 private:
  // Contains the machine-specific restricted mode ELF blobs.
  // Initialized and destroyed with the test suite.
  static std::unique_ptr<std::unordered_map<elfldltl::ElfMachine, std::unique_ptr<RestrictedBlob>>>
      restricted_blobs_;

  // The target ELF machine is used to parameterize the tests.
  RestrictedBlobInfo restricted_blob_info_{};

  // VMO to hold thread local storage objects
  zx::vmo thread_storage_vmo_;

  // Atomic to hold shared atomic objects
  zx::vmo atomic_storage_vmo_;

  // Pointer to where thread_storage_vmo_ is mapped.
  zx_vaddr_t thread_storage_base_ = 0;

  // Pointer to where atomic_storage_vmo_ is mapped.
  zx_vaddr_t atomic_storage_base_ = 0;

  // The number of shared objects allocated.
  unsigned int thread_count_ = 0;
  unsigned int atomic_count_ = 0;
};

const uint32_t RestrictedMode::kAtomicStorageSize = 2048;
const uint32_t RestrictedMode::kThreadStorageSize = 2048;
std::unique_ptr<std::unordered_map<elfldltl::ElfMachine, std::unique_ptr<RestrictedBlob>>>
    RestrictedMode::restricted_blobs_ = {};

}  // namespace

INSTANTIATE_TEST_SUITE_P(RestrictedModePerArch, RestrictedMode, zxtest::ValuesIn(kRestrictedBlobs),
                         [](const zxtest::TestParamInfo<RestrictedMode::ParamType>& info) {
                           switch (info.param.machine) {
                             case elfldltl::ElfMachine::kX86_64:
                               return "x64";
                             case elfldltl::ElfMachine::kArm:
                               return "aarch32";
                             case elfldltl::ElfMachine::kAarch64:
                               return "aarch64";
                             case elfldltl::ElfMachine::kRiscv:
                               return "riscv64";
                             default:
                               return "unknown";
                           }
                         });

// Verify that restricted_enter handles invalid args.
TEST_P(RestrictedMode, EnterInvalidArgs) {
  NEEDS_NEXT_SKIP(zx_restricted_enter);

  // Invalid options.
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, zx_restricted_enter(0xffffffff, 0, 0));

  // Enter restricted mode with invalid args.
  // Vector table must be valid user pointer.
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, zx_restricted_enter(0, -1, 0));
}

TEST_P(RestrictedMode, BindState) {
  NEEDS_NEXT_SKIP(zx_restricted_bind_state);

  // Bad options.
  zx::vmo v_invalid;
  ASSERT_EQ(ZX_ERR_INVALID_ARGS, zx_restricted_bind_state(1, v_invalid.reset_and_get_address()));
  ASSERT_FALSE(v_invalid.is_valid());
  ASSERT_EQ(ZX_ERR_INVALID_ARGS, zx_restricted_bind_state(2, v_invalid.reset_and_get_address()));
  ASSERT_FALSE(v_invalid.is_valid());
  ASSERT_EQ(ZX_ERR_INVALID_ARGS,
            zx_restricted_bind_state(0xffffffff, v_invalid.reset_and_get_address()));
  ASSERT_FALSE(v_invalid.is_valid());

  // Happy case.
  zx::vmo vmo;
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

  // Binding again is fine and replaces any previously bound VMO.
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  ASSERT_TRUE(vmo.is_valid());

  // Map the vmo and verify the state follows.
  zx_vaddr_t ptr = 0;
  ASSERT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0,
                                       zx_system_get_page_size(), &ptr));
  zx_restricted_state_t* state2 = reinterpret_cast<zx_restricted_state_t*>(ptr);

  // Read the state out of the vmo and compare with memory map.
  zx_restricted_state_t state = {};
  ASSERT_OK(vmo.read(&state, 0, sizeof(state)));
  EXPECT_EQ(0, memcmp(state2, &state, sizeof(state)));

  // Fill the state with garbage and make sure it follows.
  memset(state2, 0x99, sizeof(state));
  ASSERT_OK(vmo.read(&state, 0, sizeof(state)));
  EXPECT_EQ(0, memcmp(state2, &state, sizeof(state)));

  // Write garbage via the write syscall and read it back out of the mapping.
  memset(&state, 0x55, sizeof(state));
  ASSERT_OK(vmo.write(&state, 0, sizeof(state)));
  EXPECT_EQ(0, memcmp(state2, &state, sizeof(state)));

  // Teardown the mapping.
  zx::vmar::root_self()->unmap(ptr, zx_system_get_page_size());
}

TEST_P(RestrictedMode, UnbindState) {
  NEEDS_NEXT_SKIP(zx_restricted_unbind_state);

  // Repeated unbind is OK.
  ASSERT_OK(zx_restricted_unbind_state(0));
  ASSERT_OK(zx_restricted_unbind_state(0));
  ASSERT_OK(zx_restricted_unbind_state(0));

  // Options must be 0.
  ASSERT_EQ(ZX_ERR_INVALID_ARGS, zx_restricted_unbind_state(1));
  ASSERT_EQ(ZX_ERR_INVALID_ARGS, zx_restricted_unbind_state(1));
  ASSERT_EQ(ZX_ERR_INVALID_ARGS, zx_restricted_unbind_state(0xffffffff));
}

// This is the happy case.
TEST_P(RestrictedMode, Basic) {
  NEEDS_NEXT_SKIP(zx_restricted_bind_state);

  zx::vmo vmo;
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

  // Configure the initial register state.
  auto state = GetArchRegisterState();
  state->InitializeRegisters(GetTlsAddress(0));

  // Set the PC to the syscall_bounce routine, as the PC is where zx_restricted_enter
  // will jump to.
  state->set_pc(restricted_symbols().addr_of("syscall_bounce"));

  // Write the state to the state VMO.
  ASSERT_OK(vmo.write(&state->restricted_state(), 0, sizeof(state->restricted_state())));

  // Enter restricted mode with reasonable args, expect a bounce back.
  zx_restricted_reason_t reason_code = 99;
  ASSERT_OK(
      restricted_enter_wrapper(0, reinterpret_cast<uintptr_t>(&restricted_exit), &reason_code));
  ASSERT_EQ(ZX_RESTRICTED_REASON_SYSCALL, reason_code);

  // Read the state out of the thread.
  ASSERT_OK(vmo.read(&state->restricted_state(), 0, sizeof(state->restricted_state())));

  state->VerifyArchSpecificRestrictedState();

  // Validate that the instruction pointer is right after the syscall instruction.
  EXPECT_EQ(restricted_symbols().addr_of("syscall_bounce_post_syscall"), state->pc());
  state->VerifyTwiddledRestrictedState(RegisterMutation::kFromSyscall);
}

// Verify that floating point state is saved correctly on context switch.
TEST_P(RestrictedMode, FloatingPointState) {
  NEEDS_NEXT_SKIP(zx_restricted_bind_state);

  constexpr uint32_t kNumRestrictedThreads = kRestrictedThreadCount;
  constexpr uint32_t kNumFloatingPointThreads = 32;
  std::atomic_int num_threads_ready = 0;
  std::atomic_int* num_threads_in_rmode = GetAtomicInt(0);
  std::atomic_int start_restricted_threads = 0;
  std::atomic_int* exit_restricted_mode = GetAtomicInt(1);
  zx_status_t statuses[kNumRestrictedThreads]{};
  memset(statuses, ZX_OK, sizeof(statuses));
  std::vector<std::thread> threads;

  auto thread_body = [this, exit_restricted_mode, &num_threads_ready, num_threads_in_rmode,
                      &start_restricted_threads, &statuses](uint32_t thread_num) {
    // Configure the initial register state.
    zx::vmo vmo;
    zx_status_t status = zx_restricted_bind_state(0, vmo.reset_and_get_address());
    if (status != ZX_OK) {
      statuses[thread_num] = status;
      return;
    }
    auto cleanup = fit::defer([]() { zx_restricted_unbind_state(0); });

    auto state = GetArchRegisterState();
    state->InitializeRegisters(this->GetTlsAddress(thread_num));
    uint64_t wait_then_syscall_addr = restricted_symbols().addr_of("wait_then_syscall");

    state->set_pc(wait_then_syscall_addr);
    state->set_arg_regs(reinterpret_cast<uint64_t>(num_threads_in_rmode),
                        reinterpret_cast<uint64_t>(exit_restricted_mode));

    // Write the state to the state VMO.
    status = vmo.write(&state->restricted_state(), 0, sizeof(state->restricted_state()));
    if (status != ZX_OK) {
      statuses[thread_num] = status;
      return;
    }

    // Wait for the main thread to tell us that we can write the FPU state and enter
    // restricted mode. We synchronize this to make sure that all of the restricted threads
    // modify their FPU state at the same time - context switching between the FPU write and
    // the entry into restricted mode can cause the threads to save the FPU state in normal
    // mode, which we do not want.
    num_threads_ready.fetch_add(1);
    while (start_restricted_threads.load() == 0) {
    }

    zx_restricted_reason_t reason_code = 99;
    // Construct the desired FPU state.
    std::unique_ptr<char[]> fpu_buffer = std::make_unique<char[]>(kFpuBufferSize);
    memset(fpu_buffer.get(), 0x10 + thread_num, kFpuBufferSize);
    // Allocate the buffer we're going to use to retrieve the FPU state after exiting
    // restricted mode. We do this before writing the desired FPU state as the process of
    // zeroing out the array modifies the floating-point registers on ARM.
    std::unique_ptr<char[]> got_fpu_buffer = std::make_unique<char[]>(kFpuBufferSize);
    load_fpu_registers(fpu_buffer.get());
    status =
        restricted_enter_wrapper(0, reinterpret_cast<uintptr_t>(&restricted_exit), &reason_code);
    if (status != ZX_OK) {
      statuses[thread_num] = status;
      return;
    }
    if (reason_code != ZX_RESTRICTED_REASON_SYSCALL) {
      ADD_FAILURE() << "thread " << thread_num << ": received reason code " << reason_code
                    << "instead of ZX_RESTRICTED_REASON_SYSCALL";
      // This is not a desired outcome. However, if it happens, logging
      // actionable information is helpful.
      if (reason_code == ZX_RESTRICTED_REASON_EXCEPTION) {
        zx_restricted_exception_t exception_state = {};
        status = vmo.read(&exception_state, 0, sizeof(exception_state));
        if (status == ZX_OK &&
            sizeof(exception_state.exception) == exception_state.exception.header.size) {
          ADD_FAILURE() << "thread " << thread_num << ": dumping exception state to stdout";
          state->PrintExceptionState(exception_state);
        }
      }
      statuses[thread_num] = ZX_ERR_BAD_STATE;
      return;
    }

    // Validate that the FPU contains the expected contents.
    store_fpu_registers(got_fpu_buffer.get());
    if (memcmp(fpu_buffer.get(), got_fpu_buffer.get(), kFpuBufferSize) != 0) {
      // Mark this test as a failure.
      statuses[thread_num] = ZX_ERR_BAD_STATE;

      // Print out the diff for easy debugging.
      for (uint16_t i = 0; i < kFpuBufferSize; i++) {
        if (got_fpu_buffer[i] != fpu_buffer[i]) {
          printf("thread %d: byte %u differs; got 0x%x, want 0x%x\n", thread_num, i,
                 got_fpu_buffer[i], fpu_buffer[i]);
        }
      }
    } else {
      statuses[thread_num] = ZX_OK;
    }
  };

  for (uint32_t i = 0; i < kNumRestrictedThreads; i++) {
    threads.emplace_back(thread_body, i);
  }

  // Wait for all the threads that will run restricted mode to spawn.
  while (num_threads_ready.load() != kNumRestrictedThreads) {
    // Check that each thread has successfully made it to the fetch_add that
    // increments num_threads_ready. This will ensure the test fails out
    // instead of hanging in those cases.
    for (uint32_t i = 0; i < kNumRestrictedThreads; i++) {
      ASSERT_OK(statuses[i]) << "thread " << i << " failed to bind state or write state VMO\n";
    }
  }
  // Tell all of the restricted threads to start.
  start_restricted_threads.store(1);

  // Wait for all of the restricted threads to enter restricted mode.
  while (num_threads_in_rmode->load() != kNumRestrictedThreads) {
    // Check that each thread has successfully made it to the fetch_add that
    // increments num_threads_in_rmode. This will ensure the test fails out
    // instead of hanging in those cases.
    for (uint32_t i = 0; i < kNumRestrictedThreads; i++) {
      ASSERT_OK(statuses[i]) << "thread " << i << " failed to make it into restricted mode\n";
    }
  }

  // Spawn a bunch of threads that overwrite the contents of the floating point registers.
  // We spawn enough threads to make sure that all CPU's floating point registers are overwritten.
  for (uint32_t i = 0; i < kNumFloatingPointThreads; i++) {
    threads.emplace_back(
        [](uint32_t thread_num) {
          std::unique_ptr<char[]> fpu_buffer = std::make_unique<char[]>(kFpuBufferSize);
          memset(fpu_buffer.get(), 0x90 + thread_num, kFpuBufferSize);
          load_fpu_registers(fpu_buffer.get());
        },
        i);
  }

  // Signal all of the restricted mode threads to exit, then wait for them to do so.
  exit_restricted_mode->store(1);
  for (auto& thread : threads) {
    thread.join();
  }
  for (uint32_t i = 0; i < kNumRestrictedThreads; i++) {
    ASSERT_OK(statuses[i]);
  }
}

// This is a simple benchmark test that prints some rough performance numbers.
TEST_P(RestrictedMode, Bench) {
  NEEDS_NEXT_SKIP(zx_restricted_bind_state);

  // Run the test 5 times to help filter out noise.
  for (auto i = 0; i < 5; i++) {
    zx::vmo vmo;
    ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
    auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

    // Set the state.
    auto state = GetArchRegisterState();
    state->InitializeRegisters(GetTlsAddress(0));
    state->set_pc(restricted_symbols().addr_of("syscall_bounce"));
    ASSERT_OK(vmo.write(&state->restricted_state(), 0, sizeof(state->restricted_state())));

    // Go through a full restricted syscall entry/exit cycle iter times and show the time.
    {
      zx_restricted_reason_t reason_code;
      auto t = zx::ticks::now();
      auto deadline = t + zx::ticks::per_second();
      int iter = 0;
      while (zx::ticks::now() <= deadline) {
        ASSERT_OK(restricted_enter_wrapper(0, reinterpret_cast<uintptr_t>(&restricted_exit),
                                           &reason_code));
        iter++;
      }
      t = zx::ticks::now() - t;

      printf("restricted call %ld ns per round trip (%ld raw ticks), %d iters\n",
             t / iter * ZX_SEC(1) / zx::ticks::per_second(), t.get(), iter);
    }

    {
      // For way of comparison, time a null syscall.
      auto t = zx::ticks::now();
      auto deadline = t + zx::ticks::per_second();
      int iter = 0;
      while (zx::ticks::now() <= deadline) {
        ASSERT_OK(zx_syscall_test_0());
        iter++;
      }
      t = zx::ticks::now() - t;

      printf("test syscall %ld ns per call (%ld raw ticks), %d iters\n",
             t / iter * ZX_SEC(1) / zx::ticks::per_second(), t.get(), iter);
    }

    // In-thread exception handling
    auto t = zx::ticks::now();
    auto deadline = t + zx::ticks::per_second();
    int iter = 0;
    zx_restricted_reason_t reason_code;
    state->set_pc(restricted_symbols().addr_of("exception_bounce_exception_address"));
    ASSERT_OK(vmo.write(&state->restricted_state(), 0, sizeof(state->restricted_state())));
    while (zx::ticks::now() <= deadline) {
      ASSERT_OK(
          restricted_enter_wrapper(0, reinterpret_cast<uintptr_t>(&restricted_exit), &reason_code));
      iter++;
    }
    t = zx::ticks::now() - t;

    printf("in-thread exceptions %ld ns per round trip (%ld raw ticks) %d iters\n",
           t / iter * ZX_SEC(1) / zx::ticks::per_second(), t.get(), iter);
  }
}

// Verify we can receive restricted exceptions using in-thread exception handlers.
TEST_P(RestrictedMode, InThreadException) {
  NEEDS_NEXT_SKIP(zx_restricted_bind_state);

  zx::vmo vmo;
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

  // Configure initial register state.
  auto state = GetArchRegisterState();
  state->InitializeRegisters(GetTlsAddress(0));
  state->set_pc(restricted_symbols().addr_of("exception_bounce"));

  // Enter restricted mode. The restricted code will twiddle some registers
  // and generate an exception.
  ASSERT_OK(vmo.write(&state->restricted_state(), 0, sizeof(state->restricted_state())));
  zx_restricted_reason_t reason_code = 99;
  ASSERT_OK(
      restricted_enter_wrapper(0, reinterpret_cast<uintptr_t>(&restricted_exit), &reason_code));

  ASSERT_EQ(ZX_RESTRICTED_REASON_EXCEPTION, reason_code);

  zx_restricted_exception_t exception_state = {};
  ASSERT_OK(vmo.read(&exception_state, 0, sizeof(exception_state)));
  EXPECT_EQ(sizeof(exception_state.exception), exception_state.exception.header.size);
  EXPECT_EQ(ZX_EXCP_UNDEFINED_INSTRUCTION, exception_state.exception.header.type);
  EXPECT_EQ(0u, exception_state.exception.context.synth_code);
  EXPECT_EQ(0u, exception_state.exception.context.synth_data);
#if defined(__x86_64__)
  EXPECT_EQ(0u, exception_state.exception.context.arch.u.x86_64.err_code);
  EXPECT_EQ(0u, exception_state.exception.context.arch.u.x86_64.cr2);
  // 0x6 corresponds to the invalid opcode vector.
  EXPECT_EQ(0x6u, exception_state.exception.context.arch.u.x86_64.vector);
#elif defined(__aarch64__)
  constexpr uint32_t kEsrIlBit = 1ull << 25;
  EXPECT_EQ(kEsrIlBit, exception_state.exception.context.arch.u.arm_64.esr);
  EXPECT_EQ(0u, exception_state.exception.context.arch.u.arm_64.far);
#elif defined(__riscv)
  EXPECT_EQ(0x2, exception_state.exception.context.arch.u.riscv_64.cause);
  EXPECT_EQ(0u, exception_state.exception.context.arch.u.riscv_64.tval);
#endif
}

// Verify that restricted_enter fails on invalid zx_restricted_state_t values.
TEST_P(RestrictedMode, EnterBadStateStruct) {
  NEEDS_NEXT_SKIP(zx_restricted_bind_state);

  zx::vmo vmo;
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

  auto state = GetArchRegisterState();
  state->InitializeRegisters(GetTlsAddress(0));

  [[maybe_unused]] auto set_state_and_enter = [&]() {
    // Set the state.
    ASSERT_OK(vmo.write(&state->restricted_state(), 0, sizeof(state->restricted_state())));

    // This should fail with bad state.
    ASSERT_EQ(ZX_ERR_BAD_STATE,
              zx_restricted_enter(0, reinterpret_cast<uintptr_t>(&restricted_exit), 0));
  };

  state->set_pc(-1);  // pc is outside of user space
  set_state_and_enter();

#ifdef __x86_64__
  state->InitializeRegisters(GetTlsAddress(0));
  state->set_pc(restricted_symbols().addr_of("syscall_bounce"));
  state->restricted_state().flags = (1UL << 31);  // set an invalid flag
  set_state_and_enter();

  state->InitializeRegisters(GetTlsAddress(0));
  state->set_pc(restricted_symbols().addr_of("syscall_bounce"));
  state->restricted_state().fs_base = (1UL << 63);  // invalid fs (non canonical)
  set_state_and_enter();

  state->InitializeRegisters(GetTlsAddress(0));
  state->set_pc(restricted_symbols().addr_of("syscall_bounce"));
  state->restricted_state().gs_base = (1UL << 63);  // invalid gs (non canonical)
  set_state_and_enter();
#endif

#ifdef __aarch64__
  state->InitializeRegisters(GetTlsAddress(0));
  state->set_pc(restricted_symbols().addr_of("syscall_bounce"));
  state->restricted_state().cpsr = 0x1;  // CPSR contains non-user settable flags.
  set_state_and_enter();
#endif
}

TEST_P(RestrictedMode, KickBeforeEnter) {
  NEEDS_NEXT_SKIP(zx_restricted_bind_state);

  zx::vmo vmo;
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

  // Configure the initial register state.
  auto state = GetArchRegisterState();
  state->InitializeRegisters(GetTlsAddress(0));

  // Set the PC to the syscall_bounce routine, as the PC is where zx_restricted_enter
  // will jump to.
  state->set_pc(restricted_symbols().addr_of("syscall_bounce"));

  // Write the state to the state VMO.
  ASSERT_OK(vmo.write(&state->restricted_state(), 0, sizeof(state->restricted_state())));

  zx::unowned<zx::thread> current_thread(thrd_get_zx_handle(thrd_current()));

  // Issue a kick on ourselves which should apply to the next attempt to enter restricted mode.
  uint32_t options = 0;
  ASSERT_OK(zx_restricted_kick(current_thread->get(), options));

  // Enter restricted mode with reasonable args, expect it to return due to kick and not run any
  // restricted mode code.
  uint64_t reason_code = 99;
  ASSERT_OK(
      restricted_enter_wrapper(0, reinterpret_cast<uintptr_t>(&restricted_exit), &reason_code));
  EXPECT_EQ(reason_code, ZX_RESTRICTED_REASON_KICK);

  // Read the state out of the thread.
  ASSERT_OK(vmo.read(&state->restricted_state(), 0, sizeof(state->restricted_state())));
  state->VerifyArchSpecificRestrictedState();

  // Validate that the instruction pointer is still pointing at the entry point.
  EXPECT_EQ(restricted_symbols().addr_of("syscall_bounce"), state->pc());

#if defined(__x86_64__)
  // Validate that the state is unchanged
  EXPECT_EQ(0x0101010101010101, state->restricted_state().rax);
#elif defined(__aarch64__)  // defined(__x86_64__)
  // Even aarch32 will show the initialized 64-bit value here since
  // it was never re-saved by zircon.
  EXPECT_EQ(0x0202020202020202, state->restricted_state().x[1]);
#elif defined(__riscv)      // defined(__aarch64__)
  EXPECT_EQ(0x0b0b0b0b0b0b0b0b, state->restricted_state().a1);
#endif                      // defined(__riscv)

  // Check that the kicked state is cleared
  ASSERT_OK(
      restricted_enter_wrapper(0, reinterpret_cast<uintptr_t>(&restricted_exit), &reason_code));
  EXPECT_EQ(reason_code, ZX_RESTRICTED_REASON_SYSCALL);

  // Read the state out of the thread.
  ASSERT_OK(vmo.read(&state->restricted_state(), 0, sizeof(state->restricted_state())));
  state->VerifyArchSpecificRestrictedState();

  // Validate that the instruction pointer is right after the syscall instruction.
  EXPECT_EQ(restricted_symbols().addr_of("syscall_bounce_post_syscall"), state->pc());

  // Validate that the value in first general purpose register is incremented.
  state->VerifyTwiddledRestrictedState(RegisterMutation::kFromSyscall);
}

TEST_P(RestrictedMode, KickWhileStartingAndExiting) {
  NEEDS_NEXT_SKIP(zx_restricted_kick);

  struct ExceptionChannelRegistered {
    std::condition_variable cv;
    std::mutex m;
    bool registered = false;
  };
  ExceptionChannelRegistered ec;

  struct ChildThreadStarted {
    std::condition_variable cv;
    std::mutex m;
    zx_koid_t koid = 0;
  };
  ChildThreadStarted ct;

  // Register a debugger exception channel so we can intercept thread lifecycle events
  // and issue restricted kicks. This runs on a child thread so that we can process events
  // while the main thread is blocked on the main thread starting and joining.
  std::thread exception_thread([&ec, &ct]() {
    zx::channel exception_channel;
    ASSERT_OK(zx::process::self()->create_exception_channel(ZX_EXCEPTION_CHANNEL_DEBUGGER,
                                                            &exception_channel));
    // Notify the main thread that the exception channel is registered so it knows when to
    // start the test thread.
    {
      std::lock_guard lock(ec.m);
      ec.registered = true;
      ec.cv.notify_one();
    }
    zx_koid_t child_koid;
    // Wait for the child thread to start and tell us its KOID.
    {
      std::unique_lock lock(ct.m);
      ct.cv.wait(lock, [&ct]() { return ct.koid != 0; });
      child_koid = ct.koid;
    }

    // Read exceptions out of the exception channel until we get the first one triggered by
    // the child thread. We do this to avoid a rare race condition in which a
    // ZX_EXCP_THREAD_EXITING triggered by a thread in another test case is delivered to the
    // process exception channel after this test case starts. See https://fxbug.dev/42078955
    // for more info.
    zx_exception_info_t info;
    zx::exception exception;
    zx_info_handle_basic_t handle_info = {};
    zx::thread thread;
    while (handle_info.koid != child_koid) {
      ASSERT_OK(exception_channel.wait_one(ZX_CHANNEL_READABLE, zx::time::infinite(), nullptr));
      ASSERT_OK(exception_channel.read(0, &info, exception.reset_and_get_address(), sizeof(info), 1,
                                       nullptr, nullptr));
      ASSERT_OK(exception.get_thread(&thread));
      size_t actual;
      size_t avail;
      ASSERT_OK(zx_object_get_info(thread.get(), ZX_INFO_HANDLE_BASIC, &handle_info,
                                   sizeof(handle_info), &actual, &avail));
      ASSERT_EQ(actual, 1);
      ASSERT_EQ(avail, 1);
    }

    // Starting child_thread should generate a ZX_EXCP_THREAD_STARTING message on our exception
    // channel.
    ASSERT_EQ(info.type, ZX_EXCP_THREAD_STARTING);

    uint32_t kick_options = 0;
    ASSERT_OK(zx_restricted_kick(thread.get(), kick_options));
    // Release the exception to let the thread start the rest of the way.
    exception.reset();

    // When the thread joins, we expect to receive a ZX_EXCP_THREAD_EXITING message on our exception
    // channel.
    ASSERT_OK(exception_channel.wait_one(ZX_CHANNEL_READABLE, zx::time::infinite(), nullptr));
    ASSERT_OK(exception_channel.read(0, &info, exception.reset_and_get_address(), sizeof(info), 1,
                                     nullptr, nullptr));
    ASSERT_EQ(info.type, ZX_EXCP_THREAD_EXITING);

    ASSERT_OK(exception.get_thread(&thread));
    // Since this thread is now in the DYING state, sending a restricted kick is expected to return
    // ZX_ERR_BAD_STATE.
    EXPECT_EQ(zx_restricted_kick(thread.get(), kick_options), ZX_ERR_BAD_STATE);
  });

  {
    std::unique_lock lock(ec.m);
    ec.cv.wait(lock, [&ec]() { return ec.registered; });
  }

  std::thread child_thread([this]() {
    zx::vmo vmo;
    ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
    auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

    auto state = GetArchRegisterState();
    state->InitializeRegisters(GetTlsAddress(0));
    state->set_pc(0u);

    ASSERT_OK(vmo.write(&state->restricted_state(), 0, sizeof(state->restricted_state())));

    // Attempting to enter restricted mode should return immediately with a kick.
    uint64_t reason_code = 99;
    ASSERT_OK(
        restricted_enter_wrapper(0, reinterpret_cast<uintptr_t>(&restricted_exit), &reason_code));
    EXPECT_EQ(reason_code, ZX_RESTRICTED_REASON_KICK);
  });

  // Get the KOID of the child thread and communicate it to the exception handler.
  auto child_handle = native_thread_get_zx_handle(child_thread.native_handle());
  size_t actual;
  size_t avail;
  zx_info_handle_basic_t info = {};
  ASSERT_OK(
      zx_object_get_info(child_handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), &actual, &avail));
  ASSERT_EQ(actual, 1);
  ASSERT_EQ(avail, 1);

  {
    std::lock_guard lock(ct.m);
    ct.koid = info.koid;
    ct.cv.notify_one();
  }

  child_thread.join();
  exception_thread.join();
}

TEST_P(RestrictedMode, KickWhileRunning) {
  NEEDS_NEXT_SKIP(zx_restricted_kick);

  zx::vmo vmo;
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

  // Configure the initial register state.
  auto state = GetArchRegisterState();
  std::atomic_int* flag = GetAtomicInt(0);
  state->InitializeRegisters(GetTlsAddress(0));
  state->set_pc(restricted_symbols().addr_of("store_one"));
  state->set_arg_regs(reinterpret_cast<uint64_t>(flag), 42);

  // Write the state to the state VMO.
  ASSERT_OK(vmo.write(&state->restricted_state(), 0, sizeof(state->restricted_state())));

  zx::unowned<zx::thread> current_thread(thrd_get_zx_handle(thrd_current()));

  // Start up a thread that will enter kick this thread once it detects that 'flag' has been
  // written to, indicating that r-mode code is running.
  std::thread kicker([flag, &current_thread] {
    // Wait for the first thread to write to 'flag' so we know it's in restricted mode.
    while (flag->load() == 0) {
    }
    // Kick it
    uint32_t options = 0;
    ASSERT_OK(zx_restricted_kick(current_thread->get(), options));
  });

  // Enter restricted mode and expect to tell us that it was kicked out.
  uint64_t reason_code = 99;
  ASSERT_OK(
      restricted_enter_wrapper(0, reinterpret_cast<uintptr_t>(&restricted_exit), &reason_code));
  EXPECT_EQ(reason_code, ZX_RESTRICTED_REASON_KICK);

  kicker.join();
  EXPECT_EQ(flag->load(), 1);

  // Read the state out of the thread.
  ASSERT_OK(vmo.read(&state->restricted_state(), 0, sizeof(state->restricted_state())));
  state->VerifyArchSpecificRestrictedState();

  // Expect to see second general purpose register incremented in the observed restricted state.
#if defined(__x86_64__)
  EXPECT_EQ(state->restricted_state().rsi, 43);
#elif defined(__aarch64__)  // defined(__x86_64__)
  EXPECT_EQ(state->restricted_state().x[1], 43);
#elif defined(__riscv)      // defined(__aarch64__)
  EXPECT_EQ(state->restricted_state().a1, 43);
#endif                      // defined(__riscv)
}

TEST_P(RestrictedMode, KickJustBeforeSyscall) {
  NEEDS_NEXT_SKIP(zx_restricted_kick);

  zx::vmo vmo;
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

  // Configure the initial register state.
  auto state = GetArchRegisterState();
  state->InitializeRegisters(GetTlsAddress(0));

  state->set_pc(restricted_symbols().addr_of("wait_then_syscall"));

  // Create atomic int 'signal' and 'wait_on'
  std::atomic_int* wait_on = GetAtomicInt(0);
  std::atomic_int* signal = GetAtomicInt(1);
  state->set_arg_regs(reinterpret_cast<uint64_t>(wait_on), reinterpret_cast<uint64_t>(signal));

  // Write the state to the state VMO.
  ASSERT_OK(vmo.write(&state->restricted_state(), 0, sizeof(state->restricted_state())));

  zx::unowned<zx::thread> current_thread(thrd_get_zx_handle(thrd_current()));

  std::thread kicker([wait_on, signal, &current_thread] {
    // Wait until the restricted mode thread is just about to issue a syscall.
    while (wait_on->load() == 0) {
    }
    // Suspend the restricted mode thread before we kick it so we can ensure that it doesn't
    // proceed before the kick is processed.
    zx::suspend_token token;
    ASSERT_OK(current_thread->suspend(&token));
    ASSERT_OK(current_thread->wait_one(ZX_THREAD_SUSPENDED, zx::time::infinite(), nullptr));
    // Issue a kick.
    uint32_t kick_options = 0;
    ASSERT_OK(zx_restricted_kick(current_thread->get(), kick_options));
    // Unsuspend the thread.
    token.reset();
    // Store a signal to release the restricted mode thread so it could issue a syscall
    // if it continues executing. We expect it to come out of thread suspend and process
    // the kick instead of continuing.
    signal->store(1);
  });
  uint64_t reason_code = 99;
  ASSERT_OK(
      restricted_enter_wrapper(0, reinterpret_cast<uintptr_t>(&restricted_exit), &reason_code));
  ASSERT_EQ(ZX_RESTRICTED_REASON_KICK, reason_code);
  kicker.join();
}
