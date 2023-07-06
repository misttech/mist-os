# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""The library module handles creating Python classes and types based on FIDL IR rules."""
from __future__ import annotations

import dataclasses
import enum
import inspect
import json
import keyword
import os
import re
import sys
import types

from typing import Dict, List, Optional, Mapping, ForwardRef, Set, Union, Sequence, Tuple, Callable, Iterable, Any
from fidl_codec import add_ir_path
from ._client import FidlClient

LIB_MAP: Dict[str, str] = {}
MAP_INIT = False

# Defines a mapping from import names, e.g. "fidl.foo_bar_baz" to IR representations.
#
# load_ir_from_import should be the only function that touches the IR_MAP.
IR_MAP: Dict[str, IR] = {}


class Method(dict):
    """A light wrapper around a dict that represents a FIDL method."""

    def __init__(self, parent_ir, json_dict):
        super().__init__(json_dict)
        self.parent_ir = parent_ir

    def has_response(self) -> bool:
        """Returns True if the method has a response."""
        return bool(self["has_response"])

    def has_request(self) -> bool:
        """Returns True if the method has a request."""
        return bool(self["has_request"])

    def request_payload_identifier(self) -> str:
        """Attempts to lookup the payload identifier if it exists.

        Returns:
            None if there is no identifier, else an identifier string.
        """
        assert "maybe_request_payload" in self
        return self["maybe_request_payload"]["identifier"]

    def name(self) -> str:
        return self["name"]


class IR(dict):
    """A light wrapper around a dict that contains some convenience lookup methods."""

    def __init__(self, path, json_dict):
        super().__init__(json_dict)
        self.path = path
        if "library_dependencies" in self:
            # The names of these fields are specific to how they are declared in the IR. This is so
            # they can be programmatically looked up through reflection.
            #
            # See _sorted_type_declarations for an example of looking these fields up.
            for decl in ["bits", "enum", "struct", "table", "union", "const",
                         "alias", "protocol"]:
                setattr(self, f"{decl}_decls", self._decl_dict(decl))

    def _decl_dict(self, ty: str) -> Dict[str, IR]:
        return {x["name"]: IR(self.path, x) for x in self[f"{ty}_declarations"]}

    def name(self) -> str:
        return self["name"]

    def methods(self) -> List[Method]:
        return [Method(self, x) for x in self["methods"]]

    def declaration(self, identifier: str) -> Optional[str]:
        """Returns the declaration from the set of 'declarations,' None if not in the set.

        Args:
            identifier: The FIDL identifier, e.g. foo.bar/Baz to denote the Baz struct from library
            foo.bar.

        Returns:
            The identifier's declaration type, or None if not found. The declaration type is a FIDL
            type declaration, e.g. "const," "struct," "table," etc.
        """
        return self["declarations"].get(identifier)

    def _sorted_type_declarations(self, ty: str) -> List[IR]:
        """Returns type declarations in their IR sorted order.

        Args:
            ty: The type of the declaration as a string, e.g. "const," "table," "union," etc.

        Returns:
            The declarations within this IR library, sorted by their dependency order (in order of
            least dependencies to most dependencies. An increase in dependencies means that a type
            is composited with more elements).

        This ensures that declarations, when being exported as types for the given module, are
        constructed in the correct dependency order.
        """
        return [
            getattr(self, f"{ty}_decls")[x]
            for x in self["declaration_order"]
            if self.declaration(x) == ty
        ]

    def protocol_declarations(self) -> List[IR]:
        return self._sorted_type_declarations("protocol")

    def union_declarations(self) -> List[IR]:
        return self._sorted_type_declarations("union")

    def const_declarations(self) -> List[IR]:
        return self._sorted_type_declarations("const")

    def bits_declarations(self) -> List[IR]:
        return self._sorted_type_declarations("bits")

    def enum_declarations(self) -> List[IR]:
        return self._sorted_type_declarations("enum")

    def struct_declarations(self) -> List[IR]:
        return self._sorted_type_declarations("struct")

    def table_declarations(self) -> List[IR]:
        return self._sorted_type_declarations("table")

    def resolve_kind(self, key) -> Tuple[str, str]:
        """Iteratively attempts to resolve the passed kind.

        Returns not only the kind, but the IR in which the kind was found.
        """
        kind = self.declaration(key)
        ir = self
        # If the kind is none, then it is declared in a separate library and must be imported and
        # further unwrapped.
        while kind is None:
            library = fidl_ident_to_py_import(key)
            ir = load_ir_from_import(library)
            kind = ir.declaration(key)
        return (
            kind,
            next(d for d in ir[f"{kind}_declarations"] if d["name"] == key))


