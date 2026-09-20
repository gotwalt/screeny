//! What this patch promises, checked.

use super::*;
use crate::patch::Params;
use crate::snapshot::{self, Shot};

/// The same seed at the same moment is the same PNG. Everything else here
/// leans on this.
#[test]
fn the_same_seed_at_the_same_moment_is_the_same_frame() {
    let params = Params::defaults(PARAMS);
    let shot = Shot { seed: 7, at: 9.0, warmup: 9.0, ..Shot::default() };
    let a = snapshot::take(&DEF, &params, &shot);
    let b = snapshot::take(&DEF, &params, &shot);
    assert_eq!(a.wire.rgb, b.wire.rgb);
}
