// Read as src/hook/x.rs (module path hook::x) with child module k.
// Named: a::b, c::d, e, hook (super), g::h, hook::x (self and k), m::n, o, o::p.
use crate::a::b::C;
use crate::{c::d, e};
use super::f;
use super::super::g::h;
use self::i::j;
use k::l;
use crate::o::{self, p};

mod k;

fn x() {
    $crate::m::n!();
    let _ = "crate::q::r"; // not code: crate::s::t
    let _ = Self::new();
    let _ = std::k::Thing;
}
