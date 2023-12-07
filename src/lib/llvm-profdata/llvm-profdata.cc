// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/llvm-profdata/llvm-profdata.h>
#include <lib/stdcompat/span.h>
#include <zircon/assert.h>

#include <functional>

#ifndef HAVE_LLVM_PROFDATA
#error "build system regression"
#endif

// This is defined for a test build.
#ifdef HAVE_LLVM_PROFDATA_OVERRIDE
#undef HAVE_LLVM_PROFDATA
#define HAVE_LLVM_PROFDATA HAVE_LLVM_PROFDATA_OVERRIDE
#endif

#if !HAVE_LLVM_PROFDATA

// If not compiled with instrumentation at all, then all the link-time
// references in the real implementation below won't work.  So provide stubs.

void LlvmProfdata::Init(cpp20::span<const std::byte> build_id) {}

LlvmProfdata::LiveData LlvmProfdata::DoFixedData(cpp20::span<std::byte> data, bool match) {
  return {};
}

void LlvmProfdata::CopyLiveData(LiveData data) {}

void LlvmProfdata::MergeLiveData(LiveData data) {}

void LlvmProfdata::UseLiveData(LiveData data) {}

#else  // HAVE_LLVM_PROFDATA

#include <array>
#include <atomic>
#include <cstdint>
#include <cstring>

// TODO(fxbug.dev/131696): names changed in the .inc file Handle both old and
// new names by defining the old names as macros for the new ones. Remove these
// after the toolchain rolls in the new .inc file version.
#define DataSize NumData
#define CountersSize NumCounters

#include <profile/InstrProfData.inc>