def fidl_ir_prefix_path() -> str:
    """Returns the prefix to the FIDL IR.

    If FUCHSIA_DIR is not set in the environment, or .fx-build-dir does not exist, this returns an
    empty string.
    """
    fuchsia_dir = os.environ.get("FUCHSIA_DIR")
    if fuchsia_dir:
        try:
            with open(os.path.join(fuchsia_dir, ".fx-build-dir"), "r") as f:
                build_dir = f.readlines()[0].strip()
                return os.path.join(fuchsia_dir, build_dir)
        except FileNotFoundError:
            return ""
    return ""


def get_fidl_ir_map() -> Mapping[str, str]:
    """Returns a singleton mapping of library names to FIDL files."""
    # This operates under the assumption that the topmost element in the FIDL IR file is the name
    # of the library. This is potentially very fragile. It is currently implemented this way to
    # improve speed, because otherwise this function will parse an entire several-megabyte sized
    # JSON file to look up a single top-level element. Another approach may be to use a regex or
    # some parser that parses a JSON object only one level deep.
    global MAP_INIT
    if MAP_INIT:
        return LIB_MAP
    string_start = '"name": "'
    string_end = '",'
    # TODO(fxbug.dev/128618): Create an index at build time rather than parsing everything at
    # runtime.
    # TODO(fxbug.dev/128218): Determining where IR is located MUST not only be done using
    # all_fidl_json.txt; this only works in-tree, and it only works if the necessary environment
    # parameters are set (which can be done automatically when using `fx test`, for example, but
    # again that is only available in tree). This hinders users who are out-of-tree and who wish to
    # simply "play around" with fuchsia controller in an interactive way.
    prefix = fidl_ir_prefix_path()
    with open(os.path.join(prefix, "all_fidl_json.txt"), "r",
              encoding="UTF-8") as f:
        while lib := f.readline().strip():
            full_path = os.path.join(prefix, lib)
            try:
                with open(full_path, "r", encoding="UTF-8") as ir_file:
                    while line := ir_file.readline().strip():
                        if line.startswith(string_start) and line.endswith(
                                string_end):
                            lib_name = line[len(string_start):-len(string_end)]
                            # This does not check for any conflicts.
                            LIB_MAP[lib_name] = os.path.join(prefix, full_path)
                            break
            except FileNotFoundError:
                continue
    MAP_INIT = True
    return LIB_MAP


def string_to_basetype(t: str) -> type:
    """Takes a base type like int32, bool, etc, and returns a Python type encapsulating it.

    Examples:
        "int32" would return the type `int`.
        "float64" would return the type `bool`.

    Returns:
        The type represented by the string.
    """
    if t.startswith("int") or t.startswith("uint"):
        return int
    elif t.startswith("float"):
        return float
    elif t == "bool":
        return bool
    else:
        raise Exception(f"Unsupported subtype: {t}")


def fidl_import_to_fidl_library(name: str) -> str:
    """Converts a fidl import, e.g. fidl.foo_bar_baz, to a fidl library: 'foo.bar.baz'"""
    assert name.startswith("fidl.")
    short_name = name[len("fidl."):]
    short_name = short_name.replace("_", ".")
    return short_name


def fidl_import_to_library_path(name: str) -> str:
    """Returns a fidl IR path based on the fidl import name."""
    try:
        return get_fidl_ir_map()[fidl_import_to_fidl_library(name)]
    except KeyError:
        raise RuntimeError(
            f"Unable to import library {name}." +
            " Please ensure that the FIDL IR for this library has been created."
        )


