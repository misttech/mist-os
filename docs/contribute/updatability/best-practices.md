# Platform updatability best practices

<span class="compare-better">Recommended</span>: Use [FIDL] types and protocols
to define interfaces between any two things that may update separately. Leverage
the [Fuchsia API evolution guidelines][fidl-evolution] and
[FIDL rubric][fidl-rubric] where applicable.

- FIDL has [versioning annotations][fidl-versioning].
- FIDL offers [API and ABI compatibility guarantees][fidl-compatibility].
- FIDL enables soft transitions by supporting
  [changing type definitions][rfc-0061],
  [adding and removing methods over time][rfc-0021], and
  [renaming types][rfc-0048].

<span class="compare-worse">Not recommended</span>: Avoid languages other than
FIDL to define interfaces where independent updates matter. These include:
plain text, JSON, and protocol buffers.

When reviewing alternatives, ask yourself what affordances they have for
updatability.

- Is there a schema for the data?
- Can the schema change over time, while providing backward/forward
  compatibility? How?
- What changes to the schema are API/ABI preserving/breaking? How would you know
  before committing a breaking change?
- Is the wire format stable?

<span class="compare-better">Recommended</span>: Be careful when designing
platform APIs and ABIs for use outside the platform. Design for updatability, find
ways to enforce that your clients use the intended interfaces, and don’t offer
ways to circumvent the interface.

<span class="compare-worse">Not recommended</span>: Avoid exposing your clients
to your implementation details that are not contractual. Common mistakes include
exposing broadly-scoped capabilities or namespaces, and leaking implementation
details via [component identifiers][identifiers] (such as `fuchsia-pkg://` URLs)
and [diagnostics selectors][selectors].

[fidl]: /docs/concepts/fidl/overview.md
[fidl-compatibility]: /docs/development/languages/fidl/guides/compatibility/README.md
[fidl-evolution]: /docs/development/api/evolution.md
[fidl-rubric]: /docs/development/api/fidl.md
[identifiers]: /docs/concepts/components/v2/identifiers.md
[rfc-0021]: /docs/contribute/governance/rfcs/0021_soft_transitions_methods_add_remove.md
[rfc-0048]: /docs/contribute/governance/rfcs/0048_explicit_union_ordinals.md
[rfc-0061]: /docs/contribute/governance/rfcs/0061_extensible_unions.md
[fidl-versioning]: /docs/reference/fidl/language/versioning.md
[selectors]: /docs/reference/diagnostics/selectors.md
