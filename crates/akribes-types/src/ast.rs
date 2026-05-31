//! AST shapes that travel over the wire.
//!
//! This is the SDK-facing slice of `akribes_core::ast`: just the types
//! that consumers need to interpret engine events ([`Span`], [`TypeRef`],
//! [`TypeField`], [`ActorHint`], [`FieldConstraint`] and the associated
//! sentinel constants). The full `akribes_core::ast` module also defines
//! `Stmt`, `Expr`, `Program`, and the rest of the language AST, which
//! stays in core because the parser/analyzer/compiler own those shapes.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    pub line: usize,
    pub col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeField {
    pub name: String,
    pub ty: TypeRef,
    pub docs: Option<String>,
    pub span: Span,
    /// Field-level validation constraints declared via `matches /re/`,
    /// `at_least_items 3`, prose `"..."` lines, etc. Attached by the parser
    /// to the most recent field whose `span.col` matches the constraint's
    /// column (see `parser.rs` constraint attachment rules). `#[serde(default)]`
    /// so ASTs serialized before constraints existed still deserialize.
    #[serde(default)]
    pub constraints: Vec<FieldConstraint>,
}

/// A single field-level validation constraint attached to a `type` field.
/// Parsed from the Constraint Mini-Language (see
/// `docs/superpowers/specs/2026-04-18-epa-constraint-language-design.md`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldConstraint {
    /// A structured (Tier-1) constraint like `matches /^[A-Z0-9]+$/` or
    /// `at_least_items 3`. `phrase` is the canonical phrase name (e.g.
    /// `"matches"`) — the key the `ConstraintRegistry` uses to look up the
    /// handler. `args` is a handler-specific JSON payload (for `matches` this
    /// is `{"pattern": "<regex>"}`).
    Tier1 {
        phrase: String,
        args: serde_json::Value,
        span: Span,
    },
    /// An unrecognized Tier-2 prose rule, e.g.
    /// `"must be a valid ticker symbol"`. Rendered verbatim into the prompt
    /// in Tier-1 prose form; never enforced at runtime.
    ProseRule { text: String, span: Span },
    /// A `validate_with: <ident>` custom-validator hook. `name` is the
    /// validator's canonical identifier — resolved against the
    /// `validation::validator_registry::VALIDATORS` registry at
    /// analysis time (emits `AKRIBES-E-VALIDATE-WITH-UNKNOWN` on misses) and
    /// dispatched at task-end (failures surface as
    /// `AKRIBES-E-VALIDATE-WITH-FAIL` corrective retries).
    ValidateWith { name: String, span: Span },
}

impl FieldConstraint {
    pub fn span(&self) -> &Span {
        match self {
            FieldConstraint::Tier1 { span, .. } => span,
            FieldConstraint::ProseRule { span, .. } => span,
            FieldConstraint::ValidateWith { span, .. } => span,
        }
    }
}

/// Upper cap on discriminated-union arms. 8 is the tightest reliable
/// value across Anthropic tool-use + Gemini `responseSchema` under
/// preliminary testing. The analyzer raises `AKRIBES-E-UNION-009` when an
/// arm list exceeds this.
pub const MAX_UNION_ARMS: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeRef {
    pub name: String,
    pub inner: Option<Box<TypeRef>>,
    /// Populated only for string-literal union types; in that case `name` is
    /// the sentinel `"choice"` and `choices` holds the variant strings in
    /// declaration order. `None` for every other type. Variant validation
    /// (non-empty, unique, ≥2) is the analyzer's job, not the parser's.
    pub choices: Option<Vec<String>>,
    /// Populated only for discriminated-union types (general `A | B | ...`
    /// including the binary `T | Unable` special case). When `Some`, `name`
    /// is the sentinel `"variant_union"` (mirroring `"choice"`), both
    /// `inner` and `choices` are `None`, and `variants` holds every arm in
    /// source order with length in `[2, MAX_UNION_ARMS]`. The analyzer
    /// enforces arm-record-only, ≤8, no duplicates, and
    /// return-position-only usage in v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<TypeRef>>,
}

/// Sentinel `TypeRef.name` for a discriminated union (`A | B | ...`).
pub const VARIANT_UNION_SENTINEL: &str = "variant_union";

/// Sentinel `TypeRef.name` for an optional type (`T?`). The `inner` field
/// holds the wrapped `T`. `none` is assignable to any optional type;
/// `T?` is NOT assignable to `T` without an explicit `?? default` unwrap
/// or a pattern-match on `none`. (D2)
pub const OPTIONAL_SENTINEL: &str = "optional";

