//! Test suite: the differential oracle. Every program that checks
//! clean and touches nothing outside its VM is compiled, verified and
//! run twice, and the runs must agree; none may end in a
//! `type_confusion` error, an internal error or a panic (`oracle.rs`).
//!
//! The inputs come in classes, one module each. A finding that is known
//! and reported has a line in `skip.txt`; the suite fails on a finding
//! without one and on a line without its finding (`sweep.rs`).

mod corpora;
mod generated;
mod goldens;
mod limits;
mod oracle;
mod selfcheck;
mod sweep;
