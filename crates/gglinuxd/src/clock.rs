// SPDX-License-Identifier: GPL-3.0-or-later
//! Small timing helper shared by the device workers.

use std::time::{Duration, Instant};

/// Whether `at` is unset or longer ago than `age`.
pub fn is_older_than(at: Option<Instant>, age: Duration) -> bool {
    at.is_none_or(|t| t.elapsed() >= age)
}
