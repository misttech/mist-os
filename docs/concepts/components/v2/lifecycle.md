# Component lifecycle

This document describes how Component manager interacts with individual component
instances to manage their lifecycle.

## Lifecycle states {#states}

Component instances progress through the following major lifecycle states:

![Component lifecycle states](images/component-lifecycle.png){: width="662"}

Component instances may retain isolated persistent state on a storage medium
while they are not running, which can be used to help them maintain continuity
across restarts.

### Created {#creating}

A component instance may be created in the following ways:

-   Configured as the root component of Component manager.
-   Statically discovered as the [child][doc-manifests-children] of another
    component.
-   Dynamically created at runtime in a [collection][doc-collections].

Every component instance has a component URL that describes how to resolve the
component, and a moniker that uniquely identifies the instance by its path from
the root. For more details, see [component identifiers][doc-identifiers].

Once created, a component instance can then be [resolved](#resolving) or
[destroyed](#destroying).

### Resolved {#resolving}

Resolving a component instance fetches the component declaration for the
specified component URL. Component manager resolves component URLs by finding a
[component resolver][doc-resolvers] that supports a matching URL scheme in the
environment. Developers can resolve components manually using the
[`ffx component resolve`][ref-ffx-resolve] command.

Components must successfully resolve before they can be [started](#starting).

### Started {#starting}

Starting a component instance loads and runs the component's program and
provides it access to the capabilities that it requires.

The most common reason for starting a component instance is when another
component [binds](#binding) to one of its exposed capabilities. Developers can
also start components manually using the [`ffx component start`][ref-ffx-start]
command.

Once started, a component instance continues to run until it is
[stopped](#stopping).

### Stopped {#stopping}

Stopping a component instance terminates the component's program but preserves
its [persistent state][doc-storage]. Components enter this state when their
program exits, as defined by the component's [runner][doc-runners].

The Component Framework may stop a component instance for the following reasons:

-   The component is about to be destroyed.
-   The system is shutting down.

A component can implement a lifecycle handler ([example][handler-example]) to
receive a notification of events such as impending termination.
Note that components may not receive these events in circumstances such as
resource exhaustion, crashes, or power failure.

Once stopped, a component instance may be [restarted](#starting) or
[shutdown](#shutdown).

### Shutdown {#shutdown}

Component manager sets the final execution state of a component instance to
shutdown to indicate that it cannot be restarted and to signal that the instance
can be safely [destroyed](#destroying).

### Destroyed {#destroying}

A component instance may be destroyed in the following ways:

-   Dynamically removed from a [collection][doc-collections] at runtime. This is
    also true if the component is a descendant of another component being removed.

Once destroyed, Component manager completely removes the instance from the
component topology, including all persistent state. New instances of the same
component will each have their own identity and state distinct from all prior
instances.

## Lifecycle actions {#actions}

This section describes common actions used by the Component Framework to
transition the lifecycle state of component instances.

### Bind {#binding}

A component instance `A` _binds_ to another component instance `B` when `A`
connects to some capability that is provided by `B`. This causes component `B`
to [start](#starting) if it is not already running.

Concretely, there are two ways that `A` can bind to `B`:

-   `A` connects to a capability in its namespace which is
    [exposed][doc-manifests-expose] or [offered][doc-manifests-offer] by `B`.
    This is the most common way.
-   `A` binds to the [`fuchsia.component.Binder`][binder.fidl]
    [framework protocol][doc-framework-protocol] which is exposed or offered
    by `B`. Unlike a traditional capability, this protocol
    is implemented by the component framework.

Note: For more details on running components during development, see
[Run components][doc-run].

### Shutdown ordering

When the component framework decides to [shut down](#shutdown) a component, it will
shut down the component and its children in an order determined by their dependencies.
This behavior is recursive which means that shutting down a component will shut
down the full topology under that component.

Components within a realm are shut down in a specific order, which is based on
the strong dependencies between those components. The dependencies between components form
a Directed Acyclical Graph (DAG), and components are shut down in target-to-source order.

All capabilities are considered in the dependency graph. Capabilities are
offered as a strong dependency by default, but a client can offer a dependency
by specifying [`dependency: weak`](#weak).

As an example, imagine a realm with components `A`, `B`, `C`, `D`.
There are the following offers:

- `A` strongly offers a capability to `B` and `C`.
- `B` strongly offers a capability to `C`.
- `C` weakly offers a capability to `B`.

Here is a drawing of the realm in question:

![Component shutdown example](images/component_shutdown_example.png){: width="662"}

The shutdown ordering would be the following:

- `D` and `C` can be shut down in any order as they have no dependents.
- `B` can then be shut down after `C`.
- `A` can then be shut down after `B`.

#### Weak dependencies {#weak}

Remember that the component dependencies forms a DAG, meaning there are no
cycles. This means that in the example above, `C` cannot strongly offer a
capability to `A`.

However, `C` is allowed to offer a capability to `A` with `dependency: "weak"`.
A weak dependency is not considered to be a part of the dependency DAG and it will
not affect shutdown ordering.

[binder.fidl]: https://fuchsia.dev/reference/fidl/fuchsia.component#Binder
[doc-framework-protocol]: capabilities/protocol.md#framework
[doc-collections]: realms.md#collections
[doc-identifiers]: identifiers.md
[doc-manifests-children]: https://fuchsia.dev/reference/cml#children
[doc-manifests-expose]: https://fuchsia.dev/reference/cml#expose
[doc-manifests-offer]: https://fuchsia.dev/reference/cml#offer
[doc-manifests]: component_manifests.md
[doc-resolvers]: capabilities/resolver.md
[doc-runners]: capabilities/runner.md
[doc-storage]: capabilities/storage.md
[doc-run]: /docs/development/components/run.md
[handler-example]: /examples/components/lifecycle
[ref-ffx-resolve]: https://fuchsia.dev/reference/tools/sdk/ffx#resolve
[ref-ffx-start]: https://fuchsia.dev/reference/tools/sdk/ffx#start
