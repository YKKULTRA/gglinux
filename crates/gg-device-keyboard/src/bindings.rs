// SPDX-License-Identifier: GPL-3.0-or-later
//! Key bindings: what each key does on its own, while Fn is held and past its second
//! actuation point, the key that holds Fn and the keys lit while it's held.
//!
//! From Apex Control's protocol reference (github.com/Rikearon/apex-control,
//! `docs/PROTOCOL.md` at `f7db8c5`, CC BY 4.0), "Key bindings": `36` writes a layer, `B6`
//! reads it back, `35` picks the Fn key, `3C` sets the Fn-lit keys, and `38 61 44 02` is the
//! second actuation point's table. All of them write the keyboard's RAM only. The
//! allowlist lets them out only with the bindings family on ([`crate::allow`]).

use crate::Keyboard;

impl Keyboard {
    // Unit kb-bindings adds read_bindings/write_bindings here.
}