namespace {

constexpr uint64_t kMagic = INSTR_PROF_RAW_MAGIC_64;

using IntPtrT = intptr_t;

enum ValueKind {
#define VALUE_PROF_KIND(Enumerator, Value, Descr) Enumerator = Value,
#include <profile/InstrProfData.inc>
};

struct __llvm_profile_data {
#define INSTR_PROF_DATA(Type, LLVMType, Name, Initializer) Type Name;
#include <profile/InstrProfData.inc>
};

extern "C" {

// This is sometimes emitted by the compiler with a different value.
// The header is expected to use whichever value this had at link time.
// This supplies the default value when the compiler doesn't supply it.
[[gnu::weak]] extern const uint64_t INSTR_PROF_RAW_VERSION_VAR = INSTR_PROF_RAW_VERSION;

// The compiler emits phantom references to this as a way to ensure
// that the runtime is linked in.
extern const int INSTR_PROF_PROFILE_RUNTIME_VAR = 0;

// In relocating mode, the compiler adds this to the address of a profiling
// counter in .bss for the counter to actually update.  At startup, this is
// zero so the .bss counters get updated.  When data is being published, the
// live-published counters get copied from the .bss counters and then this is
// set so future updates are redirected to the published copy.
//
// This definition is weak in case the standard profile runtime is also linked
// in with its own definition.
[[gnu::weak]] extern uintptr_t INSTR_PROF_PROFILE_COUNTER_BIAS_VAR = 0;

// These are outcalls made by the value-profiling instrumentation.  This
// runtime doesn't support value-profiling in any meaningful way.  But the
// entry points are provided so that instrumented code can be linked against
// this runtime instead of the standard runtime.  The stubs here are made weak
// in case both this and the standard runtime are linked in.

[[gnu::weak]] extern void INSTR_PROF_VALUE_PROF_FUNC(uint64_t TargetValue, void* Data,
                                                     uint32_t CounterIndex) {}

[[gnu::weak]] extern void INSTR_PROF_VALUE_PROF_MEMOP_FUNC(uint64_t TargetValue, void* Data,
                                                           uint32_t CounterIndex) {}

}  // extern "C"

// Here _WIN32 really means EFI.  At link-time, it's Windows/x64 essentially.
// InstrProfData.inc uses #ifdef _WIN32, so match that.
#if defined(_WIN32)

// These magic section names don't have macros in InstrProfData.inc,
// though their ".blah$M" counterparts do.

// Merge read-write sections into .data.
#pragma comment(linker, "/MERGE:.lprfc=.data")
#pragma comment(linker, "/MERGE:.lprfd=.data")

// Do not merge .lprfn and .lcovmap into .rdata.
// `llvm-cov` must be able to find them after the fact.

// Allocate read-only section bounds.
#pragma section(".lprfn$A", read)
#pragma section(".lprfn$Z", read)

// Allocate read-write section bounds.
#pragma section(".lprfd$A", read, write)
#pragma section(".lprfd$Z", read, write)
#pragma section(".lprfc$A", read, write)
#pragma section(".lprfc$Z", read, write)

// The ".blah$A" and ".blah$Z" placeholder sections get magically sorted with
// ".blah$M" in between them, so these symbols identify the bounds of the
// compiler-emitted data at link time.  The compiler seems to accept emitting
// a zero-length array if it has an explicit initializer.

// This data is morally `const`, i.e. it's a RELRO case in the ELF world.  But
// there is no RELRO in PE-COFF (?) so it's just a writable section and the
// compiler wants the declaration's constness to match #pragma section above.
[[gnu::section(".lprfd$A")]] static __llvm_profile_data DataBegin[0] = {};
[[gnu::section(".lprfd$Z")]] static __llvm_profile_data DataEnd[0] = {};

[[gnu::section(".lprfn$A")]] static const char NamesBegin[0] = {};
[[gnu::section(".lprfn$Z")]] static const char NamesEnd[0] = {};

[[gnu::section(".lprfc$A")]] static uint64_t CountersBegin[0] = {};
[[gnu::section(".lprfc$Z")]] static uint64_t CountersEnd[0] = {};

#ifdef INSTR_PROF_BITS_SECT_NAME
[[gnu::section(".lprfb$A")]] static char BitmapBegin[0] = {};
[[gnu::section(".lprfb$Z")]] static char BitmapEnd[0] = {};
#else
static char BitmapBegin[1] = {};
constexpr char* BitmapEnd = BitmapBegin;
#endif

#elif defined(__APPLE__)

extern "C" {

[[gnu::visibility("hidden")]] extern const __llvm_profile_data DataBegin[] __asm__(
    "section$start$__DATA$" INSTR_PROF_DATA_SECT_NAME);
[[gnu::visibility("hidden")]] extern const __llvm_profile_data DataEnd[] __asm__(
    "section$end$__DATA$" INSTR_PROF_DATA_SECT_NAME);

[[gnu::visibility("hidden")]] extern const char NamesBegin[] __asm__(
    "section$start$__DATA$" INSTR_PROF_NAME_SECT_NAME);
[[gnu::visibility("hidden")]] extern const char NamesEnd[] __asm__(
    "section$end$__DATA$" INSTR_PROF_NAME_SECT_NAME);

[[gnu::visibility("hidden")]] extern uint64_t CountersBegin[] __asm__(
    "section$start$__DATA$" INSTR_PROF_CNTS_SECT_NAME);
[[gnu::visibility("hidden")]] extern uint64_t CountersEnd[] __asm__(
    "section$end$__DATA$" INSTR_PROF_CNTS_SECT_NAME);

#ifdef INSTR_PROF_BITS_SECT_NAME
[[gnu::visibility("hidden")]] extern char BitmapBegin[] __asm__(
    "section$start$__DATA$" INSTR_PROF_BITS_SECT_NAME);
[[gnu::visibility("hidden")]] extern char BitmapEnd[] __asm__(
    "section$end$__DATA$" INSTR_PROF_BITS_SECT_NAME);
#else
static char BitmapBegin[1] = {};
constexpr char* BitmapEnd = BitmapBegin;
#endif

}  // extern "C"

#else  // Not _WIN32 or __APPLE__.

#ifndef __ELF__
#error "unsupported object file format???"
#endif

extern "C" {

// ELF linkers implicitly provide __start_SECNAME and __stop_SECNAME symbols
// when there is a SECNAME output section.  If selective instrumentation causes
// no actual metadata sections to be emitted, or even if all instrumentation
// sections in the input are in GC'd groups, then there is no such output
// section and so these symbols aren't defined.  In the userland runtime, this
// is handled simply by using weak references to the symbols.  However, those
// references require GOT slots for PIC-friendly links even with hidden
// visibility since there is no way for a PC-relative relocation to be resolved
// to absolute zero to indicate a missing value.  So instead, we need to ensure
// that there will be a zero-length section of the expected name that induces
// the linker to resolve the __start_SECNAME and __stop_SECNAME symbols.
// Having an explicit empty section with SHF_GNU_RETAIN accomplishes that
// without adding anything to the actual memory image.  Since the start and
// stop symbols are equal, the loops across them will just do nothing.

#define PROFDATA_SECTION(type, begin, end, section, writable)        \
  [[gnu::visibility("hidden")]] extern type begin[] __asm__(         \
      INSTR_PROF_QUOTE(INSTR_PROF_SECT_START(section)));             \
  [[gnu::visibility("hidden")]] extern type end[] __asm__(           \
      INSTR_PROF_QUOTE(INSTR_PROF_SECT_STOP(section)));              \
  __asm__(".pushsection " INSTR_PROF_QUOTE(section) ",\"aR" writable \
                                                    "\",%progbits\n" \
                                                    ".popsection")

PROFDATA_SECTION(const __llvm_profile_data, DataBegin, DataEnd, INSTR_PROF_DATA_COMMON, "");

PROFDATA_SECTION(const char, NamesBegin, NamesEnd, INSTR_PROF_NAME_COMMON, "");

PROFDATA_SECTION(uint64_t, CountersBegin, CountersEnd, INSTR_PROF_CNTS_COMMON, "w");

PROFDATA_SECTION(char, BitmapBegin, BitmapEnd, INSTR_PROF_BITS_COMMON, "w");

}  // extern "C"

#endif  // Not _WIN32 or __APPLE__.

struct ProfRawHeader {
  size_t binary_ids_size() const {
    if constexpr (INSTR_PROF_RAW_VERSION < 6) {
      return 0;
    } else {
      return static_cast<size_t>(BinaryIdsSize);
    }
  }

#define INSTR_PROF_RAW_HEADER(Type, Name, Initializer) Type Name;
#include <profile/InstrProfData.inc>
};

constexpr size_t kAlignAfterBuildId = sizeof(uint64_t);

constexpr size_t PaddingSize(size_t chunk_size_bytes) {
  return (kAlignAfterBuildId - (chunk_size_bytes % kAlignAfterBuildId)) % kAlignAfterBuildId;
}

constexpr size_t PaddingSize(cpp20::span<const std::byte> chunk) {
  return PaddingSize(chunk.size_bytes());
}

constexpr size_t BinaryIdsSize(cpp20::span<const std::byte> build_id) {
  if (build_id.empty()) {
    return 0;
  }
  return sizeof(uint64_t) + build_id.size_bytes() + PaddingSize(build_id);
}

[[gnu::const]] cpp20::span<const __llvm_profile_data> ProfDataArray() {
  return {
      DataBegin,
      (reinterpret_cast<const std::byte*>(DataEnd) - reinterpret_cast<const std::byte*>(DataBegin) +
       sizeof(__llvm_profile_data) - 1) /
          sizeof(__llvm_profile_data),
  };
}

// This is the .bss data that gets updated live by instrumented code when the
// bias is set to zero.
[[gnu::const]] cpp20::span<uint64_t> ProfCountersData() {
  return cpp20::span<uint64_t>(CountersBegin, CountersEnd - CountersBegin);
}

[[gnu::const]] cpp20::span<char> ProfBitmapData() {
  return cpp20::span<char>(BitmapBegin, BitmapEnd - BitmapBegin);
}

[[gnu::const]] ProfRawHeader GetHeader(cpp20::span<const std::byte> build_id) {
  // These are used by the INSTR_PROF_RAW_HEADER initializers.
  const uint64_t NumData = ProfDataArray().size();
  const uint64_t PaddingBytesBeforeCounters = 0;
  const uint64_t NumCounters = ProfCountersData().size();
  const uint64_t PaddingBytesAfterCounters = 0;
  // TODO(fxbug.dev/133950): These are not used by older InstrProfData.inc
  // versions but are used by newer ones.  Remove the [[maybe_unused]]
  // attributes after a new version has rolled and stuck.
  [[maybe_unused]] const uint64_t NumBitmapBytes = ProfBitmapData().size();
  [[maybe_unused]] const uint64_t PaddingBytesAfterBitmapBytes = 0;
  const uint64_t NamesSize = NamesEnd - NamesBegin;
  auto __llvm_profile_get_magic = []() -> uint64_t { return kMagic; };
  auto __llvm_profile_get_version = []() -> uint64_t { return INSTR_PROF_RAW_VERSION_VAR; };
  auto __llvm_write_binary_ids = [build_id](void* ignored) -> uint64_t {
    ZX_DEBUG_ASSERT(ignored == nullptr);
    return BinaryIdsSize(build_id);
  };

  return {
#define INSTR_PROF_RAW_HEADER(Type, Name, Initializer) .Name = Initializer,
#include <profile/InstrProfData.inc>
  };
}

// Don't publish anything if no functions were actually instrumented.
[[gnu::const]] bool NoData() { return ProfCountersData().empty() && ProfBitmapData().empty(); }

template <typename T, template <typename> class Op>
void MergeData(cpp20::span<std::byte> to, cpp20::span<const std::byte> from) {
  ZX_ASSERT(to.size_bytes() == from.size_bytes());
  ZX_ASSERT(to.size_bytes() % sizeof(T) == 0);

  cpp20::span to_data{reinterpret_cast<T*>(to.data()), to.size_bytes() / sizeof(T)};

  cpp20::span from_data{reinterpret_cast<const T*>(from.data()), from.size_bytes() / sizeof(T)};

  constexpr Op<T> op;
  for (size_t i = 0; i < to_data.size(); ++i) {
    to_data[i] = op(to_data[i], from_data[i]);
  }
}

template <typename T, template <typename> class Op, typename FromT>
void MergeSelfData(cpp20::span<std::byte> to, cpp20::span<FromT> from, const char* what) {
  ZX_ASSERT_MSG(to.size_bytes() >= from.size_bytes(),
                "merging %zu bytes of %s with only %zu bytes left!", from.size_bytes(), what,
                to.size_bytes());
  MergeData<T, Op>(to.subspan(0, from.size_bytes()), cpp20::as_bytes(from));
}

}  // namespace

