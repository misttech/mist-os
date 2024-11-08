// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_SYMBOL_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_SYMBOL_H_

#include <cstdint>
#include <optional>
#include <span>
#include <string_view>
#include <type_traits>

#include "abi-span.h"
#include "compat-hash.h"
#include "gnu-hash.h"
#include "layout.h"

namespace elfldltl {

// SymbolName represents an identifier to be looked up in a symbol table.  It's
// really just a string_view with a cache of the string's hash value(s).  The
// type is constexpr friendly and when used in a constexpr context with a
// string literal, it can precompute at compile time to optimize.
//
// The Lookup calls are just front-ends that take a SymbolInfo object and call
// its Lookup method (see below).
//
// Note that though this is a cheaply-copyable type, it's always best to pass
// it by mutable reference so its cache can be updated as needed (outside
// constexpr context).  Lookup has both const and non-const overloads, but the
// const overload has to recompute every time if the hash isn't already cached.
//
// ELF symbol names cannot contain NUL characters, so it's an invariant that
// SymbolName cannot contain embedded NULs (unlike std::string_view, which can).
// Construction and assignment enforce this by turning a std::string_view argument
// that contains embedded NULs into the empty string.
//
class SymbolName : public std::string_view {
 public:
  using std::string_view::string_view;

  constexpr SymbolName() = default;

  constexpr SymbolName(const SymbolName&) = default;

  // When constructing from a constant, precompute the hashes since it can be
  // done entirely in constexpr context.
  template <size_t N>
  constexpr explicit SymbolName(const char (&name)[N])
      : std::string_view(name),
        compat_hash_(CompatHashString(*this)),
        gnu_hash_(GnuHashString(*this)) {}

  // This will precompute the hashes in constexpr context, see below.
  constexpr explicit SymbolName(std::string_view name) { *this = name; }

  constexpr explicit SymbolName(const char* name) { *this = name; }

  // Convenient constructor using a symbol table entry (see below).
  template <class SymbolInfo, class Sym>
  constexpr SymbolName(const SymbolInfo& si, const Sym& sym) : SymbolName(si.string(sym.name)) {}

  constexpr SymbolName& operator=(const SymbolName&) = default;

  constexpr SymbolName& operator=(std::string_view name) {
    // No valid symbol will have an embedded NUL character, in which case just yield an empty
    // symbol.
    if (!name.empty() && name.find_first_of('\0') != std::string_view::npos) [[unlikely]] {
      name = {};
    }
    return ChangeName(name);
  }

  constexpr SymbolName& operator=(const char* name) {
    // Implicit const char* -> std::string_view conversion makes it impossible
    // for there to be an embedded NUL, so no need to check that invariant.
    return ChangeName(name);
  }

  constexpr uint32_t compat_hash() {
    if (compat_hash_ == kCompatNoHash) {
      compat_hash_ = CompatHashString(*this);
    }
    return compat_hash_;
  }

  constexpr uint32_t compat_hash() const { return SymbolName(*this).compat_hash(); }

  constexpr uint32_t gnu_hash() {
    if (gnu_hash_ == kGnuNoHash) {
      gnu_hash_ = GnuHashString(*this);
    }
    return gnu_hash_;
  }

  constexpr uint32_t gnu_hash() const { return SymbolName(*this).gnu_hash(); }

  template <class SymbolInfoType, typename Filter>
  constexpr const typename SymbolInfoType::Sym* Lookup(const SymbolInfoType& si, Filter&& filter) {
    // DT_GNU_HASH format is superior when available.  Modern systems should
    // default to --hash-style=gnu or --hash-style=both so it's available.
    if (auto gnu = si.gnu_hash()) {
      return si.Lookup(*gnu, *this, gnu_hash(), std::forward<Filter>(filter));
    }

    // But it's easy enough to support the old format (--hash-style=sysv) too.
    if (auto compat = si.compat_hash()) {
      return si.Lookup(*compat, *this, compat_hash(), std::forward<Filter>(filter));
    }

    return nullptr;
  }

  // A const object can't update its cache, but constexpr will already have it.
  template <class SymbolInfoType, typename Filter>
  constexpr const typename SymbolInfoType::Sym* Lookup(const SymbolInfoType& si,
                                                       Filter&& filter) const {
    // The copy is mutable in case we don't already have cached hash values.
    return SymbolName(*this).Lookup(si, std::forward<Filter>(filter));
  }

  template <class SymbolInfoType>
  constexpr auto Lookup(const SymbolInfoType& si) {
    return Lookup(si, SymbolInfoType::DefinedSymbol);
  }

