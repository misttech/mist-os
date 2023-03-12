# escher/mesh/

## Indexed Triangle Mesh

`IndexedTriangleMesh` is a CPU-based mesh representation which is amenable to geometric algorithms
such as clipping against an oriented plane.

## Tessellation

Provided functions to generate a variety of 2D and 3D mesh shapes, e.g.
- sphere
- circle
- ring
- rounded rectangle

Some of these produce a mesh that has already been uploaded to the GPU, and others produce an
`IndexedTriangleMesh` which can be processed further before uploading.
