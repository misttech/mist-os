# Component bedrock

Component bedrock introduces powerful low-level APIs for running programs and
routing capabilities on Fuchsia. Bedrock APIs are the primitives used to define
the Fuchsia component model.

## Goals

Bedrock aims to:
* Safely expose the dynamic nature of the component topology while preserving
  the secure static overlay we currently know as Component Framework v2.
  Specifically, we wish to expose APIs and objects for:
    * Capabilities
    * Capability routing
    * Directories
    * Programs
    * Components
    * Runners
    * Resolvers
* Separate the APIs above as a 'bedrock' layer from the policy layers above,
  which we'll call 'porcelain', and later expose additional 'porcelain' APIs
  implemented on top of the 'bedrock' APIs.
* Simplify the Component Framework while doing the above, eliminating edge cases
  and special cases as much as possible. We intend to migrate much of the
  Component Manager codebase to use the new bedrock APIs from above.

## Non-goals

* Create a Component Framework v3
* Require any large-scale migrations of client components like we did for the
  CFv2 migration.
* Overhaul the static layer of the component model ('porcelain') dramatically

## When is the project done?
1. When we can confidently expose the underlying CF APIs to a user like Driver
   Framework or Starnix, and they report positive results after an integration
   to do more dynamic component creation and capability routing.

1. When we've implemented and exposed bedrock objects for all object types
   listed in 'Goals', and have extended our developer documentation and tooling
   to use them.

1. The bedrock APIs have been reviewed and approved by the security team and
   Fuchsia API council.

1. We have happy customers of the bedrock APIs who are using them to implement
   features on top of component framework that were previously not possible or
   required workarounds.

## Current requests / use cases

### `use runner` from child
The starnix team would like to be able to `use` a runner from a child component,
which is currently not possible.

### ...