def type_annotation(type_ir, root_ir, recurse_guard=None) -> type:
    """Attempts to turn a type's IR representation into a type annotation for class constructions."""

    def wrap_optional(annotation):
        try:
            if type(annotation) == str:
                annotation = ForwardRef(annotation)
            if type_ir["nullable"]:
                return Optional[annotation]
        except KeyError:
            pass
        finally:
            return annotation

    kind = type_ir["kind"]
    if kind == "identifier":
        ident = type_ir["identifier"]
        ident_kind = get_kind_by_identifier(ident, root_ir)
        ty = get_type_by_identifier(ident, root_ir, recurse_guard)
        if ident_kind == "bits":
            py_name = Union[ty, Set[ty]]
        return wrap_optional(ty)
    elif kind == "primitive":
        return string_to_basetype(type_ir["subtype"])
    elif kind == "handle":
        return wrap_optional(f"zx.{type_ir['subtype']}")
    elif kind == "string":
        return wrap_optional(str)
    elif kind == "vector" or kind == "array":
        element_type = type_ir["element_type"]
        if element_type["kind"] == "primitive" and element_type[
                "subtype"] == "uint8":
            return wrap_optional(bytes)
        else:
            ty = type_annotation(element_type, root_ir, recurse_guard)
            return wrap_optional(Sequence[ty])
    elif kind == "request":
        return wrap_optional(
            fidl_ident_to_py_library_member(type_ir["subtype"]) + ".Server")
    raise TypeError(
        f"As yet unsupported type in library {root_ir['name']}: {kind}")


def fidl_library_to_py_module_path(name: str) -> str:
    """Converts a fidl library, e.g. foo.bar.baz into a Python-friendly import: fidl.foo_bar_baz"""
    return "fidl." + name.replace(".", "_")


def fidl_ident_to_library(name: str) -> str:
    """Takes a fidl identifier and returns the library: foo.bar.baz/Foo would return foo.bar.baz"""
    return name.split("/")[0]


def fidl_ident_to_py_import(name: str) -> str:
    """Python import from fidl identifier: foo.bar.baz/Foo would return fidl.foo_bar_baz"""
    fidl_lib = fidl_ident_to_library(name)
    return fidl_library_to_py_module_path(fidl_lib)


def fidl_ident_to_marker(name: str) -> str:
    """Changes a fidl library member to a marker used for protocol lookup.

    Returns: foo.bar.baz/Foo returns foo.bar.baz.Foo
    """
    return name.replace("/", ".")


def fidl_ident_to_py_library_member(name: str) -> str:
    """Returns fidl library member name from identifier: foo.bar.baz/Foo would return Foo"""
    return name.split("/")[1]


def docstring(decl, default: Optional[str] = None) -> Optional[str]:
    """Constructs docstring from a fidl's IR documentation declaration if it exists."""
    doc_attr = next(
        (
            attr for attr in decl.get("maybe_attributes", [])
            if attr["name"] == "doc"),
        None,
    )
    if doc_attr is None:
        return default
    return doc_attr["arguments"][0]["value"]["value"].strip()


def bits_or_enum_root_type(ir, type_name: str) -> enum.EnumMeta:
    """Constructs a Python type from either bits or enums (they are quite similar so they bottom out
    on this function)."""
    name = fidl_ident_to_py_library_member(ir.name())
    members = {
        member["name"]: int(member["value"]["value"])
        for member in ir["members"]
    }
    ty = enum.IntFlag(name, members)
    setattr(ty, "__fidl_kind__", type_name)
    setattr(ty, "__doc__", docstring(ir))
    return ty


def bits_type(ir) -> enum.EnumMeta:
    """Constructs a Python type from a bits declaration in IR."""
    return bits_or_enum_root_type(ir, "bits")


def enum_type(ir) -> enum.EnumMeta:
    """Constructs a Python type from a bits declaration in IR."""
    return bits_or_enum_root_type(ir, "enum")


def _union_get_value(self):
    """Helper function that attempts to get the union value."""
    items = [
        m[0].replace('_type', '')
        for m in inspect.getmembers(self)
        if m[0].endswith('_type')
    ]
    got = None
    for i in items:
        got = getattr(self, i)
        if got is not None:
            break
    return got


def union_repr(self) -> str:
    """Returns the union repr in the format <'foo.bar.baz/FooUnion' object({value})>

    If {value} is not set, will write None."""
    return f"<'{self.__fidl_type__}' object({_union_get_value(self)})>"


def union_str(self) -> str:
    """Returns the union string representation, e.g. whatever the union type has been set to."""
    return str(_union_get_value(self))


