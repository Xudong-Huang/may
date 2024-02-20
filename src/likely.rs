#[inline]
#[cold]
const fn cold() {}

#[inline]
pub const fn likely(b: bool) -> bool {
    if !b {
        cold()
    }
    b
}

#[inline]
pub const fn unlikely(b: bool) -> bool {
    if b {
        cold()
    }
    b
}
