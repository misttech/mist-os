# TEE Internal Core API

include/ contains the header files for the TEE Internal Core API v1.2.1.

* src/ contains the implementation of the API. It is currently a shared library,
although it might not stay that way.

* src/binding.rs is the generated Rust definitions based on the header files. It
is manually updated by running bindgen.sh and checking in the results.