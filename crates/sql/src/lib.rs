//! SQL lexer and parser, hand-written.
//!
//! No `sqlparser-rs`. The point of the project is to write this from scratch.
//! Target dialect: a meaningful subset — SELECT, INSERT, UPDATE, DELETE,
//! CREATE TABLE, JOIN, WHERE, GROUP BY, ORDER BY, LIMIT.

#![forbid(unsafe_code)]
