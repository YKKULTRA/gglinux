// SPDX-License-Identifier: GPL-3.0-or-later
//! The five onboard profile slots in the keyboard's flash: what it loads when it powers up.
//!
//! From Apex Control's protocol reference (github.com/Rikearon/apex-control,
//! `docs/PROTOCOL.md` at `f7db8c5`, CC BY 4.0), "Onboard profiles & the flash filesystem":
//! a slot is the file `0x80 + slot`, a 12 324-byte image (a 12 320-byte body and its
//! STM32 CRC-32); `83 01` reads it in chunks, `02 01` erases it, `03 01` writes it in
//! 500-byte chunks, `B1` has the keyboard check it and `B2` makes the keyboard run it. The
//! allowlist lets them out only with the onboard family on, and only for those five files
//! ([`crate::allow`]).

use crate::Keyboard;

impl Keyboard {
    // Unit kb-onboard adds read_slot/write_slot (the checked transaction) here.
}