def union_type(ir, root_ir, recurse_guard=None) -> type:
    """Constructs a Python type from a FIDL IR Union declaration."""
    name = fidl_ident_to_py_library_member(ir.name())
    base = type(
        name,
        (),
        {
            "__doc__": docstring(ir),
            "__fidl_kind__": "union",
            "__repr__": union_repr,
            "__str__": union_str,
            "__fidl_type__": ir.name(),
        },
    )
    # TODO(fxbug.dev/127787): Prevent unions from having more than one value set at the same time.
    # This is silently allowed during encoding, but should either be prevented during encoding, or
    # prevented from within the object itself. If it cannot be prevented there should be some hook
    # that sets the previous union variants to None.
    for member in ir["members"]:
        if member["reserved"]:
            continue
        member_name = member["name"]
        member_type_name = member_name + "_type"
        member_type = dataclasses.make_dataclass(
            member_type_name, [
                (
                    "value",
                    type_annotation(member["type"], root_ir, recurse_guard))
            ])
        setattr(member_type, "__doc__", docstring(member))
        setattr(member_type, "__repr__", lambda self: self.value.__repr__())
        setattr(
            base, member_type_name,
            type_annotation(member["type"], root_ir, recurse_guard))
        setattr(base, member_name, None)
    return base


def normalize_member_name(name) -> str:
    """Prevents use of names for struct or table members that are already keywords"""
    if name in keyword.kwlist:
        return name + "_"
    return name


def struct_type(ir, root_ir, recurse_guard=None) -> type:
    """Constructs a Python type from a FIDL IR struct declaration."""
    name = fidl_ident_to_py_library_member(ir.name())
    members = [
        (
            normalize_member_name(member["name"]),
            type_annotation(member["type"], root_ir, recurse_guard))
        for member in ir["members"]
    ]
    ty = dataclasses.make_dataclass(name, members)
    setattr(ty, "__fidl_kind__", "struct")
    setattr(ty, "__fidl_type__", ir.name())
    setattr(ty, "__doc__", docstring(ir))
    return ty


def table_type(ir, root_ir, recurse_guard=None) -> type:
    """Constructs a Python type from a FIDL IR table declaration."""
    name = fidl_ident_to_py_library_member(ir.name())
    members = []
    for member in ir["members"]:
        if member["reserved"]:
            continue
        optional_ty = type_annotation(member["type"], root_ir, recurse_guard)
        new_member = normalize_member_name(
            member["name"]), Optional[optional_ty], dataclasses.field(
                default=None)
        members.append(new_member)
    it: Iterable[Tuple[str, type, Any]] = members
    ty = dataclasses.make_dataclass(name, it)
    setattr(ty, "__fidl_kind__", "table")
    setattr(ty, "__fidl_type__", ir.name())
    setattr(ty, "__doc__", docstring(ir))
    return ty


class FIDLConstant(object):

    def __init__(self, name, value):
        self.name = name
        self.value = value


def primitive_converter(subtype: str) -> type:
    if "int" in subtype:
        return int
    elif subtype == "bool":
        return bool
    elif "float" in subtype:
        return float
    raise TypeError(f"Unrecognized type: {subtype}")


def const_declaration(ir, root_ir, recurse_guard=None) -> FIDLConstant:
    """Constructs a Python type from a FIDL IR const declaration."""
    name = fidl_ident_to_py_library_member(ir.name())
    kind = ir["type"]["kind"]
    if kind == "primitive":
        converter = primitive_converter(ir["type"]["subtype"])
        return FIDLConstant(name, converter(ir["value"]["value"]))
    elif kind == "identifier":
        ident = ir["type"]["identifier"]
        ty = get_type_by_identifier(ident, root_ir, recurse_guard)
        if type(ty) == str:
            return FIDLConstant(name, ty(ir["value"]["value"]))
        elif ty.__class__ == enum.EnumMeta:
            return FIDLConstant(name, ty(int(ir["value"]["value"])))
        raise TypeError(
            f"As yet unsupported identifier type in lib '{root_ir['name']}': {type(ty)}"
        )
    elif kind == "string":
        return FIDLConstant(name, ir["value"]["value"])
    raise TypeError(
        f"As yet unsupported type in library '{root_ir['name']}': {kind}")


