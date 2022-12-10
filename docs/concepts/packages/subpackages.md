# Fuchsia Subpackages

[Packages] can "contain" other packages, producing a hierarchy of nested
packages.

_Updated documentation on Subpackages is coming soon!_

## Examples

### Declaring build dependencies to subpackages

Fuchsia-enabled build frameworks should include a pattern for declaring a
Fuchsia package and its contents. If also enabled to support subpackages, a
package declaration will list the subpackages it depends on, by direct
containment.

For example, in fuchsia.git, the GN templates for declaring Fuchsia packages
support two optional lists, `subpackages` and (less commonly used)
`renameable_subpackages`. One or both can be included. The `renameable_`
version allows the package to assign a package-specific name to the subpackage,
used when referring to the subpackage by package URL or component URL:

```gn
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="examples/components/subpackages/BUILD.gn" region_tag="declare_subpackages" adjust_indentation="auto" %}
```

The `subpackages` list contains a list of GN `fuchsia_package` build targets. By
default, the subpackage name (the name the containing package will use to refer
to the package) is taken from the defined `package_name` of the subpackage's
`fuchsia_package` target.

Subpackage targets can also be declared using the `package` variable in
the `renameable_subpackages` list. `renameable_targets` also include an optional
`name` variable, to override the default name for the subpackage.

### Declaring subpackaged children

A subpackage is only visible to its parent package, and the component(s) in that
package. Consequently, subpackage names only need to be unique within that
parent package. If two subpackage targets have the same name (or for any other
reason), the parent is free to assign its own subpackage names (via
`renameable_subpackages` in GN, for instance).

When declaring subpackaged child components in CML, the `url` should be the
relative subpackaged component URL, as shown in the following example:

```json5
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="examples/components/subpackages/meta/echo_client_with_subpackaged_server.cml" region_tag="declare_children_statically" adjust_indentation="auto" %}
```

Subpackaged child components can also be referenced in runtime declarations,
such as when declaring children through [Realm Builder] APIs. For example:

```rust
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="examples/components/subpackages/src/lib.rs" region_tag="declare_children_dynamically" adjust_indentation="auto" %}
```

[Packages]: package.md
[Realm Builder]: /docs/development/testing/components/realm_builder.md