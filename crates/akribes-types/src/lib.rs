//! Wire-level types shared by the Akribes SDK and core.
//!
//! This crate holds the data shapes that travel between the Akribes server
//! and its clients: [`event::EngineEvent`], [`value::Value`], the slice of
//! the AST surfaced by SDKs ([`ast::TypeField`], [`ast::TypeRef`],
//! [`ast::Span`], [`ast::ActorHint`]), and the error envelope
//! ([`error::ErrorCode`], [`error::ErrorKind`], [`error::ErrorSource`],
//! [`error::ErrorDetail`]).
//!
//! Splitting these out of `akribes-core` lets `akribes-sdk` (and other
//! downstream consumers like puto) depend on a thin, dependency-light crate
//! without pulling in the parser, analyzer, compiler, engine, or provider
//! stack. `akribes-core` re-exports the moved types so existing
//! `use akribes_core::event::EngineEvent` paths keep compiling.

pub mod ast;
pub mod error;
pub mod event;
pub mod value;
