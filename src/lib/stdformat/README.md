This library provides polyfills for different versions of the standard library.

Constructs are withing namespaces for each specific version, such as "cpp23, cpp26".

Polyfills will only be used when the standard version is not available.

Unlike the stdcompat library, this library is primarily focused on std::format related
APIs such as std::print. It also assumes C++20 is available and is not scoped to be
included in the SDK as a result.
