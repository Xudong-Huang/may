use std::hint::cold_path;

#[inline]
pub const fn likely(b: bool) -> bool {
    if !b {
        cold_path()
    }
    b
}

#[inline]
pub const fn unlikely(b: bool) -> bool {
    if b {
        cold_path()
    }
    b
}
