//! Type signatures for the stdlib typed-error enums.
//!
//! Phase 0 of the stdlib error redesign (see
//! `docs/proposals/stdlib-errors.md`): register the six per-module error
//! enums as ordinary silt types so user code can construct and
//! pattern-match them in its own wrappers. No stdlib function signatures
//! change in this phase — the enums simply become available.
//!
//! Each variant name is module-prefixed (`IoNotFound`, `JsonSyntax`,
//! etc.) to avoid silt's one-to-one `variant_to_enum` collision, which
//! prevents two enums from sharing a variant name. This is deliberate
//! and final.

use super::super::*;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    register_enum(
        checker,
        env,
        "IoError",
        &[
            ("IoNotFound", &[Type::String]),
            ("IoPermissionDenied", &[Type::String]),
            ("IoAlreadyExists", &[Type::String]),
            ("IoInvalidInput", &[Type::String]),
            ("IoInterrupted", &[]),
            ("IoUnexpectedEof", &[]),
            ("IoWriteZero", &[]),
            ("IoUnknown", &[Type::String]),
        ],
    );

    register_enum(
        checker,
        env,
        "JsonError",
        &[
            ("JsonSyntax", &[Type::String, Type::Int]),
            ("JsonTypeMismatch", &[Type::String, Type::String]),
            ("JsonMissingField", &[Type::String]),
            ("JsonUnknown", &[Type::String]),
        ],
    );

    register_enum(
        checker,
        env,
        "TomlError",
        &[
            ("TomlSyntax", &[Type::String, Type::Int]),
            ("TomlTypeMismatch", &[Type::String, Type::String]),
            ("TomlMissingField", &[Type::String]),
            ("TomlUnknown", &[Type::String]),
        ],
    );

    register_enum(
        checker,
        env,
        "ParseError",
        &[
            ("ParseEmpty", &[]),
            ("ParseInvalidDigit", &[Type::Int]),
            ("ParseOverflow", &[]),
            ("ParseUnderflow", &[]),
        ],
    );

    register_enum(
        checker,
        env,
        "HttpError",
        &[
            ("HttpConnect", &[Type::String]),
            ("HttpTls", &[Type::String]),
            ("HttpTimeout", &[]),
            ("HttpInvalidUrl", &[Type::String]),
            ("HttpInvalidResponse", &[Type::String]),
            ("HttpClosedEarly", &[]),
            ("HttpStatusCode", &[Type::Int, Type::String]),
            ("HttpUnknown", &[Type::String]),
        ],
    );

    register_enum(
        checker,
        env,
        "RegexError",
        &[
            ("RegexInvalidPattern", &[Type::String, Type::Int]),
            ("RegexTooBig", &[]),
        ],
    );
}

/// Register a concrete (no type parameters) builtin enum + its variants.
fn register_enum(
    checker: &mut TypeChecker,
    env: &mut TypeEnv,
    enum_name: &'static str,
    variants: &[(&'static str, &[Type])],
) {
    let enum_sym = intern(enum_name);
    let result_ty = Type::Generic(enum_sym, vec![]);

    checker.enums.insert(
        enum_sym,
        EnumInfo {
            _name: enum_sym,
            params: vec![],
            param_var_ids: vec![],
            variants: variants
                .iter()
                .map(|(name, fields)| VariantInfo {
                    name: intern(name),
                    field_types: fields.to_vec(),
                })
                .collect(),
        },
    );

    for (variant_name, fields) in variants {
        let variant_sym = intern(variant_name);
        checker.variant_to_enum.insert(variant_sym, enum_sym);
        let scheme = if fields.is_empty() {
            // Nullary: register as a value of the enum type.
            Scheme::mono(result_ty.clone())
        } else {
            // N-ary: register as a constructor function.
            Scheme::mono(Type::Fun(fields.to_vec(), Box::new(result_ty.clone())))
        };
        env.define(variant_sym, scheme);
    }
}