def protocol_type(ir, root_ir, recurse_guard=None) -> type:
    """Constructs a Python type from a FIDL IR protocol declaration."""
    name = fidl_ident_to_py_library_member(ir.name())
    return type(
        name,
        (object,),
        {
            "__doc__": docstring(ir),
            "__fidl_kind__": "protocol",
            "Client": protocol_client_type(ir, root_ir),
            "MARKER": fidl_ident_to_marker(ir.name()),
        },
    )


def protocol_client_type(ir: IR, root_ir) -> type:
    name = fidl_ident_to_py_library_member(ir.name())
    properties = {
        "__doc__": docstring(ir),
        "__fidl__kind__": "client",
    }
    for method in ir.methods():
        if not method.has_request():
            # This is an event. This needs to be handled on its own.
            continue
        method_snake_case = re.sub(r"(?<!^)(?=[A-Z])", "_",
                                   method.name()).lower()
        properties[method_snake_case] = protocol_client_method(method, root_ir)
    return type(name, (FidlClient,), properties)


def get_fidl_request_lambda(ir: Method, root_ir, msg) -> Callable:
    if ir.has_response():
        response_ident = ""
        if ir.get("maybe_response_payload"):
            response_kind = ir["maybe_response_payload"]["kind"]
            if response_kind == "identifier":
                ident = ir["maybe_response_payload"]["identifier"]
                # Just ensures the module for this is going to be imported.
                get_kind_by_identifier(ident, root_ir)
                response_ident = ident
            else:
                response_ident = response_kind
        if msg:
            return lambda self, **args: self._send_two_way_fidl_request(
                ir["ordinal"], root_ir.name(), msg(**args), response_ident)
        return lambda self: self._send_two_way_fidl_request(
            ir["ordinal"], root_ir.name(), msg, response_ident)
    if msg:
        return lambda self, **args: self._send_one_way_fidl_request(
            0, ir["ordinal"], root_ir.name(), msg(**args))
    return lambda self: self._send_one_way_fidl_request(
        0, ir["ordinal"], root_ir.name(), msg)


def protocol_client_method(
        method: Method, root_ir, recurse_guard=None) -> Callable:
    assert method.has_request()
    if "maybe_request_payload" in method:
        req_id = method.request_payload_identifier()
        (req_kind, req_ir) = root_ir.resolve_kind(req_id)
    else:
        req_kind = None
        method_impl = get_fidl_request_lambda(method, root_ir, None)
    if req_kind == "struct":
        params = [
            inspect.Parameter(
                normalize_member_name(member["name"]),
                inspect.Parameter.KEYWORD_ONLY,
                annotation=type_annotation(
                    member["type"], root_ir, recurse_guard),
            ) for member in req_ir["members"]
        ]
        method_impl = get_fidl_request_lambda(
            method, root_ir, get_type_by_identifier(req_id, root_ir))
    elif req_kind == "table":
        params = [
            inspect.Parameter(
                member["name"],
                inspect.Parameter.KEYWORD_ONLY,
                default=None,
                annotation=type_annotation(
                    member["type"], root_ir, recurse_guard),
            ) for member in req_ir["members"] if not member["reserved"]
        ]
        method_impl = get_fidl_request_lambda(
            method, root_ir, get_type_by_identifier(req_id, root_ir))
    elif req_kind == "union":
        params = [
            inspect.Parameter(
                member["name"],
                inspect.Parameter.POSITIONAL_ONLY,
                annotation=type_annotation(
                    member['type'], root_ir, recurse_guard))
            for member in req_ir["members"]
            if not member["reserved"]
        ]
        method_impl = get_fidl_request_lambda(
            method, root_ir, get_type_by_identifier(req_id, root_ir))
    elif req_kind == None:
        params = []
    else:
        raise RuntimeError(f"Unrecognized method parameter kind: {req_kind}")

    setattr(method_impl, "__signature__", inspect.Signature(params))
    setattr(method_impl, "__doc__", docstring(method))
    setattr(method_impl, "__fidl_type__", method.name())
    setattr(method_impl, "__name__", method.name())
    return method_impl