void LlvmProfdata::Init(cpp20::span<const std::byte> build_id) {
  build_id_ = build_id;

  if (NoData()) {
    return;
  }

  // The sequence and sizes here should match the PublishLiveData() code.

  const ProfRawHeader header = GetHeader(build_id_);

  counters_offset_ = sizeof(header) + header.binary_ids_size() +
                     (static_cast<size_t>(header.NumData) * sizeof(__llvm_profile_data)) +
                     static_cast<size_t>(header.PaddingBytesBeforeCounters);
  counters_size_bytes_ = static_cast<size_t>(header.NumCounters) * sizeof(uint64_t);
  ZX_ASSERT(counters_size_bytes_ == ProfCountersData().size_bytes());

  size_bytes_ = counters_offset_ + counters_size_bytes_ +
                static_cast<size_t>(header.PaddingBytesAfterCounters);

  const size_t PaddingBytesAfterNames = PaddingSize(static_cast<size_t>(header.NamesSize));
  size_bytes_ += header.NamesSize + PaddingBytesAfterNames;
}

LlvmProfdata::LiveData LlvmProfdata::DoFixedData(cpp20::span<std::byte> data, bool match) {
  if (size_bytes_ == 0) {
    return {};
  }

  // Write bytes at the start of data and then advance data to be the remaining
  // subspan where the next call will write its data.  When merging, this
  // doesn't actually write but instead asserts that the destination already
  // has identical contents.
  auto write_bytes = [&](cpp20::span<const std::byte> bytes, const char* what) {
    ZX_ASSERT_MSG(data.size_bytes() >= bytes.size_bytes(),
                  "%s of %zu bytes with only %zu bytes left!", what, bytes.size_bytes(),
                  data.size_bytes());
    if (match) {
      ZX_ASSERT_MSG(!memcmp(data.data(), bytes.data(), bytes.size()),
                    "mismatch somewhere in %zu bytes of %s", bytes.size(), what);
    } else {
      memcpy(data.data(), bytes.data(), bytes.size());
    }
    data = data.subspan(bytes.size());
  };

  constexpr std::array<std::byte, sizeof(uint64_t)> kPaddingBytes{};
  const cpp20::span kPadding(kPaddingBytes);
  constexpr const char* kPaddingDoc = "alignment padding";

  // These are all the chunks to be written.
  // The sequence and sizes here must match the size_bytes() code.

  const ProfRawHeader header = GetHeader(build_id_);
  write_bytes(cpp20::as_bytes(cpp20::span{&header, 1}), "INSTR_PROF_RAW_HEADER");

  const uint64_t build_id_size = build_id_.size_bytes();
  if (build_id_size > 0) {
    write_bytes(cpp20::as_bytes(cpp20::span{&build_id_size, 1}), "build ID size");
    write_bytes(cpp20::as_bytes(build_id_), "build ID");
    write_bytes(kPadding.subspan(0, PaddingSize(build_id_)), kPaddingDoc);
  }

  auto prof_data = cpp20::span(DataBegin, DataEnd - DataBegin);
  write_bytes(cpp20::as_bytes(prof_data), INSTR_PROF_DATA_SECT_NAME);
  write_bytes(kPadding.subspan(0, static_cast<size_t>(header.PaddingBytesBeforeCounters)),
              kPaddingDoc);

  // Skip over the space in the data blob for the counters.
  ZX_ASSERT(counters_size_bytes_ == ProfCountersData().size_bytes());
  ZX_ASSERT_MSG(data.size_bytes() >= counters_size_bytes_,
                "%zu bytes of counters with only %zu bytes left!", counters_size_bytes_,
                data.size_bytes());
  cpp20::span counters_data = data.subspan(0, counters_size_bytes_);
  data = data.subspan(counters_size_bytes_);
  write_bytes(kPadding.subspan(0, static_cast<size_t>(header.PaddingBytesAfterCounters)),
              kPaddingDoc);

  cpp20::span<std::byte> bitmap_data;
#if INSTR_PROF_RAW_VERSION >= 9
  // Skip over the space in the data blob for the bitmap bytes.
  ZX_ASSERT(bitmap_size_bytes_ == ProfBitmapData().size_bytes());
  ZX_ASSERT_MSG(data.size_bytes() >= bitmap_size_bytes_,
                "%zu bytes of bitmap with only %zu bytes left!", bitmap_size_bytes_,
                data.size_bytes());
  bitmap_data = data.subspan(0, bitmap_size_bytes_);
  data = data.subspan(bitmap_size_bytes_);
  write_bytes(kPadding.subspan(0, static_cast<size_t>(header.PaddingBytesAfterBitmapBytes)),
              kPaddingDoc);
#endif

  auto prof_names = cpp20::span(NamesBegin, NamesEnd - NamesBegin);
  const size_t PaddingBytesAfterNames = PaddingSize(static_cast<size_t>(header.NamesSize));
  write_bytes(cpp20::as_bytes(prof_names), INSTR_PROF_NAME_SECT_NAME);
  write_bytes(kPadding.subspan(0, PaddingBytesAfterNames), kPaddingDoc);

  return {counters_data, bitmap_data};
}

