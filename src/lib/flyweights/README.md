# flyweights

Types implementing the [flyweight pattern] for reusing object allocations.
See the crate's [documentation] for details on the approach taken and tradeoffs
to consider.

<!-- TODO(fxr/787502) mention dupefinder to find places to use -->

## Comparison to other crates

As of January 2023, there did not appear to be any popular external crates for
interning which fit all of our requirements:

* global synchronization limited to creating the values, not required for
  reading them once created
* offers a short string optimization (SSO)
* types implement `Send` and `Sync`
* frees allocations for unused strings incrementally without spiking (no GC)
* limited usage of `unsafe`
* recently maintained, some dependents in the ecosystem

Alternatives evaluated along with reasons for rejection:

* [arc-string-interner](https://crates.io/crates/arc-string-interner)
  * no way to free unused strings
  * no SSO possible
* [intaglio](https://crates.io/crates/intaglio)
  * no way to free unused strings
  * no SSO possible
* [intern-arc](https://crates.io/crates/intern-arc)
  * no SSO possible
  * lots of unsafe code to support features we don't need
* [internment](https://crates.io/crates/internment) (& a fork [arc-interner](https://crates.io/crates/arc-interner))
  * no SSO possible
  * large transitive dependencies with lots of unsafe
* [internship](https://crates.io/crates/internship)
  * types can't be used across multiple threads
* [lasso](https://crates.io/crates/lasso)
  * no way to free unused strings
* [refcount-interner](https://crates.io/crates/refcount-interner)
  * requires manually calling `shrink_to_fit()` on the interner instead of
    freeing unused strings as their references are dropped, too expensive to
    call on every `Drop`
* [string-intern](https://crates.io/crates/string-intern)
  * no updates for 5 years, is marked as beta in its readme
* [string-interner](https://crates.io/crates/string-interner)
  * no way to free unused strings
* [symbol_interner](https://crates.io/crates/symbol_interner)
  * no way to free unused strings
* [symtern](https://crates.io/crates/symtern)
  * no way to free unused strings

[flyweight pattern]: https://en.wikipedia.org/wiki/Flyweight_pattern
[documentation]: ./src/lib.rs
