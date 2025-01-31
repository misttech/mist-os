# SDK categories

Each [SDK Atom] has a category that defines which kinds of SDK consumers can see
the Atom. As SDK Atoms mature, we can increase their visibility, which implies
increasing their stability guarantees.

[SDK Atom]: /docs/glossary#sdk-atom

## Motivation

Fuchsia is built by combining many different components that interact using
protocols with schemas defined in FIDL. Components that are part of the Fuchsia
project interact with each other using the same mechanism that components
written by third parties interact with the Fuchsia platform. For this reason,
we benefit from having a uniform mechanism that can be used both to develop
Fuchsia and to develop for Fuchsia.

The simplest approach would be to put all the FIDL definitions into the Fuchsia
SDK, and then have all the developers use those same FIDL definitions in
developing their components. However, this approach breaks down because of a
common tension in designing APIs: API designers need the ability to iterate on
their designs and API consumers need stability in order to build on top of the
APIs.

This document describes SDK categories, which is Fuchsia's primary mechanism
for balancing these concerns.

## Design

FIDL libraries are one example of an SDK Atom, but there are other kinds of
SDK Atoms, including C++ client libraries, documentation, and tools. SDK
categories apply to every kind of SDK Atom, but this document uses FIDL
libraries as a running example.

SDK categories balance the needs for iteration and stability in APIs by
recognizing that different API consumers have different stability needs. API
consumers that are "closer" to API designers typically have less need for
stability and often are the first customers that provide implementation
feedback for API designers.

Each SDK Atom is annotated with an SDK category, which defines which SDK
consumers can depend upon the SDK Atom. For example, if the `fuchsia.foo` FIDL
library has an SDK category of `internal`, that means only SDK consumers within
the Fuchsia project can depend upon `fuchsia.foo`. If someone wants to change
`fuchsia.foo`, they run the risk of breaking consumers inside the Fuchsia
project but they do not run the risk of breaking consumers in other projects.

As another example, consider a `fuchsia.bar` FIDL library with an SDK category
of `partner`, which means `fuchsia.bar` can be used both within the Fuchsia
project and by SDK consumers who have partnered[^1] with the Fuchsia project.
When someone changes `fuchsia.bar`, they run a larger risk of breaking
consumers because they might break the partners that depend upon `fuchsia.bar`.

Finally, consider a `fuchsia.qux` FIDL library with an SDK category of
`public`, which means `fuchsia.qux` can be used by the general public. Changing
`fuchsia.qux` is very risky because the set of software developed by the
general public is potentially unbounded and unknowable.

Along with defining concentrically increasing sets of API consumers, SDK
categories also define increasing stability windows. For example, `fuchsia.foo`
can change dramatically from one day to the next because the `internal`
category limits the exposure to the Fuchsia project itself. Someone changing
`fuchsia.foo` can change all the clients and servers at the same time, which
means the stability window needed for the API is either very small or zero. By
way of contrast, the agreement that Fuchsia has with partner projects includes
an expectation for compatibility windows.

Currently, Fuchsia do not have any SDK Atoms with an SDK category of `public`,
which means Fuchsia has not made any commitments to supporting the general
public using its APIs. However, at some point, the Fuchsia project will begin
supporting the general public using its APIs. At that time, the Fuchsia project
will need to define the compatibility window for those APIs, which will likely
be longer than the compatibility window for `partner` APIs.

An additional type of SDK category is required for the APIs used in the prebuilt
`partner` or `public` SDK atoms when it's undesirable to expose these APIs to
SDK users. These `partner_internal` and `public_internal` categories will enforce
the same API compatibility windows as the `partner` and `public` categories
without requiring adding those APIs to the SDK API surface area. Only the
`partner_internal` category will be introduced for now as there's no `public`
SDK atoms.

A typical SDK Atom begins its lifecycle in the `internal` SDK category. At some
point, the API Council might graduate the SDK Atom might to the `partner` SDK
category, often when a partner needs access to an API contained in the Atom.
Sometime in the future, when Fuchsia has a non-empty `public` SDK category, SDK
Atoms will be able to graduate from the `partner` category to the `public`
category as well. Some SDK Atoms might remain in the `internal` SDK category
indefinitely. Others might graduate to `partner` but never graduate to
`public`.

Please note that this mechanism is complementary to `@available` mechanism for
[platform versioning][fidl-versioning]. The `@available` mechanism *records*
when and how FIDL APIs change. The SDK category mechanism determines the
*policy* for how quickly API designers can make changes.