void LlvmProfdata::CopyLiveData(LiveData data) {
  auto prof_counters = ProfCountersData();
  ZX_ASSERT_MSG(data.counters.size_bytes() >= prof_counters.size_bytes(),
                "writing %zu bytes of counters with only %zu bytes left!",
                data.counters.size_bytes(), data.counters.size_bytes());
  if (!prof_counters.empty()) {
    memcpy(data.counters.data(), prof_counters.data(), prof_counters.size_bytes());
  }

  auto prof_bitmap = ProfBitmapData();
  ZX_ASSERT_MSG(data.bitmap.size_bytes() >= prof_bitmap.size_bytes(),
                "writing %zu bytes of bitmap with only %zu bytes left!", data.bitmap.size_bytes(),
                data.bitmap.size_bytes());
  if (!prof_bitmap.empty()) {
    memcpy(data.bitmap.data(), prof_bitmap.data(), prof_bitmap.size_bytes());
  }
}

// Instead of copying, merge the old counters with our values by summation and
// the old bitmap by bitwise OR.
void LlvmProfdata::MergeLiveData(LiveData data) {
  MergeSelfData<uint64_t, std::plus>(data.counters, ProfCountersData(), "counters");
}

void LlvmProfdata::MergeLiveData(LiveData to, LiveData from) {
  MergeData<uint64_t, std::plus>(to.counters, from.counters);
  MergeData<char, std::bit_or>(to.bitmap, from.bitmap);
}

