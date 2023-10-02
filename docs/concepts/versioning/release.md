# Release process

This document describes the process by which the Fuchsia platform is released.

Throughout this document, the word "release" refers to the act of generating
and publishing various artifacts for consumption by developers further down the
software supply chain (for example, component authors or product owners). It
_does not_ refer to the act of delivering updates to Fuchsia-based products to
end users.

## Releases

A Fuchsia _release_ is a set of generated artifacts that are the output of the
Fuchsia project at a given point in time, published under a specific _release
version_ name.

It consists of:

- The [Integrator Development Kit (IDK)][idk], a small set of libraries,
  packages, and tools required to build and run programs that target Fuchsia.
  - A number of SDKs based on the IDK (for example, the [Bazel
    SDK][bazel-sdk]).
- The Operating System (OS) binaries, including the kernel, bootloaders,
  packages, tools, and other ingredients necessary to configure and assemble
  Fuchsia [product bundles][product-bundle].
  - A number of pre-assembled product bundles (for example, the [workbench]
    product)[^historical].
- Various other artifacts, such as documentation and [Compatibility Tests for
  Fuchsia (CTF)][ctf] builds.

There's no single archive that contains all of these artifacts; different
artifacts are produced by different builders and uploaded to different
repositories. Some artifacts are freely available online, and some are
confidential. (Indeed, both the IDK and the OS binaries have both public and
confidential variants.)

 What unites them is that:

- They are all built exclusively from a single revision of the Fuchsia [source
  tree][source-tree] (specifically, the Google-internal version of
  [`integration.git`][integration.git][^internal]), and
- They are all published to their respective repositories under the same
  _release version_.

[idk]: /docs/development/idk/README.md
[bazel-sdk]: /docs/contribute/governance/rfcs/0139_bazel_sdk.md
[product-bundle]: /docs/glossary#product-bundle
[workbench]: /docs/contribute/governance/rfcs/0220_the_future_of_in_tree_products.md
[ctf]: /docs/development/testing/ctf/overview.md
[source-tree]: /docs/get-started/learn/build/source-tree.md
[integration.git]: https://fuchsia.googlesource.com/integration/

[^historical]: Some of these products are defined in the Fuchsia source tree
    for purely historical reasons, and will be moved out-of-tree as soon as
    possible to promote product/platform separation.

[^internal]: These tags are only present in the internal `integration.git` due
    to technical limitations - one day we may start publishing these tags in
    the public mirror as well.

## Release versions

A _release version_ is a string of characters that names a release.

Each release version has a corresponding tag in the Google-internal version of
`integration.git`, which immutably points to the git commit from which the
release's binary artifacts were built.

Versions are named `M.YYYYMMDD.R.C` (e.g. 2.20210213.2.5) where

*   `M` indicates the "[milestone](#milestone)" of the release.
*   `YYYYMMDD` is the date when the release's history diverged from the `main`
    branch.
*   `R` indicates the "release" version number. It starts at 0 and increments
    when multiple releases are created on the same date.
*   `C` indicates the "candidate" version number. It starts at 1 and increments
    when changes are made to a previous release on a _milestone release
    branch_.

## Canary releases

A few times a day[^time-periods], a _canary_ release is created, based on the
last-known-good revision of the Fuchsia source tree. Concretely, a git tag is
applied to that revision in the Google-internal version of `integration.git`,
and various builders are triggered to build and publish the artifacts described
above. Canary releases do not get their own release branches, only tags.

Each canary release is only supported for a brief window - until the next
canary release. Bugfixes are not cherry-picked onto canary releases. (Put
another way: the "candidate" version number of a canary release should always
be "1".) If a bug is found in a canary release, a fix for that bug will be
applied to the `main` branch of the Fuchsia source tree. Clients impacted by
that bug should roll back to an earlier release and/or await a subsequent
canary release that includes the fix.

As such, canary releases are appropriate for development and testing, but may
not be appropriate for production. For production use cases, _milestone
releases_ are preferable.

[^time-periods]: This document does not specify any particular release cadence.
    Time periods are named to provide an "order of magnitude" estimate for the
    frequency of various processes. Cadences will change as project and
    customer needs evolve.

## Milestone releases {#milestone}

Every few weeks, a _milestone release branch_ is created in both the
Google-internal version _and_ the public mirror of `integration.git`, starting
from the git commit for an existing "known good" canary release. Releases
originating from a milestone release branch are known as _milestone releases_
or _stable releases_.

Milestones are numbered sequentially, and often prefixed with an "F" when
discussed (as in, "the F12 release branch").

Once the F`N` milestone release branch has been cut, mainline development in
the Fuchsia source tree will continue on the `main` branch, working towards the
next F`N+1` release, and canary releases will have a version beginning with
`N+1`, as shown in the following diagram:

![Diagram with colored arrows representing Fuchsia milestones. F11 begins on
the main branch, but then branches off to become the f11 branch. After that,
the main branch is labeled F12, which again branches off,
etc.](resources/milestones.png)

Milestone releases share the `M.YYYYMMDD.R` part of their version with the
canary release on which they are based, and therefore only the "candidate"
version number `C` changes between releases built from a given milestone
release branch. Note that this means it's not always immediately obvious from a
version whether that release is a canary or milestone release (though a value
of `C` greater than 1 indicates that a release comes from a milestone release
branch).

Unlike canary releases, milestone releases are _supported_ for some number of
weeks after their creation. That is, improvements may be made to milestone
release branches after the release branch is cut. After each change to a
milestone release branch, a new milestone release will be published with a
larger "candidate" version number.

As a matter of general policy:

* Features are developed on `main`, not in milestone release branches.
* Changes made to milestone release branches land on `main` first, and then are
  cherry-picked onto the branch.
* Only changes that fix bugs (be they related to quality, security, privacy,
  compliance, etc) will be made to milestone release branches. We do not make
  changes that introduce new functionality to milestone release branches.

These policies are designed to minimize the odds that changes to milestone
release branches introduce new bugs, and thus the reliability and stability of
releases associated with a given milestone should improve over time. As such,
downstream customers are encouraged to eagerly adopt these new milestone
releases.

Under certain exceptional circumstances, we may need to stray from these
policies (e.g., a security fix may need to be developed on a confidential
branch to avoid publicizing a vulnerability before a fix is ready). Whether to
include a change on a milestone release branch is ultimately the release
manager's decision.

## Backwards Compatibility

The Fuchsia project promises that all artifacts within a single release (be it
canary or milestone) are compatible with each other. For example, if a
developer builds a component using SDK version `2.20210213.2.5`, we promise
that component will run on a Fuchsia device with OS version `2.20210213.2.5`.
If the component sees incorrect behavior from the Fuchsia platform under these
circumstances, that will be acknowledged as a bug.

Strictly speaking, as of this writing, Fuchsia does not offer a formal
guarantee of API or ABI compatibility between _any_ two releases - no existing
RFC discusses the matter. However, in practice, Fuchsia components built with
an SDK from one release frequently run on Fuchsia systems based on a different
release, and we still address issues discovered in those configurations.

A conditional promise of API and ABI compatibility across versions will be
described and ratified in a future RFC.

## Document history

* First introduced in [RFC-0227].

[RFC-0227]: /docs/contribute/governance/rfcs/0227_fuchsia_release_process.md