  template <class SymbolInfoType>
  constexpr auto Lookup(const SymbolInfoType& si) const {
    return Lookup(si, SymbolInfoType::DefinedSymbol);
  }

 private:
  constexpr SymbolName& ChangeName(std::string_view name) {
    std::string_view::operator=(name);
    compat_hash_ = kCompatNoHash;
    gnu_hash_ = kGnuNoHash;
    if (std::is_constant_evaluated()) {  // Precompute in constexpr.
      compat_hash();
      gnu_hash();
    }
    return *this;
  }

  uint32_t compat_hash_ =  // Precompute in constexpr.
      std::is_constant_evaluated() ? CompatHashString(*this) : kCompatNoHash;
  uint32_t gnu_hash_ =  // Precompute in constexpr.
      std::is_constant_evaluated() ? GnuHashString(*this) : kGnuNoHash;
};

// This type can be used as a constructor tag to zero-construct an object whose
// default constructor would otherwise not be zero initializable. This can
// allow and object to be placed in bss. See
// `SymbolInfo::SymbolInfo(LinkerZeroInitialized)` for more.
struct LinkerZeroInitialized {};
inline constexpr LinkerZeroInitialized kLinkerZeroInitialized{};

// This represents all the dynamic symbol table information for one ELF file.
// It's primarily used for hash table lookup via SymbolName::Lookup, but can
// also be used to enumerate the symbol table or the hash tables.  It holds
// non-owning pointers into target data normally found in the RODATA segment.
//
template <class ElfLayout, class AbiTraits = LocalAbiTraits>
class SymbolInfo {
 public:
  using Elf = ElfLayout;
  using Word = typename Elf::Word;
  using Addr = typename Elf::Addr;
  using size_type = typename Elf::size_type;
  using Sym = typename Elf::Sym;

  constexpr SymbolInfo() = default;

  // This constructor can be used to zero-initialize a SymbolInfo object.
  // This can be useful for performance reasons. Note, a SymbolInfo object in
  // this state must never be used until `InitLinkerZeroInitialized` has been
  // called.
  constexpr explicit SymbolInfo(LinkerZeroInitialized) : strtab_{} {}

  constexpr void InitLinkerZeroInitialized() { strtab_ = kEmptyStrtab; }

  // Each flavor of hash table has a support class with a compatible API,
  // except for the argument to the constructor and Valid, which is a
  // span<Word> for DT_HASH and a span<Addr> for DT_GNU_HASH.
  //
  // * `static bool Valid(span table);` returns true if the table is usable.
  //   If this returns true, it's safe to pass `table` to the constructor.
  //
  // * `uint32_t symtab_size() const;` computes the maximum size of the symbol
  //   table.  This is not normally needed for plain lookups; it may be costly.
  //
  // * `uint32_t Bucket(uint32_t hash) const;` returns the hash bucket for
  //   symbol names with the given hash value.  Bucket number zero is invalid.
  //   This can be returned if no buckets contain this hash value.
  //
  // * `BucketIterator` is a forward-iterator type that has a three-argument
  //   constructor `(const Table&, uint32_t bucket, uint32_t hash)` that yields
  //   a "begin" iterator for the hash bucket and a two-argument constructor
  //   that yields an "end" iterator for the hash bucket.  The iterator yields
  //   a nonzero uint32_t symbol table index.
  //
  // * `<some type> begin(); const` and `<some type> end(); const` return
  //   iterators over the set of buckets, whose operator*() returns a
  //   BucketIterator, such that iterating through from begin() to end() with
  //   an inner iteration through each BucketIterator to its end state from
  //   there exhaustively visits every symbol in the whole hash table exactly
  //   once.  This is only used for diagnostic purposes.
  //
  using CompatHash = ::elfldltl::CompatHash<Elf, AbiTraits>;  // compat-hash.h
  using GnuHash = ::elfldltl::GnuHash<Elf, AbiTraits>;        // gnu-hash.h

  // This is a forward-iterable container view of a symbol table hash bucket.
  // Each uint32_t element is a symbol table index.
  template <class HashTable>
  class HashBucket {
   public:
    using iterator = typename HashTable::BucketIterator;
    using const_iterator = iterator;

    constexpr explicit HashBucket(const HashTable& table, iterator first)
        : begin_(first), end_(table) {}

    constexpr explicit HashBucket(const HashTable& table, uint32_t bucket, uint32_t hash)
        : HashBucket(table, iterator(table, bucket, hash)) {}

    constexpr iterator begin() const { return begin_; }
    constexpr iterator end() const { return end_; }

