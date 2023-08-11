# Component bedrock design

Component bedrock is a refactoring of component manager that separates it into
two conceptual systems:

 - Bedrock: lower-level primitives for defining capabilities, component
   interfaces (sandboxes) and running programs.
 - Porcelain: common features and operations for managing software running on
   Fuchsia, implemented in terms of bedrock.

Currently, most of component manager's implementation is porcelain. Over time,
parts of component manager will be rehosted on top of the bedrock layer.

These layers are introduced to create a set of stable and flexible APIs for
running software and routing capabilities. In general, bedrock APIs are less
opinionated than porcelain APIs and provide more fine-grained control.

## Design

Bedrock consists of the following major components:

 - [`sandbox` library](lib-sandbox): Base traits and types for capabilities and
   component/program interfaces (sandboxes)
 - [`fuchsia.component.sandbox`](fidl-sandbox): The FIDL equivalents of sandbox
   types, used to transfer bedrock types between processes
 - [`runner` library](lib-runner): Defines the component runner interface
 - [`serve_processargs` library](lib-serve_processargs): Bridges sandboxes
   and component namespaces

### sandbox

The sandbox library defines the core types for defining capabilities and
transferring them between components/programs.

All capabilities implement the [`Capability`](capability-trait) Rust trait.
The only hard requirement of a capability is that it can be converted to a
Zircon handle. This means that *all* bedrock capabilities are Zircon
capabilities, and that they can be transferred between processes as any other
handle. This also implies the same security model - you must hold a handle
to the capability (or have ownership of the Rust object) to use it.

The [`Handle`](handle-cap) capability is the simplest capability implementation.
It just holds an arbitrary Zircon handle.

From there, the sandbox library defines a few more useful types to support
transferring, or "routing", capabilities between programs.

TODO(b/295386899): Write about Dict

TODO(b/295386899): Write about Sender/receiver

## Principles

Bedrock is implemented as a set of Rust libraries and FIDL APIs, as needed,
within the component manager codebase.

Its design is guided by the following principles:

* *Self-contained*: Bedrock is useful in a world where the current component
  manager implementation and APIs do not exist.

  - Bedrock does not depend on porcelain. This prevents a circular dependency
    between the two layers which prevents one abstracting over the other.

* *No favorites*: Bedrock should not favor any client at the expense of others.

  - Bedrock should not take on features that are only useful to component
    manager and its implementation.
  - Rust and FIDL APIs are equally powerful. Bedrock and porcelain may be
    colocated in the same component manager process and communicate using Rust
    types for performance reasons. Any other process can implement the same
    porcelain operations using FIDL bedrock APIs.
  - Component framework users have access to all bedrock APIs, pending
    stabilization. Bedrock APIs are not "internal" to component manager.

* *Open/closed*: Bedrock is primarily a set of interfaces, and concrete
  implementations are extended via generics or composition.

  - Clients should not need to modify bedrock types to extend them. For example,
  bedrock collections are generic over the capability interface - a client can
  define a new capability in porcelain and put it into a bedrock container
  (dict), without modifying the container type.

## Bedrock vs porcelain

Component manager is not yet layered into the bedrock and porcelain model.
We will follow an iterative process to discover and implement the exact
contents of each layer over time:

1. Identify an API, feature, or internal implementation (an "artifact")
   that does not meet design heuristics of symmetry, totality, and unity. 

   For example, a violation of unity is that a component created from a static
   declaration (a child in a manifest) cannot be destroyed, while a component
   created dynamically can be.
2. Design a new artifact that meets the heuristics, initially internal to
   component manager, and refactor the previous "porcelain" artifact
   in terms of the new "bedrock" artifact.

   For example, both static, declarative components and dynamic components
   may be implemented with a single component type that is imperatively
   created and destroyed. The system that implements the static declaration
   as imperative calls is the porcelain, and the underlying component type
   is bedrock.
3. Repeat to arrive a set of bedrock artifacts that are as low-level as
   possible, while preserving invariants.

   A single iteration of this process may not be enough to create a stable
   bedrock artifact - it may need to be broken up or refactored further.

Bedrock generally has the following properties:

* Imperative, not declarative

Porcelain generally has the following properties:

* Both imperative (e.g. `fuchsia.realm.*` APIs) and declarative (manifests)
* Makes a distinction between "static" and "dynamic" component instances
* Defines "Built in" and "framework" capabilities that are hosted by a
  common runtime
* Defines features that transform capabilities, especially during routing.
  For example:
    - The `subdir` operation is porcelain that transforms one directory
      capability to another - bedrock does not "know" that one directory is a
      subdirectory of another, just that they are two separate capabilities.
    - Aggregate service routing is a porcelain action that combines several
      service capabilities into a one. Bedrock participates in routing these
      capabilities (transferring them between sandboxes), while porcelain
      performs the transformation.

## Open questions

We have not yet decided on the following design choices:

* Structure of the component topology. For example, whether components have a
  parent-child relationship, and types that assume this relationship (monikers)
* Whether existing `fuchsia.component.*` FIDL protocols are part of bedrock
  or porcelain, and if so, which ones.
* Whether any new bedrock FIDL APIs are exposed alongside porcelain APIs in
  `fuchsia.component.*` or under a different namespace, or both.

[capability-trait]: //src/sys/component_manager/lib/sandbox/src/capability.rs
[handle-cap]: //src/sys/component_manager/lib/sandbox/src/handle.rs
[lib-sandbox]: //src/sys/component_manager/lib/sandbox
[lib-runner]: //src/sys/component_manager/lib/runner
[lib-serve_processargs]: //src/sys/component_manager/lib/serve_processargs
[fidl-sandbox]: //sdk/fidl/fuchsia.component.sandbox/sandbox.fidl