[^1]: Currently, the set of partners is not public. As the project scales, we
      will likely need to revisit our approach to partnerships.

[fidl-versioning]: /docs/reference/fidl/language/versioning.md

## Categories

SDK categories have been implemented in the [`sdk_atom`](/docs/glossary#sdk-atom) GN Rule.
Each SDK Atom has an `category` parameter with one of the following values:

- `internal`: deprecated (no new uses allowed)
- `cts`: supported for use in the Compatibility Tests for Fuchsia (not yet supported)
- `partner_internal`: supported for use in non-source SDK atoms in the
  `partner` category but not exposed to the SDK users
- `partner`: supported for use by select partners
- `public`: supported for use by the general public (not yet supported)

These categories form an ordered list with a monotonically increasing audience.
For example, an SDK Atom in the `public` category is necessarily available to
select partners because `public` comes after `partner` in this list.

## Commitments

Adding an API to the `partner` or `partner_internal` category amounts to a
commitment to our partners that we will not break their code or impose undue
[churn][churn-policy] on them. Each team that owns an API in one of these
categories has a responsibility to uphold these commitments.

[churn-policy]: /docs/contribute/governance/policy/churn.md

### `internal`

APIs in the `internal` category have minimal commitments beyond those that
apply to [all code in the Fuchsia project][contributor-guide].

If your API is only ever called by *in-tree* code, with both sides of the
communication always having been built from the _same revision_ of the Fuchsia
source (as is the case for two platform components talking to each other), then
it should be `internal`.

Note that even if all of your API's clients are in-tree, that's not sufficient
to say it belongs in `internal`. For instance, the source for `ffx` is in the
Fuchsia tree, but it doesn't meet the second requirement: `ffx` subtools built
at one Fuchsia revision will frequently talk to a device built at another. As
such, `ffx` subtools, and any other artifacts shipped in the SDK, must not
depend on `internal` APIs.

[contributor-guide]: /CONTRIBUTING.md

### `partner_internal`

Partners don't write their own code using `partner_internal` APIs, but they
still depend on these APIs _indirectly_ via tools, libraries, or packages
written by the Fuchsia team. Since the Fuchsia team owns the code that uses
these APIs, we can change these APIs without churning our partners. However,
the tools, libraries, and packages that use `partner_internal` APIs will, in
general, be built from a different revision than the platform components that
they talk to. Thus, we must follow our ABI compatibility policies whenever we
change `partner_internal` APIs.

Namely, the owners of an API in the `partner_internal` category agree to:

* Use [FIDL Versioning][fidl-versioning] annotations on their APIs.
* Only ever modify their API at the `in-development` API level, or at `HEAD`.
  Once an API level is declared stable, it should not be changed (see [version_history.json]).
* Keep the platform components that implement those APIs compatible with all
  Fuchsia-supported API levels (see [version_history.json]).

See the [API evolution guidelines][evolution-guidelines] for more details on
API compatibility.

[version_history.json]:  /sdk/version_history.json
[evolution-guidelines]: /docs/development/api/evolution.md

### `partner`

Partners use `partner` APIs directly. These APIs are the foundation on which
our partners build their applications, and it is our responsibility to keep
that foundation reliable and stable.

Owners of an API in the `partner` category agree to:

* Make all the versioning commitments from the `partner_internal` section
  above.
* Own our partners' developer experience when it comes to this API, including:
  * Providing good documentation.
  * Following consistent style.
  * Anything else you'd like to see in an SDK you were using.

  See the [API Development Guide][api-dev] for relevant rules and suggestions.
* Acknowledge that backwards-incompatible changes to new API levels impose a
  cost on our partners, even when we follow our [API evolution
  guidelines][evolution-guidelines]. If and when a partner chooses to update
  their target API level, they will need to make modifications within their
  codebase to adapt to your change. As such, these changes should not be made
  lightly.

  If you _do_ decide a backwards-incompatible change is worth making, you agree
  to pay most of the downstream costs of that change, in accordance with the
  [churn policy][churn-policy].

  Changes are _much_ easier to make to `internal` APIs than `partner` APIs, so
  any planned API refactoring should be done just _before_ adding an API to the
  `partner` category, rather than after.

[api-dev]: /docs/development/api/README.md

## Change history

- First documented in [RFC-0165: SDK categories][rfc-0165].

[rfc-0165]: /docs/contribute/governance/rfcs/0165_sdk_categories.md