impl TypeRef {
    /// Build a primitive or named type reference (no generic inner, no
    /// choice variants). Use this in preference to a struct literal so the
    /// `choices` field stays consistently `None` at non-choice sites.
    pub fn primitive(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            inner: None,
            choices: None,
            variants: None,
        }
    }

    /// Build an optional type `T?` wrapping `inner` (D2). Idempotent:
    /// applying this to a type that is already optional returns the same
    /// shape (no double-wrap), matching most languages' `T??` collapse.
    pub fn optional(inner: TypeRef) -> Self {
        if inner.is_optional() {
            return inner;
        }
        Self {
            name: OPTIONAL_SENTINEL.to_string(),
            inner: Some(Box::new(inner)),
            choices: None,
            variants: None,
        }
    }

    /// `true` iff this `TypeRef` is an `Optional[T]` sentinel.
    pub fn is_optional(&self) -> bool {
        self.name == OPTIONAL_SENTINEL && self.inner.is_some()
    }

    /// Borrow the wrapped `T` from an `Optional[T]`; `None` for non-optional.
    pub fn optional_inner(&self) -> Option<&TypeRef> {
        if self.is_optional() {
            self.inner.as_deref()
        } else {
            None
        }
    }

    /// Build a discriminated union from an ordered arm list. Grammar
    /// guarantees `arms.len() >= 2`; arm-count caps are analyzer-enforced
    /// (`AKRIBES-E-UNION-009`) so oversized unions reach the analyzer as
    /// parsed ASTs instead of panicking in the parser. The binary
    /// `T | Unable` case is just `variant_union(vec![T, Unable])`.
    pub fn variant_union(arms: Vec<TypeRef>) -> Self {
        debug_assert!(arms.len() >= 2, "variant union requires >= 2 arms");
        Self {
            name: VARIANT_UNION_SENTINEL.to_string(),
            inner: None,
            choices: None,
            variants: Some(arms),
        }
    }

    /// Build a binary union `success | Unable`. Kept as a named constructor
    /// because every #157 call site uses it; internally delegates to
    /// [`variant_union`] with `[success, Unable]` in canonical source
    /// order.
    pub fn union_with_unable(success: TypeRef) -> Self {
        Self::variant_union(vec![success, TypeRef::primitive("Unable")])
    }

    /// Return `true` iff this `TypeRef` is a discriminated-union sentinel
    /// (any arm count).
    pub fn is_variant_union(&self) -> bool {
        self.name == VARIANT_UNION_SENTINEL && self.variants.is_some()
    }

    /// Slice over the declared arms in source order (or `None` for
    /// non-union types).
    pub fn union_arms(&self) -> Option<&[TypeRef]> {
        self.variants.as_deref()
    }

    /// Return `true` iff this `TypeRef` is a binary union whose two arms
    /// are exactly one non-Unable record and one `Unable`. Used by every
    /// #157 call site that gates on "this is a T | Unable return type" —
    /// kept for backwards compatibility and cheap pattern-matching.
    pub fn is_union_with_unable(&self) -> bool {
        match self.variants.as_deref() {
            Some(arms) if arms.len() == 2 => {
                (arms[0].name == "Unable") ^ (arms[1].name == "Unable")
            }
            _ => false,
        }
    }

    /// Return the non-Unable branch of a binary `T | Unable`, or `None` if
    /// this is not exactly such a union. N-ary unions and unions without
    /// an `Unable` arm return `None` — callers that need the general arm
    /// list should use [`union_arms`].
    pub fn unwrap_union_success(&self) -> Option<&TypeRef> {
        match self.variants.as_deref() {
            Some(arms) if arms.len() == 2 => {
                if arms[0].name == "Unable" && arms[1].name != "Unable" {
                    Some(&arms[1])
                } else if arms[1].name == "Unable" && arms[0].name != "Unable" {
                    Some(&arms[0])
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Return the declared success arm of any discriminated union — the
    /// first arm in source order. Used for retry gating and
    /// `on <variant> default` type-checking. Returns `None` for non-union
    /// types.
    pub fn union_success_arm(&self) -> Option<&TypeRef> {
        self.variants.as_deref().and_then(|arms| arms.first())
    }

    /// Render a `TypeRef` as a source-level fragment for error messages,
    /// LSP labels, and user-facing diagnostics. Union types render as
    /// `A | B | ...`; choice types render as `"a" | "b" | ...`; generics
    /// render as `list[str]`; primitives render as their `name`.
    pub fn display(&self) -> String {
        // D2: `Optional[T]` renders as `T?` at the source level, mirroring
        // the postfix syntax authors typed.
        if let Some(inner) = self.optional_inner() {
            return format!("{}?", inner.display());
        }
        if let Some(arms) = &self.variants {
            return arms
                .iter()
                .map(|a| a.display())
                .collect::<Vec<_>>()
                .join(" | ");
        }
        if let Some(choices) = &self.choices {
            choices
                .iter()
                .map(|c| format!("\"{}\"", c))
                .collect::<Vec<_>>()
                .join(" | ")
        } else if let Some(inner) = &self.inner {
            format!("{}[{}]", self.name, inner.display())
        } else {
            self.name.clone()
        }
    }
}

impl fmt::Display for TypeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ActorHint {
    Human,
    Any,
    Client(String),
}
