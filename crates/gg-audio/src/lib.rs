//! Sonar-style audio for gglinux, on PipeWire.
//!
//! PipeWire hosts the graph — four virtual output channels declared in a
//! drop-in config ([`config`]) — and this crate controls it with PipeWire's own
//! tools (`pw-dump`, `pw-metadata`, `wpctl`) through [`tools::PwTools`]. Every
//! action is a command a person could type, and the logic is tested against a
//! fake.

pub mod config;
pub mod mix;
pub mod rules;
pub mod tools;