   private:
    iterator begin_, end_;
  };

  // This is the degenerate (always true) filter predicate for Lookup.
  static constexpr bool AnySymbol(const Sym& sym) { return true; }

  // This is the default filter predicate for Lookup to match defined symbols.
  static constexpr bool DefinedSymbol(const Sym& sym) {
    if (sym.shndx != 0) {
      switch (sym.type()) {
        case ElfSymType::kNoType:
        case ElfSymType::kObject:
        case ElfSymType::kFunc:
        case ElfSymType::kCommon:
        case ElfSymType::kTls:
        case ElfSymType::kIfunc:
          return true;
        default:
          break;
      }
    }
    return false;
  }

  // Look up a symbol in one of the hash tables.  The filter is a predicate to
  // accept or reject symbols before name matching.
  // This takes a SymbolName to enforce the invariant that there are no embedded NUL characters.
  // It's hash fields are not used.
  template <class HashTable, typename Filter>
  constexpr const Sym* Lookup(const HashTable& table, const SymbolName& name, uint32_t hash,
                              Filter&& filter = DefinedSymbol) const {
    static_assert(std::is_invocable_r_v<bool, Filter, const Sym&>);
    const uint32_t bucket = table.Bucket(hash);
    if (bucket != 0 && name.size() && name.size() < strtab().size()) {
      for (uint32_t i : HashBucket<HashTable>(table, bucket, hash)) {
        if (i >= symtab_.size()) [[unlikely]] {
          break;
        }
        const Sym& sym = symtab_[i];
        // TODO(mcgrathr): diag for bad st_name
        if (filter(sym) && sym.name < strtab().size() && strtab().size() - name.size() > sym.name &&
            strtab()[sym.name + name.size()] == '\0' &&
            strtab().substr(sym.name, name.size()) == name) {
          return &sym;
        }
      }
    }
    return nullptr;
  }

  // Fetch the raw string table.
  constexpr std::string_view strtab() const { return strtab_; }

  // Fetch a NUL-terminated string from the string table by offset,
  // e.g. as stored in st_name or DT_SONAME.
  constexpr const char* string(size_t offset) const {
    if (strtab().size() && offset < strtab().size() - 1) [[likely]] {
      return strtab().data() + offset;
    }
    return "";
  }

  // Fetch the raw symbol table.  Note this size may be an upper bound.  It's
  // all valid memory to read, but there might be garbage data past the last
  // actual valid symbol table index.
  constexpr std::span<const Sym> symtab() const { return symtab_; }

  // Fetch the symbol table and try to reduce its apparent size to its real
  // size or at least a better approximation.  This provides no guarantee that
  // the size will be smaller than the raw symtab() size, but does a bit more
  // work to try to ensure it.  If using only indices that are presumed to be
  // valid, such as those in relocation entries, just use symtab() instead.
  // This is better for blind enumeration.
  std::span<const Sym> safe_symtab() const { return symtab_.subspan(0, safe_symtab_size()); }

  // Return the CompatHash object (see compat-hash.h) if DT_HASH is present.
  constexpr std::optional<CompatHash> compat_hash() const {
    if (CompatHash::Valid(compat_hash_)) {
      return CompatHash(compat_hash_);
    }
    return {};
  }

  constexpr std::optional<GnuHash> gnu_hash() const {
    if (GnuHash::Valid(gnu_hash_)) {
      return GnuHash(gnu_hash_);
    }
    return {};
  }

  constexpr std::string_view soname() const {
    if (soname_ != 0) {
      return string(soname_);
    }
    return {};
  }

  // Return the DT_FLAGS bits.
  constexpr size_type flags() const { return flags_; }

  // Return the DT_FLAGS_1 bits.
  constexpr size_type flags1() const { return flags1_; }

  // Install data for the various tables.  These return *this so they can be
  // called in fluent style, e.g. in a constexpr initializer.

  constexpr SymbolInfo& set_strtab(std::string_view strtab) {
    if (strtab.empty() || strtab.back() != '\0') {
      // Invalid string table has no NUL terminator; don't use it at all.
      strtab = kEmptyStrtab;
    }
    strtab_ = strtab;
    return *this;
  }

  constexpr SymbolInfo& set_strtab_as_span(std::span<const char> strtab) {
    return set_strtab({strtab.data(), strtab.size()});
  }

  constexpr SymbolInfo& set_symtab(std::span<const Sym> symtab) {
    symtab_ = symtab;
    return *this;
  }

  constexpr SymbolInfo& set_compat_hash(std::span<const Word> table) {
    compat_hash_ = table;
    return *this;
  }

