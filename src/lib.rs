//! `makelint` — lints Makefiles for missing `.PHONY` declarations,
//! tab-vs-space recipe indentation errors, and undefined variable
//! references. See the README for scope.

pub mod parser;
pub mod rules;
