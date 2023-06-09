// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_INCLUDE_FIDL_DIAGNOSTICS_H_
#define TOOLS_FIDL_FIDLC_INCLUDE_FIDL_DIAGNOSTICS_H_

#include <string_view>
#include <vector>

#include "tools/fidl/fidlc/include/fidl/diagnostic_types.h"
#include "tools/fidl/fidlc/include/fidl/fixables.h"
#include "tools/fidl/fidlc/include/fidl/source_span.h"
#include "tools/fidl/fidlc/include/fidl/versioning_types.h"

namespace fidl {

constexpr ErrorDef<1, std::string_view> ErrInvalidCharacter("invalid character '{}'");
constexpr ErrorDef<2> ErrUnexpectedLineBreak("unexpected line-break in string literal");
constexpr ErrorDef<3, std::string_view> ErrInvalidEscapeSequence("invalid escape sequence '{}'");
constexpr ErrorDef<4, char> ErrInvalidHexDigit("invalid hex digit '{}'");
constexpr RetiredDef<5> ErrInvalidOctDigit;
constexpr ErrorDef<6, std::string_view> ErrExpectedDeclaration("invalid declaration type {}");
constexpr ErrorDef<7> ErrUnexpectedToken("found unexpected token");
constexpr ErrorDef<8, Token::KindAndSubkind, Token::KindAndSubkind> ErrUnexpectedTokenOfKind(
    "unexpected token {}, was expecting {}");
constexpr ErrorDef<9, Token::KindAndSubkind, Token::KindAndSubkind> ErrUnexpectedIdentifier(
    "unexpected identifier {}, was expecting {}");
constexpr ErrorDef<10, std::string_view> ErrInvalidIdentifier("invalid identifier '{}'");
constexpr ErrorDef<11, std::string_view> ErrInvalidLibraryNameComponent(
    "Invalid library name component {}");
constexpr ErrorDef<12> ErrInvalidLayoutClass(
    "layouts must be of the class: bits, enum, struct, table, or union.");
constexpr ErrorDef<13> ErrInvalidWrappedType("wrapped type for bits/enum must be an identifier");
constexpr ErrorDef<14> ErrAttributeWithEmptyParens(
    "attributes without arguments must omit the trailing empty parentheses");
constexpr ErrorDef<15> ErrAttributeArgsMustAllBeNamed(
    "attributes that take multiple arguments must name all of them explicitly");
constexpr ErrorDef<16> ErrMissingOrdinalBeforeMember("missing ordinal before member");
constexpr ErrorDef<17> ErrOrdinalOutOfBound("ordinal out-of-bound");
constexpr ErrorDef<18> ErrOrdinalsMustStartAtOne("ordinals must start at 1");
constexpr ErrorDef<19> ErrMustHaveOneMember("must have at least one member");
constexpr ErrorDef<20> ErrInvalidProtocolMember("invalid protocol member");
constexpr RetiredDef<21> ErrExpectedProtocolMember;
constexpr ErrorDef<22> ErrCannotAttachAttributeToIdentifier(
    "cannot attach attributes to identifiers");
constexpr ErrorDef<23> ErrRedundantAttributePlacement(
    "cannot specify attributes on the type declaration and the corresponding layout at the same "
    "time; please merge them into one location instead");
constexpr ErrorDef<24> ErrDocCommentOnParameters("cannot have doc comment on parameters");
constexpr ErrorDef<25> ErrLibraryImportsMustBeGroupedAtTopOfFile(
    "library imports must be grouped at top-of-file");
constexpr WarningDef<26> WarnCommentWithinDocCommentBlock(
    "cannot have comment within doc comment block");
constexpr WarningDef<27> WarnBlankLinesWithinDocCommentBlock(
    "cannot have blank lines within doc comment block");
constexpr WarningDef<28> WarnDocCommentMustBeFollowedByDeclaration(
    "doc comment must be followed by a declaration");
constexpr ErrorDef<29> ErrMustHaveOneProperty("must have at least one property");
constexpr ErrorDef<30, Token::KindAndSubkind, Token::KindAndSubkind> ErrCannotSpecifyModifier(
    "cannot specify modifier {} for {}");
constexpr ErrorDef<31, Token::KindAndSubkind> ErrCannotSpecifySubtype(
    "cannot specify subtype for {}");
constexpr ErrorDef<32, Token::KindAndSubkind> ErrDuplicateModifier(
    "duplicate occurrence of modifier {}");
constexpr ErrorDef<33, Token::KindAndSubkind, Token::KindAndSubkind> ErrConflictingModifier(
    "modifier {} conflicts with modifier {}");
constexpr ErrorDef<34, std::string_view, SourceSpan> ErrNameCollision(
    "the name '{}' conflicts with another declaration at {}");
constexpr ErrorDef<35, std::string_view, std::string_view, SourceSpan, std::string_view>
    ErrNameCollisionCanonical(
        "the name '{}' conflicts with '{}' from {}; both are represented by "
        "the canonical form '{}'");
constexpr ErrorDef<36, std::string_view, SourceSpan, VersionSet, Platform> ErrNameOverlap(
    "the name '{}' conflicts with another declaration at {}; both are "
    "available {} of platform '{}'");
constexpr ErrorDef<37, std::string_view, std::string_view, SourceSpan, std::string_view, VersionSet,
                   Platform>
    ErrNameOverlapCanonical(
        "the name '{}' conflicts with '{}' from {}; both are represented "
        "by the canonical form '{}' and are available {} of platform '{}'");
constexpr ErrorDef<38, flat::Name> ErrDeclNameConflictsWithLibraryImport(
    "Declaration name '{}' conflicts with a library import. Consider using the "
    "'as' keyword to import the library under a different name.");
constexpr ErrorDef<39, flat::Name, std::string_view> ErrDeclNameConflictsWithLibraryImportCanonical(
    "Declaration name '{}' conflicts with a library import due to its "
    "canonical form '{}'. Consider using the 'as' keyword to import the "
    "library under a different name.");
constexpr ErrorDef<40> ErrFilesDisagreeOnLibraryName(
    "Two files in the library disagree about the name of the library");
constexpr ErrorDef<41, std::vector<std::string_view>> ErrMultipleLibrariesWithSameName(
    "There are multiple libraries named '{}'");
constexpr ErrorDef<42, std::vector<std::string_view>> ErrDuplicateLibraryImport(
    "Library {} already imported. Did you require it twice?");
constexpr ErrorDef<43, std::vector<std::string_view>> ErrConflictingLibraryImport(
    "import of library '{}' conflicts with another library import");
constexpr ErrorDef<44, std::vector<std::string_view>, std::string_view>
    ErrConflictingLibraryImportAlias(
        "import of library '{}' under alias '{}' conflicts with another library import");
constexpr ErrorDef<45, const raw::AttributeList *> ErrAttributesNotAllowedOnLibraryImport(
    "no attributes allowed on library import, found: {}");
constexpr ErrorDef<46, std::vector<std::string_view>> ErrUnknownLibrary(
    "Could not find library named {}. Did you include its sources with --files?");
constexpr ErrorDef<47, SourceSpan> ErrProtocolComposedMultipleTimes(
    "protocol composed multiple times; previous was at {}");
constexpr ErrorDef<48> ErrOptionalTableMember("Table members cannot be optional");
constexpr ErrorDef<49> ErrOptionalUnionMember("Union members cannot be optional");
constexpr ErrorDef<50> ErrDeprecatedStructDefaults(
    "Struct defaults are deprecated and should not be used (see RFC-0160)");
constexpr ErrorDef<51, std::vector<std::string_view>, std::vector<std::string_view>>
    ErrUnknownDependentLibrary(
        "Unknown dependent library {} or reference to member of "
        "library {}. Did you require it with `using`?");
constexpr ErrorDef<52, std::string_view, std::vector<std::string_view>> ErrNameNotFound(
    "cannot find '{}' in library '{}'");
constexpr ErrorDef<53, const flat::Decl *> ErrCannotReferToMember("cannot refer to member of {}");
constexpr ErrorDef<54, const flat::Decl *, std::string_view> ErrMemberNotFound(
    "{} has no member '{}'");
constexpr ErrorDef<55, const flat::Element *, VersionRange, Platform, const flat::Element *,
                   const flat::Element *>
    ErrInvalidReferenceToDeprecated(
        "invalid reference to {}, which is deprecated {} of platform '{}' while {} "
        "is not; either remove this reference or mark {} as deprecated");
constexpr ErrorDef<56, const flat::Element *, VersionRange, Platform, const flat::Element *,
                   VersionRange, Platform, const flat::Element *>
    ErrInvalidReferenceToDeprecatedOtherPlatform(
        "invalid reference to {}, which is deprecated {} of platform '{}' while {} "
        "is not deprecated {} of platform '{}'; either remove this reference or mark {} as "
        "deprecated");
// ErrIncludeCycle is thrown either as part of SortDeclarations or as part of
// CompileStep, depending on the type of the cycle, because SortDeclarations
// understands the support for boxed recursive structs, while CompileStep
// handles recursive protocols and self-referencing type-aliases.
constexpr ErrorDef<57, std::vector<const flat::Decl *>> ErrIncludeCycle(
    "There is an includes-cycle in declarations: {}");
constexpr ErrorDef<58, flat::Name> ErrAnonymousNameReference("cannot refer to anonymous name {}");
constexpr ErrorDef<59, const flat::Type *> ErrInvalidConstantType("invalid constant type {}");
constexpr ErrorDef<60> ErrCannotResolveConstantValue("unable to resolve constant value");
constexpr ErrorDef<61> ErrOrOperatorOnNonPrimitiveValue(
    "Or operator can only be applied to primitive-kinded values");
constexpr ErrorDef<62, flat::Name, std::string_view> ErrNewTypesNotAllowed(
    "newtypes not allowed: type declaration {} defines a new type of the existing {} type, which "
    "is not yet supported");
constexpr ErrorDef<63, flat::Name> ErrExpectedValueButGotType(
    "{} is a type, but a value was expected");
constexpr ErrorDef<64, flat::Name, flat::Name> ErrMismatchedNameTypeAssignment(
    "mismatched named type assignment: cannot define a constant or default value of type {} "
    "using a value of type {}");
constexpr ErrorDef<65, const flat::Constant *, const flat::Type *, const flat::Type *>
    ErrTypeCannotBeConvertedToType("{} (type {}) cannot be converted to type {}");
constexpr ErrorDef<66, const flat::Constant *, const flat::Type *> ErrConstantOverflowsType(
    "{} overflows type {}");
constexpr ErrorDef<67> ErrBitsMemberMustBePowerOfTwo("bits members must be powers of two");
constexpr ErrorDef<68, std::string_view> ErrFlexibleEnumMemberWithMaxValue(
    "flexible enums must not have a member with a value of {}, which is "
    "reserved for the unknown value. either: remove the member, change its "
    "value to something else, or explicitly specify the unknown value with "
    "the @unknown attribute. see "
    "<https://fuchsia.dev/fuchsia-src/reference/fidl/language/attributes#unknown> "
    "for more info.");
constexpr ErrorDef<69, const flat::Type *> ErrBitsTypeMustBeUnsignedIntegralPrimitive(
    "bits may only be of unsigned integral primitive type, found {}");
constexpr ErrorDef<70, const flat::Type *> ErrEnumTypeMustBeIntegralPrimitive(
    "enums may only be of integral primitive type, found {}");
constexpr ErrorDef<71> ErrUnknownAttributeOnStrictEnumMember(
    "the @unknown attribute can be only be used on flexible enum members.");
constexpr ErrorDef<72> ErrUnknownAttributeOnMultipleEnumMembers(
    "the @unknown attribute can be only applied to one enum member.");
constexpr ErrorDef<73> ErrComposingNonProtocol("This declaration is not a protocol");
constexpr ErrorDef<74, flat::Decl::Kind> ErrInvalidMethodPayloadLayoutClass(
    "cannot use {} as a request/response; must use a struct, table, or union");
constexpr ErrorDef<75, const flat::Type *> ErrInvalidMethodPayloadType(
    "invalid request/response type '{}'; must use a struct, table, or union");
constexpr RetiredDef<76> ErrResponsesWithErrorsMustNotBeEmpty;
constexpr ErrorDef<77, std::string_view> ErrEmptyPayloadStructs(
    "method '{}' cannot have an empty struct as a payload, prefer omitting the payload altogether");
constexpr ErrorDef<78, std::string_view, SourceSpan> ErrDuplicateMethodName(
    "multiple protocol methods named '{}'; previous was at {}");
constexpr ErrorDef<79, std::string_view, std::string_view, SourceSpan, std::string_view>
    ErrDuplicateMethodNameCanonical(
        "protocol method '{}' conflicts with method '{}' from {}; both are "
        "represented by the canonical form '{}'");
constexpr ErrorDef<80> ErrGeneratedZeroValueOrdinal("Ordinal value 0 disallowed.");
constexpr ErrorDef<81, SourceSpan> ErrDuplicateMethodOrdinal(
    "Multiple methods with the same ordinal in a protocol; previous was at {}.");
constexpr ErrorDef<82> ErrInvalidSelectorValue(
    "invalid selector value, must be a method name or a fully qualified method name");
constexpr ErrorDef<83> ErrFuchsiaIoExplicitOrdinals(
    "fuchsia.io must have explicit ordinals (https://fxbug.dev/77623)");
constexpr ErrorDef<84> ErrPayloadStructHasDefaultMembers(
    "default values are not allowed on members of request/response structs");
constexpr ErrorDef<85, std::string_view, SourceSpan> ErrDuplicateServiceMemberName(
    "multiple service members named '{}'; previous was at {}");
constexpr ErrorDef<86> ErrStrictUnionMustHaveNonReservedMember(
    "strict unions must have at least one non-reserved member");
constexpr ErrorDef<87, std::string_view, std::string_view, SourceSpan, std::string_view>
    ErrDuplicateServiceMemberNameCanonical(
        "service member '{}' conflicts with member '{}' from {}; both are "
        "represented by the canonical form '{}'");
constexpr ErrorDef<88> ErrOptionalServiceMember("service members cannot be optional");
constexpr ErrorDef<89, std::string_view, SourceSpan> ErrDuplicateStructMemberName(
    "multiple struct fields named '{}'; previous was at {}");
constexpr ErrorDef<90, std::string_view, std::string_view, SourceSpan, std::string_view>
    ErrDuplicateStructMemberNameCanonical(
        "struct field '{}' conflicts with field '{}' from {}; both are represented "
        "by the canonical form '{}'");
constexpr ErrorDef<91, std::string_view, const flat::Type *> ErrInvalidStructMemberType(
    "struct field {} has an invalid default type {}");
constexpr ErrorDef<92> ErrTooManyTableOrdinals(
    "table contains too many ordinals; tables are limited to 64 ordinals");
constexpr ErrorDef<93> ErrMaxOrdinalNotTable(
    "the 64th ordinal of a table may only contain a table type");
constexpr ErrorDef<94, SourceSpan> ErrDuplicateTableFieldOrdinal(
    "multiple table fields with the same ordinal; previous was at {}");
constexpr ErrorDef<95, std::string_view, SourceSpan> ErrDuplicateTableFieldName(
    "multiple table fields named '{}'; previous was at {}");
constexpr ErrorDef<96, std::string_view, std::string_view, SourceSpan, std::string_view>
    ErrDuplicateTableFieldNameCanonical(
        "table field '{}' conflicts with field '{}' from {}; both are represented "
        "by the canonical form '{}'");
constexpr ErrorDef<97, SourceSpan> ErrDuplicateUnionMemberOrdinal(
    "multiple union fields with the same ordinal; previous was at {}");
constexpr ErrorDef<98, std::string_view, SourceSpan> ErrDuplicateUnionMemberName(
    "multiple union members named '{}'; previous was at {}");
constexpr ErrorDef<99, std::string_view, std::string_view, SourceSpan, std::string_view>
    ErrDuplicateUnionMemberNameCanonical(
        "union member '{}' conflicts with member '{}' from {}; both are represented "
        "by the canonical form '{}'");
constexpr ErrorDef<100, uint64_t> ErrNonDenseOrdinal(
    "missing ordinal {} (ordinals must be dense); consider marking it reserved");
constexpr ErrorDef<101> ErrCouldNotResolveSizeBound("unable to resolve size bound");
constexpr ErrorDef<102, std::string_view> ErrCouldNotResolveMember("unable to resolve {} member");
constexpr ErrorDef<103, std::string_view> ErrCouldNotResolveMemberDefault(
    "unable to resolve {} default value");
constexpr ErrorDef<104> ErrCouldNotResolveAttributeArg("unable to resolve attribute argument");
constexpr ErrorDef<105, std::string_view, std::string_view, SourceSpan> ErrDuplicateMemberName(
    "multiple {} members named '{}'; previous was at {}");
constexpr ErrorDef<106, std::string_view, std::string_view, std::string_view, SourceSpan,
                   std::string_view>
    ErrDuplicateMemberNameCanonical(
        "{} member '{}' conflicts with member '{}' from {}; both are "
        "represented by the canonical form '{}'");
constexpr ErrorDef<107, std::string_view, std::string_view, std::string_view, SourceSpan>
    ErrDuplicateMemberValue(
        "value of {} member '{}' conflicts with previously declared member '{}' at {}");
constexpr ErrorDef<108, std::string_view, SourceSpan> ErrDuplicateResourcePropertyName(
    "multiple resource properties named '{}'; previous was at {}");
constexpr ErrorDef<109, std::string_view, std::string_view, SourceSpan, std::string_view>
    ErrDuplicateResourcePropertyNameCanonical(
        "resource property '{}' conflicts with property '{}' from {}; both are "
        "represented by the canonical form '{}'");
constexpr ErrorDef<110, flat::Name, std::string_view, std::string_view, flat::Name>
    ErrTypeMustBeResource(
        "'{}' may contain handles (due to member '{}'), so it must "
        "be declared with the `resource` modifier: `resource {} {}`");
constexpr ErrorDef<111, flat::Name, uint32_t, uint32_t> ErrInlineSizeExceedsLimit(
    "'{}' has an inline size of {} bytes, which exceeds the maximum allowed "
    "inline size of {} bytes");
// TODO(fxbug.dev/70399): As part of consolidating name resolution, these should
// be grouped into a single "expected foo but got bar" error, along with
// ErrExpectedValueButGotType.
constexpr ErrorDef<112> ErrOnlyClientEndsInServices("service members must be client_end:P");
constexpr ErrorDef<113, std::string_view, std::string_view, std::string_view, std::string_view>
    ErrMismatchedTransportInServices(
        "service member {} is over the {} transport, but member {} is over the {} transport. "
        "Multiple transports are not allowed.");
constexpr ErrorDef<114, types::Openness, flat::Name, types::Openness, flat::Name>
    ErrComposedProtocolTooOpen(
        "{} protocol '{}' cannot compose {} protocol '{}'; composed protocol may not be more open "
        "than composing protocol");
constexpr ErrorDef<115, types::Openness> ErrFlexibleTwoWayMethodRequiresOpenProtocol(
    "flexible two-way method may only be defined in an open protocol, not {}");
constexpr ErrorDef<116, std::string_view> ErrFlexibleOneWayMethodInClosedProtocol(
    "flexible {} may only be defined in an open or ajar protocol, not closed");
constexpr ErrorDef<117, std::string_view, std::string_view, const flat::Decl *>
    ErrHandleUsedInIncompatibleTransport(
        "handle of type {} may not be sent over transport {} used by {}");
constexpr ErrorDef<118, std::string_view, std::string_view, const flat::Decl *>
    ErrTransportEndUsedInIncompatibleTransport(
        "client_end / server_end of transport type {} may not be sent over transport {} used by "
        "{}");
constexpr ErrorDef<119, std::string_view> ErrEventErrorSyntaxDeprecated(
    "Event '{}' uses the error syntax. This is deprecated (see fxbug.dev/99924)");
constexpr ErrorDef<120, const flat::Attribute *> ErrInvalidAttributePlacement(
    "placement of attribute '{}' disallowed here");
constexpr ErrorDef<121, const flat::Attribute *> ErrDeprecatedAttribute(
    "attribute '{}' is deprecated");
constexpr ErrorDef<122, std::string_view, SourceSpan> ErrDuplicateAttribute(
    "duplicate attribute '{}'; previous was at {}");
constexpr ErrorDef<123, std::string_view, std::string_view, SourceSpan, std::string_view>
    ErrDuplicateAttributeCanonical(
        "attribute '{}' conflicts with attribute '{}' from {}; both are "
        "represented by the canonical form '{}'");
constexpr ErrorDef<124, const flat::AttributeArg *, const flat::Attribute *>
    ErrCanOnlyUseStringOrBool(
        "argument '{}' on user-defined attribute '{}' cannot be a numeric "
        "value; use a bool or string instead");
constexpr ErrorDef<125> ErrAttributeArgMustNotBeNamed(
    "attributes that take a single argument must not name that argument");
constexpr ErrorDef<126, std::string_view> ErrAttributeArgNotNamed(
    "attributes that take multiple arguments must name all of them explicitly, but '{}' was not");
constexpr ErrorDef<127, const flat::Attribute *, std::string_view> ErrMissingRequiredAttributeArg(
    "attribute '{}' is missing the required '{}' argument");
constexpr ErrorDef<128, const flat::Attribute *> ErrMissingRequiredAnonymousAttributeArg(
    "attribute '{}' is missing its required argument");
constexpr ErrorDef<129, const flat::Attribute *, std::string_view> ErrUnknownAttributeArg(
    "attribute '{}' does not support the '{}' argument");
constexpr ErrorDef<130, const flat::Attribute *, std::string_view, SourceSpan>
    ErrDuplicateAttributeArg(
        "attribute '{}' provides the '{}' argument multiple times; previous was at {}");
constexpr ErrorDef<131, const flat::Attribute *, std::string_view, std::string_view, SourceSpan,
                   std::string_view>
    ErrDuplicateAttributeArgCanonical(
        "attribute '{}' argument '{}' conflicts with argument '{}' from {}; both "
        "are represented by the canonical form '{}'");
constexpr ErrorDef<132, const flat::Attribute *> ErrAttributeDisallowsArgs(
    "attribute '{}' does not support arguments");
constexpr ErrorDef<133, std::string_view, const flat::Attribute *> ErrAttributeArgRequiresLiteral(
    "argument '{}' of attribute '{}' does not support referencing constants; "
    "please use a literal instead");
constexpr RetiredDef<134> ErrAttributeConstraintNotSatisfied;
constexpr ErrorDef<135, std::string_view> ErrInvalidDiscoverableName(
    "invalid @discoverable name '{}'; must follow the format 'the.library.name.TheProtocolName'");
constexpr RetiredDef<136> ErrTableCannotBeSimple;
constexpr RetiredDef<137> ErrUnionCannotBeSimple;
constexpr RetiredDef<138> ErrElementMustBeSimple;
constexpr RetiredDef<139> ErrTooManyBytes;
constexpr RetiredDef<140> ErrTooManyHandles;
constexpr ErrorDef<141> ErrInvalidErrorType(
    "invalid error type: must be int32, uint32 or an enum thereof");
constexpr ErrorDef<142, std::string_view, std::set<std::string_view>> ErrInvalidTransportType(
    "invalid transport type: got {} expected one of {}");
constexpr RetiredDef<143> ErrBoundIsTooBig;
constexpr RetiredDef<144> ErrUnableToParseBound;
constexpr WarningDef<145, std::string_view, std::string_view> WarnAttributeTypo(
    "suspect attribute with name '{}'; did you mean '{}'?");
constexpr ErrorDef<146> ErrInvalidGeneratedName("generated name must be a valid identifier");
constexpr ErrorDef<147> ErrAvailableMissingArguments(
    "at least one argument is required: 'added', 'deprecated', or 'removed'");
constexpr ErrorDef<148> ErrNoteWithoutDeprecation(
    "the argument 'note' cannot be used without 'deprecated'");
constexpr ErrorDef<149> ErrPlatformNotOnLibrary(
    "the argument 'platform' can only be used on the library's @available attribute");
constexpr ErrorDef<150> ErrLibraryAvailabilityMissingAdded(
    "missing 'added' argument on the library's @available attribute");
constexpr ErrorDef<151, std::vector<std::string_view>> ErrMissingLibraryAvailability(
    "to use the @available attribute here, you must also annotate the "
    "`library {};` declaration in one of the library's files");
constexpr ErrorDef<152, std::string_view> ErrInvalidPlatform(
    "invalid platform '{}'; must match the regex [a-z][a-z0-9]*");
constexpr ErrorDef<153, uint64_t> ErrInvalidVersion(
    "invalid version '{}'; must be an integer from 1 to 2^63-1 inclusive, or "
    "the special constant `HEAD`");
constexpr ErrorDef<154> ErrInvalidAvailabilityOrder(
    "invalid availability; must have added <= deprecated < removed");
constexpr ErrorDef<155, const flat::AttributeArg *, std::string_view, const flat::AttributeArg *,
                   std::string_view, SourceSpan, std::string_view, std::string_view,
                   std::string_view>
    ErrAvailabilityConflictsWithParent(
        "the argument {}={} conflicts with {}={} at {}; a child element "
        "cannot be {} {} its parent element is {}");
constexpr ErrorDef<156, flat::Name> ErrCannotBeOptional("{} cannot be optional");
constexpr ErrorDef<157, flat::Name> ErrMustBeAProtocol("{} must be a protocol");
constexpr ErrorDef<158, flat::Name> ErrCannotBoundTwice("{} cannot bound twice");
constexpr ErrorDef<159, flat::Name> ErrStructCannotBeOptional(
    "structs can no longer be marked optional; please use the new syntax, "
    "`box<{}>`");
constexpr ErrorDef<160, flat::Name> ErrCannotIndicateOptionalTwice(
    "{} is already optional, cannot indicate optionality twice");
constexpr ErrorDef<161, flat::Name> ErrMustHaveNonZeroSize("{} must have non-zero size");
constexpr ErrorDef<162, flat::Name, size_t, size_t> ErrWrongNumberOfLayoutParameters(
    "{} expected {} layout parameter(s), but got {}");
constexpr ErrorDef<163> ErrMultipleConstraintDefinitions(
    "cannot specify multiple constraint sets on a type");
constexpr ErrorDef<164, flat::Name, size_t, size_t> ErrTooManyConstraints(
    "{} expected at most {} constraints, but got {}");
constexpr ErrorDef<165> ErrExpectedType("expected type but got a literal or constant");
constexpr ErrorDef<166, flat::Name> ErrUnexpectedConstraint("{} failed to resolve constraint");
constexpr ErrorDef<167, flat::Name> ErrCannotConstrainTwice("{} cannot add additional constraint");
constexpr ErrorDef<168, flat::Name> ErrProtocolConstraintRequired(
    "{} requires a protocol as its first constraint");
// The same error as ErrCannotBeOptional, but with a more specific message since the
// optionality of boxes may be confusing
constexpr ErrorDef<169> ErrBoxCannotBeOptional(
    "cannot specify optionality for box, boxes are optional by default");
constexpr ErrorDef<170> ErrBoxedTypeCannotBeOptional(
    "no double optionality, boxes are already optional");
constexpr ErrorDef<171, flat::Name> ErrCannotBeBoxedShouldBeOptional(
    "type {} cannot be boxed, try using optional instead");
constexpr ErrorDef<172, flat::Name> ErrResourceMustBeUint32Derived("resource {} must be uint32");
constexpr ErrorDef<173, flat::Name> ErrResourceMissingSubtypeProperty(
    "resource {} expected to have the subtype property, but it was missing");
constexpr RetiredDef<174> ErrResourceMissingRightsProperty;
constexpr ErrorDef<175, flat::Name> ErrResourceSubtypePropertyMustReferToEnum(
    "the subtype property must be an enum, but wasn't in resource {}");
constexpr RetiredDef<176> ErrHandleSubtypeMustReferToResourceSubtype;
constexpr ErrorDef<177, flat::Name> ErrResourceRightsPropertyMustReferToBits(
    "the rights property must be a uint32 or a uint32-based bits, "
    "but wasn't defined as such in resource {}");
constexpr ErrorDef<178, std::vector<std::string_view>, std::vector<std::string_view>,
                   std::vector<std::string_view>>
    ErrUnusedImport("Library {} imports {} but does not use it. Either use {}, or remove import.");
constexpr ErrorDef<179, flat::Name> ErrNewTypeCannotHaveConstraint(
    "{} is a newtype, which cannot carry constraints");
constexpr ErrorDef<180, flat::Name> ErrExperimentalZxCTypesDisallowed(
    "{} is an experimental type that must be enabled by with `--experimental zx_c_types`");
constexpr ErrorDef<181> ErrReferenceInLibraryAttribute(
    "attributes on the 'library' declaration do not support referencing constants");
constexpr ErrorDef<182, const flat::AttributeArg *> ErrLegacyWithoutRemoval(
    "the argument '{}' is not allowed on an element that is never removed");
constexpr ErrorDef<183, const flat::AttributeArg *, std::string_view, const flat::AttributeArg *,
                   std::string_view, SourceSpan>
    ErrLegacyConflictsWithParent(
        "the argument {}={} conflicts with {}={} at {}; a child element "
        "cannot be added back at LEGACY if its parent is removed");
constexpr ErrorDef<184, std::string_view> ErrUnexpectedControlCharacter(
    "unexpected control character in string literal; use the Unicode escape `\\u{{}}` instead");
constexpr ErrorDef<185> ErrUnicodeEscapeMissingBraces(
    "Unicode escape must use braces, like `\\u{a}` for U+000A");
constexpr ErrorDef<186> ErrUnicodeEscapeUnterminated(
    "Unicode escape is missing a closing brace '}'");
constexpr ErrorDef<187> ErrUnicodeEscapeEmpty("Unicode escape must have at least 1 hex digit");
constexpr ErrorDef<188> ErrUnicodeEscapeTooLong("Unicode escape must have at most 6 hex digits");
constexpr ErrorDef<189, std::string_view> ErrUnicodeEscapeTooLarge(
    "invalid Unicode code point '{}'; maximum is 10FFFF");
constexpr RetiredDef<190> ErrSimpleProtocolMustBeClosed;
constexpr ErrorDef<191, std::string_view> ErrMethodMustDefineStrictness(
    "Method {} must explicitly specify strict or flexible. (The default is changing "
    "from strict to flexible, and explicit modifiers are mandatory during the migration.)",
    {.fixable = Fixable::Kind::kProtocolModifier});
constexpr ErrorDef<192, std::string_view> ErrProtocolMustDefineOpenness(
    "Protocol {} must explicitly specify open, ajar, or closed. (The default is changing "
    "from closed to open, and explicit modifiers are mandatory during the migration.)",
    {.fixable = Fixable::Kind::kProtocolModifier});
constexpr ErrorDef<193, flat::Name> ErrCannotBeBoxedNorOptional("type {} cannot be boxed");
constexpr ErrorDef<194, std::string_view> ErrEmptyPayloadStructsWhenResultUnion(
    "method '{}' cannot have an empty struct as a payload, prefer omitting the payload"
    "altogether",
    {.fixable = Fixable::Kind::kEmptyStructResponse});
constexpr ErrorDef<195> ErrExperimentalOverflowingAttributeMissingExperimentalFlag(
    "the @experimental_overflowing attribute can only be used if the"
    "`--experiment allow_overflowing` flag has been enabled for fidlc");
constexpr ErrorDef<196> ErrExperimentalOverflowingIncorrectUsage(
    "the @experimental_overflowing attribute must have at least one of the `request` or `response`"
    "arguments set to true");
constexpr ErrorDef<197> ErrOverlayMustBeStrict("overlays must be strict", {.documented = false});
constexpr ErrorDef<198> ErrOverlayMustBeValue("overlays must be value (not resource) types",
                                              {.documented = false});
constexpr ErrorDef<199> ErrOverlayMemberMustBeValue("overlays may not contain resource members",
                                                    {.documented = false});
constexpr ErrorDef<200> ErrOverlayMustNotContainReserved(
    "overlays may not contain reserved members", {.documented = false});
constexpr ErrorDef<201, std::vector<std::string_view>, Platform, Platform>
    ErrPlatformVersionNotSelected(
        "library '{}' belongs to platform '{}', but no version was selected for it; "
        "please choose a version N by passing `--available {}:N`");

// To add a new error:
//
// 1. Define it above using the last error's number + 1
// 2. Add it to the end of kAllDiagnosticDefs below
// 3. Run $FUCHSIA_DIR/tools/fidl/scripts/add_errcat_entry.py

static constexpr const DiagnosticDef *kAllDiagnosticDefs[] = {
    /* fi-0001 */ &ErrInvalidCharacter,
    /* fi-0002 */ &ErrUnexpectedLineBreak,
    /* fi-0003 */ &ErrInvalidEscapeSequence,
    /* fi-0004 */ &ErrInvalidHexDigit,
    /* fi-0005 */ &ErrInvalidOctDigit,
    /* fi-0006 */ &ErrExpectedDeclaration,
    /* fi-0007 */ &ErrUnexpectedToken,
    /* fi-0008 */ &ErrUnexpectedTokenOfKind,
    /* fi-0009 */ &ErrUnexpectedIdentifier,
    /* fi-0010 */ &ErrInvalidIdentifier,
    /* fi-0011 */ &ErrInvalidLibraryNameComponent,
    /* fi-0012 */ &ErrInvalidLayoutClass,
    /* fi-0013 */ &ErrInvalidWrappedType,
    /* fi-0014 */ &ErrAttributeWithEmptyParens,
    /* fi-0015 */ &ErrAttributeArgsMustAllBeNamed,
    /* fi-0016 */ &ErrMissingOrdinalBeforeMember,
    /* fi-0017 */ &ErrOrdinalOutOfBound,
    /* fi-0018 */ &ErrOrdinalsMustStartAtOne,
    /* fi-0019 */ &ErrMustHaveOneMember,
    /* fi-0020 */ &ErrInvalidProtocolMember,
    /* fi-0021 */ &ErrExpectedProtocolMember,
    /* fi-0022 */ &ErrCannotAttachAttributeToIdentifier,
    /* fi-0023 */ &ErrRedundantAttributePlacement,
    /* fi-0024 */ &ErrDocCommentOnParameters,
    /* fi-0025 */ &ErrLibraryImportsMustBeGroupedAtTopOfFile,
    /* fi-0026 */ &WarnCommentWithinDocCommentBlock,
    /* fi-0027 */ &WarnBlankLinesWithinDocCommentBlock,
    /* fi-0028 */ &WarnDocCommentMustBeFollowedByDeclaration,
    /* fi-0029 */ &ErrMustHaveOneProperty,
    /* fi-0030 */ &ErrCannotSpecifyModifier,
    /* fi-0031 */ &ErrCannotSpecifySubtype,
    /* fi-0032 */ &ErrDuplicateModifier,
    /* fi-0033 */ &ErrConflictingModifier,
    /* fi-0034 */ &ErrNameCollision,
    /* fi-0035 */ &ErrNameCollisionCanonical,
    /* fi-0036 */ &ErrNameOverlap,
    /* fi-0037 */ &ErrNameOverlapCanonical,
    /* fi-0038 */ &ErrDeclNameConflictsWithLibraryImport,
    /* fi-0039 */ &ErrDeclNameConflictsWithLibraryImportCanonical,
    /* fi-0040 */ &ErrFilesDisagreeOnLibraryName,
    /* fi-0041 */ &ErrMultipleLibrariesWithSameName,
    /* fi-0042 */ &ErrDuplicateLibraryImport,
    /* fi-0043 */ &ErrConflictingLibraryImport,
    /* fi-0044 */ &ErrConflictingLibraryImportAlias,
    /* fi-0045 */ &ErrAttributesNotAllowedOnLibraryImport,
    /* fi-0046 */ &ErrUnknownLibrary,
    /* fi-0047 */ &ErrProtocolComposedMultipleTimes,
    /* fi-0048 */ &ErrOptionalTableMember,
    /* fi-0049 */ &ErrOptionalUnionMember,
    /* fi-0050 */ &ErrDeprecatedStructDefaults,
    /* fi-0051 */ &ErrUnknownDependentLibrary,
    /* fi-0052 */ &ErrNameNotFound,
    /* fi-0053 */ &ErrCannotReferToMember,
    /* fi-0054 */ &ErrMemberNotFound,
    /* fi-0055 */ &ErrInvalidReferenceToDeprecated,
    /* fi-0056 */ &ErrInvalidReferenceToDeprecatedOtherPlatform,
    /* fi-0057 */ &ErrIncludeCycle,
    /* fi-0058 */ &ErrAnonymousNameReference,
    /* fi-0059 */ &ErrInvalidConstantType,
    /* fi-0060 */ &ErrCannotResolveConstantValue,
    /* fi-0061 */ &ErrOrOperatorOnNonPrimitiveValue,
    /* fi-0062 */ &ErrNewTypesNotAllowed,
    /* fi-0063 */ &ErrExpectedValueButGotType,
    /* fi-0064 */ &ErrMismatchedNameTypeAssignment,
    /* fi-0065 */ &ErrTypeCannotBeConvertedToType,
    /* fi-0066 */ &ErrConstantOverflowsType,
    /* fi-0067 */ &ErrBitsMemberMustBePowerOfTwo,
    /* fi-0068 */ &ErrFlexibleEnumMemberWithMaxValue,
    /* fi-0069 */ &ErrBitsTypeMustBeUnsignedIntegralPrimitive,
    /* fi-0070 */ &ErrEnumTypeMustBeIntegralPrimitive,
    /* fi-0071 */ &ErrUnknownAttributeOnStrictEnumMember,
    /* fi-0072 */ &ErrUnknownAttributeOnMultipleEnumMembers,
    /* fi-0073 */ &ErrComposingNonProtocol,
    /* fi-0074 */ &ErrInvalidMethodPayloadLayoutClass,
    /* fi-0075 */ &ErrInvalidMethodPayloadType,
    /* fi-0076 */ &ErrResponsesWithErrorsMustNotBeEmpty,
    /* fi-0077 */ &ErrEmptyPayloadStructs,
    /* fi-0078 */ &ErrDuplicateMethodName,
    /* fi-0079 */ &ErrDuplicateMethodNameCanonical,
    /* fi-0080 */ &ErrGeneratedZeroValueOrdinal,
    /* fi-0081 */ &ErrDuplicateMethodOrdinal,
    /* fi-0082 */ &ErrInvalidSelectorValue,
    /* fi-0083 */ &ErrFuchsiaIoExplicitOrdinals,
    /* fi-0084 */ &ErrPayloadStructHasDefaultMembers,
    /* fi-0085 */ &ErrDuplicateServiceMemberName,
    /* fi-0086 */ &ErrStrictUnionMustHaveNonReservedMember,
    /* fi-0087 */ &ErrDuplicateServiceMemberNameCanonical,
    /* fi-0088 */ &ErrOptionalServiceMember,
    /* fi-0089 */ &ErrDuplicateStructMemberName,
    /* fi-0090 */ &ErrDuplicateStructMemberNameCanonical,
    /* fi-0091 */ &ErrInvalidStructMemberType,
    /* fi-0092 */ &ErrTooManyTableOrdinals,
    /* fi-0093 */ &ErrMaxOrdinalNotTable,
    /* fi-0094 */ &ErrDuplicateTableFieldOrdinal,
    /* fi-0095 */ &ErrDuplicateTableFieldName,
    /* fi-0096 */ &ErrDuplicateTableFieldNameCanonical,
    /* fi-0097 */ &ErrDuplicateUnionMemberOrdinal,
    /* fi-0098 */ &ErrDuplicateUnionMemberName,
    /* fi-0099 */ &ErrDuplicateUnionMemberNameCanonical,
    /* fi-0100 */ &ErrNonDenseOrdinal,
    /* fi-0101 */ &ErrCouldNotResolveSizeBound,
    /* fi-0102 */ &ErrCouldNotResolveMember,
    /* fi-0103 */ &ErrCouldNotResolveMemberDefault,
    /* fi-0104 */ &ErrCouldNotResolveAttributeArg,
    /* fi-0105 */ &ErrDuplicateMemberName,
    /* fi-0106 */ &ErrDuplicateMemberNameCanonical,
    /* fi-0107 */ &ErrDuplicateMemberValue,
    /* fi-0108 */ &ErrDuplicateResourcePropertyName,
    /* fi-0109 */ &ErrDuplicateResourcePropertyNameCanonical,
    /* fi-0110 */ &ErrTypeMustBeResource,
    /* fi-0111 */ &ErrInlineSizeExceedsLimit,
    /* fi-0112 */ &ErrOnlyClientEndsInServices,
    /* fi-0113 */ &ErrMismatchedTransportInServices,
    /* fi-0114 */ &ErrComposedProtocolTooOpen,
    /* fi-0115 */ &ErrFlexibleTwoWayMethodRequiresOpenProtocol,
    /* fi-0116 */ &ErrFlexibleOneWayMethodInClosedProtocol,
    /* fi-0117 */ &ErrHandleUsedInIncompatibleTransport,
    /* fi-0118 */ &ErrTransportEndUsedInIncompatibleTransport,
    /* fi-0119 */ &ErrEventErrorSyntaxDeprecated,
    /* fi-0120 */ &ErrInvalidAttributePlacement,
    /* fi-0121 */ &ErrDeprecatedAttribute,
    /* fi-0122 */ &ErrDuplicateAttribute,
    /* fi-0123 */ &ErrDuplicateAttributeCanonical,
    /* fi-0124 */ &ErrCanOnlyUseStringOrBool,
    /* fi-0125 */ &ErrAttributeArgMustNotBeNamed,
    /* fi-0126 */ &ErrAttributeArgNotNamed,
    /* fi-0127 */ &ErrMissingRequiredAttributeArg,
    /* fi-0128 */ &ErrMissingRequiredAnonymousAttributeArg,
    /* fi-0129 */ &ErrUnknownAttributeArg,
    /* fi-0130 */ &ErrDuplicateAttributeArg,
    /* fi-0131 */ &ErrDuplicateAttributeArgCanonical,
    /* fi-0132 */ &ErrAttributeDisallowsArgs,
    /* fi-0133 */ &ErrAttributeArgRequiresLiteral,
    /* fi-0134 */ &ErrAttributeConstraintNotSatisfied,
    /* fi-0135 */ &ErrInvalidDiscoverableName,
    /* fi-0136 */ &ErrTableCannotBeSimple,
    /* fi-0137 */ &ErrUnionCannotBeSimple,
    /* fi-0138 */ &ErrElementMustBeSimple,
    /* fi-0139 */ &ErrTooManyBytes,
    /* fi-0140 */ &ErrTooManyHandles,
    /* fi-0141 */ &ErrInvalidErrorType,
    /* fi-0142 */ &ErrInvalidTransportType,
    /* fi-0143 */ &ErrBoundIsTooBig,
    /* fi-0144 */ &ErrUnableToParseBound,
    /* fi-0145 */ &WarnAttributeTypo,
    /* fi-0146 */ &ErrInvalidGeneratedName,
    /* fi-0147 */ &ErrAvailableMissingArguments,
    /* fi-0148 */ &ErrNoteWithoutDeprecation,
    /* fi-0149 */ &ErrPlatformNotOnLibrary,
    /* fi-0150 */ &ErrLibraryAvailabilityMissingAdded,
    /* fi-0151 */ &ErrMissingLibraryAvailability,
    /* fi-0152 */ &ErrInvalidPlatform,
    /* fi-0153 */ &ErrInvalidVersion,
    /* fi-0154 */ &ErrInvalidAvailabilityOrder,
    /* fi-0155 */ &ErrAvailabilityConflictsWithParent,
    /* fi-0156 */ &ErrCannotBeOptional,
    /* fi-0157 */ &ErrMustBeAProtocol,
    /* fi-0158 */ &ErrCannotBoundTwice,
    /* fi-0159 */ &ErrStructCannotBeOptional,
    /* fi-0160 */ &ErrCannotIndicateOptionalTwice,
    /* fi-0161 */ &ErrMustHaveNonZeroSize,
    /* fi-0162 */ &ErrWrongNumberOfLayoutParameters,
    /* fi-0163 */ &ErrMultipleConstraintDefinitions,
    /* fi-0164 */ &ErrTooManyConstraints,
    /* fi-0165 */ &ErrExpectedType,
    /* fi-0166 */ &ErrUnexpectedConstraint,
    /* fi-0167 */ &ErrCannotConstrainTwice,
    /* fi-0168 */ &ErrProtocolConstraintRequired,
    /* fi-0169 */ &ErrBoxCannotBeOptional,
    /* fi-0170 */ &ErrBoxedTypeCannotBeOptional,
    /* fi-0171 */ &ErrCannotBeBoxedShouldBeOptional,
    /* fi-0172 */ &ErrResourceMustBeUint32Derived,
    /* fi-0173 */ &ErrResourceMissingSubtypeProperty,
    /* fi-0174 */ &ErrResourceMissingRightsProperty,
    /* fi-0175 */ &ErrResourceSubtypePropertyMustReferToEnum,
    /* fi-0176 */ &ErrHandleSubtypeMustReferToResourceSubtype,
    /* fi-0177 */ &ErrResourceRightsPropertyMustReferToBits,
    /* fi-0178 */ &ErrUnusedImport,
    /* fi-0179 */ &ErrNewTypeCannotHaveConstraint,
    /* fi-0180 */ &ErrExperimentalZxCTypesDisallowed,
    /* fi-0181 */ &ErrReferenceInLibraryAttribute,
    /* fi-0182 */ &ErrLegacyWithoutRemoval,
    /* fi-0183 */ &ErrLegacyConflictsWithParent,
    /* fi-0184 */ &ErrUnexpectedControlCharacter,
    /* fi-0185 */ &ErrUnicodeEscapeMissingBraces,
    /* fi-0186 */ &ErrUnicodeEscapeUnterminated,
    /* fi-0187 */ &ErrUnicodeEscapeEmpty,
    /* fi-0188 */ &ErrUnicodeEscapeTooLong,
    /* fi-0189 */ &ErrUnicodeEscapeTooLarge,
    /* fi-0190 */ &ErrSimpleProtocolMustBeClosed,
    /* fi-0191 */ &ErrMethodMustDefineStrictness,
    /* fi-0192 */ &ErrProtocolMustDefineOpenness,
    /* fi-0193 */ &ErrCannotBeBoxedNorOptional,
    /* fi-0194 */ &ErrEmptyPayloadStructsWhenResultUnion,
    /* fi-0195 */ &ErrExperimentalOverflowingAttributeMissingExperimentalFlag,
    /* fi-0196 */ &ErrExperimentalOverflowingIncorrectUsage,
    /* fi-0197 */ &ErrOverlayMustBeStrict,
    /* fi-0198 */ &ErrOverlayMustBeValue,
    /* fi-0199 */ &ErrOverlayMemberMustBeValue,
    /* fi-0200 */ &ErrOverlayMustNotContainReserved,
    /* fi-0201 */ &ErrPlatformVersionNotSelected,
};

// In reporter.h we assert that reported error IDs are <= kNumDiagnosticDefs.
// This combined with the assert below ensures kAllDiagnosticDefs is complete.
static constexpr size_t kNumDiagnosticDefs =
    sizeof(kAllDiagnosticDefs) / sizeof(kAllDiagnosticDefs[0]);

// If anything is missing or out of order in kAllDiagnosticDefs, this assertion
// will fail and the message will include the position where it happened.
static_assert([] {
  ErrorId expected = 1;
  for (auto def : kAllDiagnosticDefs) {
    if (def->id != expected) {
      return expected;
    }
    expected++;
  }
  return 0u;
}() == 0);

}  // namespace fidl

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_DIAGNOSTICS_H_