void LlvmProfdata::UseCounters(cpp20::span<std::byte> data) {
  auto prof_counters = ProfCountersData();
  ZX_ASSERT_MSG(data.size_bytes() >= prof_counters.size_bytes(),
                "cannot relocate %zu bytes of counters with only %zu bytes left!",
                prof_counters.size_bytes(), data.size_bytes());

  const uintptr_t old_addr = reinterpret_cast<uintptr_t>(prof_counters.data());
  const uintptr_t new_addr = reinterpret_cast<uintptr_t>(data.data());
  ZX_ASSERT(new_addr % kAlign == 0);
  const uintptr_t counters_bias = new_addr - old_addr;

  // Now that the data has been copied (or merged), start updating the new
  // copy.  These compiler barriers should ensure we've finished all the
  // copying before updating the bias that the instrumented code uses.
  std::atomic_signal_fence(std::memory_order_seq_cst);
  INSTR_PROF_PROFILE_COUNTER_BIAS_VAR = counters_bias;
  std::atomic_signal_fence(std::memory_order_seq_cst);
}

void LlvmProfdata::UseLiveData(LiveData data) {
  ZX_ASSERT_MSG(data.bitmap.empty(), "bitmap bytes cannot be relocated");
  UseCounters(data.counters);
}

void LlvmProfdata::UseLinkTimeLiveData() {
  std::atomic_signal_fence(std::memory_order_seq_cst);
  INSTR_PROF_PROFILE_COUNTER_BIAS_VAR = 0;
  std::atomic_signal_fence(std::memory_order_seq_cst);
}

