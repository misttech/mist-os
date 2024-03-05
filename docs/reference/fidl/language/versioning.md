# FIDL versioning

This document describes FIDL's API versioning features. For guidance on how to
evolve Fuchsia APIs, see [Fuchsia API evolution guidelines][api-evolution].

## Motivation

FIDL versioning lets you change a FIDL library over time while keeping the
ability to generate bindings for older versions of the library. There are a
number of ways you could do this manually:

- Store `.fidl` files in a `v1/` directory. To make a change, copy `v1/` to
  `v2/` and change files there. To generate bindings for the older version,
  use the `v1/` library instead of `v2/`.

- Store `.fidl` files in a git repository and make changes in commits. To
  generate bindings for an older version, check out an older revision of the
  repository.

The first solution is tedious and creates a lot of duplication. The second
solution doesn't work well in a large repository that contains many things
besides that specific FIDL library, such as the main Fuchsia repository.

FIDL versioning accomplishes the same thing, but without these shortcomings.
When making a change, you use the `@available` attribute to describe when (i.e.
at which version) the change occurs. To generate bindings for an older version,
you pass the `--available` flag to fidlc and specify an older version.

There are two important things to keep in mind with FIDL versioning:

- It affects **API only**. Versions exist only at compile time, and have no
  impact on runtime behavior.

- It can represent **any syntactically valid change**. Just because you can
  represent a change with versioning doesn't mean that change is safe to make.

## Concepts

The unit of versioning is a group of libraries, called a **platform**. By
convention, libraries are named starting with the platform name. For example,
the libraries `fuchsia.mem` and `fuchsia.web` belong to the `fuchsia` platform.

