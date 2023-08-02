# Bedrock security invariants

The purpose of this document is to rationalize the security guarantees the
component framework offers, in a way that is easy to understand and verify.

## Context on bedrock

The bedrock layer is a collection of imperative APIs that define the basic
concepts, operations, and invariants of the component framework. The declarative
aspects of the component framework is layered on top of bedrock.

## Principles of this document

- An *invariant* is a condition that always holds given some assumptions. No
  caveats allowed.

  For example, _a component can never do `foo` given capabilities `bar,baz`_.

- Restrict scope to a bedrock model for components. Not `component_manager` as
  it exists in June 2023.

  For example, this document defines the kinds of operations on programs,
  components, resolvers, runners, sandboxes, etc, and their semantics.

  Some components may be statically declared in practice. The model will not
  make distinctions between static or dynamic components since that's a
  `component_manager` concept. It will reason in abstract about the operations a
  component can perform and provide an upper bound on the possible behavior.

- Invariants that hold true only under certain configurations of a topology are
  described separately from this document.

  For example, the model does not include specific runners or resolvers. That's
  dependent on specific topologies and the invariants in that topology will be
  layered on top of the bedrock invariants.

- APIs and their implementation reference concepts in this document and reflect
  their invariants in a verifiable fashion.

TODO(fxbug.dev/129662): start filling out the model.