def load_ir_from_import(import_name: str) -> IR:
    """Takes an import name, loads/caches the IR, and returns it."""
    lib = fidl_import_to_library_path(import_name)
    if lib not in IR_MAP:
        with open(lib, "r", encoding="UTF-8") as f:
            IR_MAP[lib] = IR(lib, json.load(f))
    return IR_MAP[lib]


def get_kind_by_identifier(ident: str, loader_ir) -> str:
    """Takes a fidl identifier, e.g. foo.bar.baz/Foo and returns its 'kind'.

    e.g. "struct," "table," etc."""
    res = loader_ir.declaration(ident)
    if res is not None:
        return res
    return get_type_by_identifier(ident, loader_ir).__fidl_kind__


def get_type_by_identifier(ident: str, loader_ir, recurse_guard=None) -> type:
    """Takes a identifier, e.g. foo.bar.baz/Foo and returns its Python type."""
    member_name = fidl_ident_to_py_library_member(ident)
    mod = load_module(fidl_ident_to_py_import(ident))
    if not hasattr(mod, member_name):
        if recurse_guard is not None:
            return ForwardRef(member_name)
        # library.name/Ident for example.
        ident = f"{fidl_import_to_fidl_library(mod.__fullname__)}/{member_name}"
        # e.g. protocol, struct, union, etc.
        ty = mod.__ir__.declaration(ident)
        # IR for library.name/Ident
        ty_decl = getattr(mod.__ir__, f"{ty}_decls")[ident]
        ty_definition = globals()[f"{ty}_type"](
            ty_decl, mod.__ir__, recurse_guard=True)
        if ty == "const":
            # This line might not actually be possible to hit.
            mod.export_const(ty_definition)
        else:
            mod.export_type(ty_definition)
    return getattr(mod, member_name)


def load_module(fullname: str) -> types.ModuleType:
    if fullname not in sys.modules:
        sys.modules[fullname] = FIDLLibraryModule(fullname)
    return sys.modules[fullname]


class FIDLLibraryModule(types.ModuleType):

    def __init__(self, fullname: str):
        # Shove ourselves into the import map so that composite types can be looked up as they are
        # exported.
        sys.modules[fullname] = self
        ir_path = fidl_import_to_library_path(fullname)
        add_ir_path(ir_path)
        self.__ir__ = load_ir_from_import(fullname)
        self.__file__ = f"<FIDL JSON:{ir_path}>"
        self.__fullname__ = fullname
        super().__init__(
            fullname,
            docstring(self.__ir__, f"FIDL library {self.__ir__.name()}"))
        self.__all__: List[str] = []

        self.export_bits()
        self.export_enums()
        self.export_structs()
        self.export_tables()
        self.export_unions()
        self.export_consts()
        self.export_aliases()
        self.export_protocols()

    def export_protocols(self):
        for decl in self.__ir__.protocol_declarations():
            if decl.name() not in self.__all__:
                self.export_type(protocol_type(decl, self.__ir__))

    def export_structs(self):
        for decl in self.__ir__.struct_declarations():
            if decl.name() not in self.__all__:
                self.export_type(struct_type(decl, self.__ir__))

    def export_tables(self):
        for decl in self.__ir__.table_declarations():
            if decl.name() not in self.__all__:
                self.export_type(table_type(decl, self.__ir__))

    def export_bits(self):
        for decl in self.__ir__.bits_declarations():
            if decl.name() not in self.__all__:
                self.export_type(bits_type(decl))

    def export_enums(self):
        for decl in self.__ir__.enum_declarations():
            if decl.name() not in self.__all__:
                self.export_type(enum_type(decl))

    def export_consts(self):
        for decl in self.__ir__.const_declarations():
            if decl.name() not in self.__all__:
                self.export_fidl_const(const_declaration(decl, self.__ir__))

    def export_aliases(self):
        # TODO(fxbug.dev/127714): support aliases. This will require constructing types based on
        # other types.
        pass

    def export_unions(self):
        for decl in self.__ir__.union_declarations():
            if decl.name() not in self.__all__:
                self.export_type(union_type(decl, self.__ir__))

    def export_fidl_const(self, c):
        setattr(self, c.name, c.value)
        self.__all__.append(c.name)

    def export_type(self, t):
        setattr(self, t.__name__, t)
        self.__all__.append(t.__name__)