Each platform has a linear **version** history. A version is an integer from 1
to 2^63-1 (inclusive), or one of the special versions `HEAD` and `LEGACY`. The
`HEAD` version is used for the latest unstable changes. The `LEGACY` version is
like `HEAD`, but it also includes [legacy elements](#legacy).

If a FIDL library doesn't have any `@available` attributes, it belongs to the
`unversioned` platform. This platform only has one version, `HEAD`.

## Command line

The FIDL compiler accepts the `--available` flag to specify platform versions.
For example, assuming `example.fidl` defines a library in the `fuchsia` platform
with no dependencies, you can compile it at version 8 as follows:

```posix-terminal
fidlc --available fuchsia:8 --out example.json --files example.fidl
```

No matter what version you select, fidlc always validates all possible versions.
For example, the above command can report an error even if the error only occurs
in version 5.

If a library `A` has a dependency on a library `B` from a different platform,
you can specify versions for both platforms using the `--available` flag twice.
However, `A` must be compatible across its entire version history with the fixed
version chosen for `B`.

## Syntax

The `@available` attribute is allowed on any [FIDL element][element]. It takes
the following arguments:

| Argument     | Type      | Note                      |
| ------------ | --------- | ------------------------- |
| `platform`   | `string`  | Only allowed on `library` |
| `added`      | `uint64`  | Integer or `HEAD`         |
| `deprecated` | `uint64`  | Integer or `HEAD`         |
| `removed`    | `uint64`  | Integer or `HEAD`         |
| `replaced`   | `uint64`  | Integer or `HEAD`         |
| `note`       | `string`  | Goes with `deprecated`    |
| `legacy`     | `boolean` | Goes with `removed`       |

There are some restrictions on the arguments:

- All arguments are optional, but at least one must be provided.
- Arguments must be literals, not references to `const` declarations.
- The `removed` and `replaced` arguments are mutually exclusive.
- Arguments must respect `added <= deprecated < removed`, or
  `added <= deprecated < replaced`.
- The `added`, `deprecated`, `removed`, `replaced`, and `legacy` arguments
  [inherit](#inheritance) from the parent element if unspecified.

For example:

```fidl
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="examples/fidl/fuchsia.examples.docs/versioning.test.fidl" region_tag="arguments" %}
```

If `@available` is used anywhere in a library, it must also appear on the
library declaration. For single-file libraries, this is straightforward. For
libraries with two or more `.fidl` files, only one file can have its library
declaration annotated. (The library is logically considered a single [element]
with attributes merged from each file, so annotating more than one file results
in a duplicate attribute error.) The FIDL style guide [recommends][overview]
creating a file named `overview.fidl` for this purpose.

On the library declaration, the `@available` attribute requires the `added`
argument and allows the `platform` argument. If the `platform` is omitted, it
defaults to the first component of the library name. For example:

```fidl
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="examples/fidl/fuchsia.examples.docs/versioning.test.fidl" region_tag="library" %}
```

## Inheritance {#inheritance}

The arguments to `@available` flow from the library declaration to top-level
declarations, and from each top-level declaration to its members. For example,
if a table is added at version 5, there is no need to repeat this annotation on
its members because they could not exist prior to the table itself. Here is a
more complicated example of inheritance:

```fidl
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="examples/fidl/fuchsia.examples.docs/versioning.test.fidl" region_tag="inheritance" %}
```

## Deprecation

Deprecation is used to indicate that an element will be removed in the future.
When you deprecate an element, you should add a `# Deprecation` section to the
doc comment with a detailed explanation, and a `note` argument to the
`@available` attribute with a brief instruction. For example:

```fidl
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="examples/fidl/fuchsia.examples.docs/versioning.test.fidl" region_tag="deprecation" %}
```

As of May 2022 deprecation has no impact in bindings. However, the FIDL team
[plans][deprecation-bug] to make it emit deprecation annotations in target
languages. For instance, the example above could produce `#[deprecated = "use
Replacement"]` in the Rust bindings.

## Replacing

The `replaced` argument lets you change an element at a particular version by
writing a completely new definition. This is the only way to change certain
aspects of FIDL elements, including:

- The value of a constant
- The type of a struct, table, or union member
- The ordinal of table or union member
- The kind of a declaration, e.g. changing a struct to an alias
- The presence of `error` syntax on a method
- Other attributes on the element such as `@selector`
- Modifiers such as `strict`, `flexible`, and `resource`

To replace an element at version `N`, annotate the old definition with
`@available(replaced=N)` and the new definition with `@available(added=N)`.
For example, here is how you change an enum from `strict` to `flexible`:

```fidl
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="examples/fidl/fuchsia.examples.docs/versioning.test.fidl" region_tag="replaced" %}
```

You can also use replacement to clean up a FIDL library that has become
cluttered by a long history of API changes. Simply replace all elements with a
fresh definition, and then move the old definitions to a separate file with a
name like `history.fidl`.

The FIDL compiler verifies that every `@available(replaced=N)` element has a
matching `@available(added=N)` replacement. It also verifies that every
`@available(removed=N)` element **does not** have such a replacement. This
validation only applies to elements directly annotated, not to elements that
[inherit](#inheritance) the `removed` or `replaced` argument.

## Legacy {#legacy}

When removing an element, you can use `legacy=true` to keep it in the `LEGACY`
version. This lets you preserve ABI for clients targeting API levels before its
removal, since the Fuchsia system image is built against `LEGACY` FIDL bindings.
For example:

```fidl
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="examples/fidl/fuchsia.examples.docs/versioning.test.fidl" region_tag="legacy" %}
```

Here, `LegacyMethod` does not appear in bindings at version 6 or higher nor at
`HEAD`, but it gets added back in the `LEGACY` version.

## References

There are a variety of ways one FIDL element can reference another. For example:

```fidl
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="examples/fidl/fuchsia.examples.docs/versioning.test.fidl" region_tag="references" %}
```

When referencing elements, you must respect the `@available` attributes. For
example, the following code is invalid because `A` exists from version 1 onward,
but it tries to reference `B` which only exists at version 2:

```fidl
// Does not compile!

@available(added=1)
const A bool = B;

@available(added=2, removed=3)
const B bool = true;
```

Similarly, it is invalid for a non-deprecated element to reference a deprecated
element. For example, the following code is invalid at version 1 because `A`
references `B`, but `B` is deprecated while `A` is not.

```fidl
// Does not compile!

@available(deprecated=2)
const A bool = B;

@available(deprecated=1)
const B bool = true;
```

[element]: /docs/contribute/governance/rfcs/0083_fidl_versioning.md#terminology
[overview]: /docs/development/languages/fidl/guides/style.md#library-overview
[deprecation-bug]: https://bugs.fuchsia.dev/p/fuchsia/issues/detail?id=7692
[api-evolution]: /docs/development/api/evolution.md