  constexpr SymbolInfo& set_gnu_hash(std::span<const Addr> table) {
    gnu_hash_ = table;
    return *this;
  }

  constexpr SymbolInfo& set_soname(size_type soname) {
    soname_ = soname;
    return *this;
  }

  constexpr SymbolInfo& set_flags(size_type flags) {
    flags_ = flags;
    return *this;
  }

  constexpr SymbolInfo& set_flags1(size_type flags1) {
    flags1_ = flags1;
    return *this;
  }

 private:
  template <typename T>
  using Span = AbiSpan<const T, std::dynamic_extent, Elf, AbiTraits>;

  // In directly-usable instantiations, ensure that the empty state is
  // guaranteed NUL terminated.  In remoting instantiations, the values will
  // always be reset from known-valid local instantiations so the zero-length
  // view will never be used.
  static constexpr AbiStringView<Elf, AbiTraits> kEmptyStrtab = []() {
    AbiStringView<Elf, AbiTraits> empty;
    if constexpr (std::is_constructible_v<AbiStringView<Elf, AbiTraits>, std::string_view>) {
      empty = std::string_view{"", 1};
    }
    return empty;
  }();

  size_type safe_symtab_size() const {
    if (symtab_.empty()) {
      return 0;
    }

    // The old format makes it very cheap to detect, so prefer that.
    if (CompatHash::Valid(compat_hash_)) {
      size_type hash_max = CompatHash(compat_hash_).symtab_size();
      return std::min(symtab_.size(), hash_max);
    }

    // The DT_GNU_HASH format has to be fully scanned to determine the size.
    if (GnuHash::Valid(gnu_hash_)) {
      size_type hash_max = GnuHash(gnu_hash_).symtab_size();
      return std::min(symtab_.size(), hash_max);
    }

    // With neither format available, there is no way to know the constraint
    // directly.  DT_STRTAB is usually right after, so that might be an upper
    // bound, but that's only a (likely) heuristic and not guaranteed.
    auto base = reinterpret_cast<const char*>(symtab_.data());
    auto limit = reinterpret_cast<const char*>(symtab_.data() + symtab_.size());
    if (base < strtab_.data() && limit > strtab_.data()) {
      return static_cast<size_type>(strtab_.data() - base) / sizeof(Sym);
    }

    // Worst case, there might still be some garbage entries at the end.  We
    // could scan through them all looking for invalid data (st_name out of
    // bounds, unsupported st_info bits, etc.), but that seems excessive.
    return symtab_.size();
  }

  AbiStringView<Elf, AbiTraits> strtab_ = kEmptyStrtab;
  Span<Sym> symtab_;
  Span<Word> compat_hash_;
  Span<Addr> gnu_hash_;
  Addr soname_ = 0;
  Addr flags_ = 0;   // DT_FLAGS
  Addr flags1_ = 0;  // DT_FLAGS_1

 public:
  // <lib/ld/remote-abi-transcriber.h> introspection API.  These aliases must
  // be public, but can't be defined lexically before the private: section that
  // declares the members; so this special public: section is at the end.

  using AbiLocal = SymbolInfo<Elf, LocalAbiTraits>;

  template <template <class...> class Template>
  using AbiBases = Template<>;

  template <template <auto...> class Template>
  using AbiMembers = Template<&SymbolInfo::strtab_, &SymbolInfo::symtab_, &SymbolInfo::compat_hash_,
                              &SymbolInfo::gnu_hash_, &SymbolInfo::soname_, &SymbolInfo::flags_,
                              &SymbolInfo::flags1_>;
};

// This constructs a SymbolInfo that just contains a single undefined symbol.
// It can be used with a resolver function (see link.h and resolve.h).
template <class Elf>
class SymbolInfoForSingleLookup : public SymbolInfo<Elf> {
 public:
  using typename SymbolInfo<Elf>::Sym;

  constexpr SymbolInfoForSingleLookup() = default;

  constexpr SymbolInfoForSingleLookup(const SymbolInfoForSingleLookup&) = default;

  explicit constexpr SymbolInfoForSingleLookup(const char* name,
                                               ElfSymType type = ElfSymType::kNoType,
                                               ElfSymBind bind = ElfSymBind::kGlobal)
      : symbol_{.info = Sym::MakeInfo(bind, type)} {
    this->set_strtab({name, std::string_view{name}.size() + 1});
    this->set_symtab(std::span{&symbol_, 1});
  }

  const Sym& symbol() const { return symbol_; }

 private:
  Sym symbol_;
};

}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_SYMBOL_H_