cpp20::span<const std::byte> LlvmProfdata::BuildIdFromRawProfile(
    cpp20::span<const std::byte> data) {
  ProfRawHeader header;
  if (data.size() < sizeof(header)) {
    return {};
  }
  memcpy(&header, data.data(), sizeof(header));
  data = data.subspan(sizeof(header));

  if (header.Magic != kMagic || header.Version < 7) {
    return {};
  }

  if (header.binary_ids_size() == 0 || header.binary_ids_size() > data.size()) {
    return {};
  }
  data = data.subspan(0, header.binary_ids_size());

  uint64_t build_id_size;
  if (data.size() < sizeof(build_id_size)) {
    return {};
  }
  memcpy(&build_id_size, data.data(), sizeof(build_id_size));
  data = data.subspan(sizeof(build_id_size));

  if (data.size() < build_id_size) {
    return {};
  }
  return data.subspan(0, static_cast<size_t>(build_id_size));
}

bool LlvmProfdata::Match(cpp20::span<const std::byte> data) {
  cpp20::span id = BuildIdFromRawProfile(data);
  return !id.empty() && id.size_bytes() == build_id_.size_bytes() &&
         !memcmp(id.data(), build_id_.data(), build_id_.size_bytes());
}

#endif  // HAVE_LLVM_PROFDATA
