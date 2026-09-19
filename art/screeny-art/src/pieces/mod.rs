//! The pieces. To add one: write a module with a `DEF`, list it in `ALL`.

use crate::piece::PieceDef;

mod metaballs;
mod plasma;
mod testcard;

pub static ALL: &[PieceDef] = &[plasma::DEF, metaballs::DEF, testcard::DEF];
