# Compatibility Tests for Fuchsia

Compatibility Testing for Fuchsia (CTF) is a mechanism for running
different versions of pre-built Fuchsia software against the Fuchsia
platform surface area to detect compatibility problems.

CTF tests ensure that a client frozen at a previous release milestone
is compatible with the Application Programming Interface (API) and
Application Binary Interface (ABI) made available to developers via
the SDK. It simulates the situation where a behavioral change is
landed in the Fuchsia platform that breaks a pre-compiled client.

CTF fundamentally consists of the following parts:

- A mechanism to freeze artifacts on a release branch and then use
  those artifacts in a test on the main branch.
- A set of build rules to select artifacts to freeze, and a set
  of build rules to simplify thawing those frozen artifacts on
  main.
- A suite of tests using those build rules to test the compatibility of
  old client code with the latest platform surface area.
- The coverage provided by that suite of tests, in terms of which
  FIDL methods and syscalls are tested for compatibility across
  versions.

CTF was originally proposed as [RFC
0015](/docs/contribute/governance/rfcs/0015_cts.md), and the project
code is located at
[//sdk/ctf](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/ctf/).

## Background, motivation, and goals

The CTF exists to prevent breaking changes to the implemented Fuchsia
API and ABI from landing in the platform.

CTF originally was designed to provide confidence that an arbitrary
Fuchsia system correctly implemented the platform surface area for
a particular ABI revision. This would mean that components build
for that revision would work on the system, and that the system is
backwards compatible with that revision.

The current purpose of CTF is to provide a mechanism for freezing
and "thawing" test artifacts across release branches. For example,
a test on the `f18` release branch is downloaded as a prebuilt at
the top of the `main` branch. This test can then be executed against
the Fuchsia platform as it exists today; a failure indicates that
the Fuchsia platform broke compatibility with F18 in some way, which
means downstream platform integrators may run in to unexpected
failures if they try to run a pre-compiled binary on this new
platform image.

This mechanism can be applied to ensure the current Fuchsia platform
supports the old API and ABI behavior for each version that is
supposed to be supported. Additionally, the frozen tests may be
applied to arbitrary Fuchsia images to achieve the original goal
of CTF if desired.