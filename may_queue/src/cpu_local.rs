use std::mem::MaybeUninit;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Read Processor ID
///
/// Reads the value of the IA32_TSC_AUX MSR (address C0000103H)
/// into the destination register.
///
/// # Safety
/// May fail with #UD if rdpid is not supported (check CPUID).
#[inline(always)]
pub unsafe fn rdpid() -> usize {
    use std::arch::asm;
    let mut pid: usize;
    asm!("rdpid {}", out(reg) pid);
    pid
}

struct Slot<T> {
    locked: AtomicUsize,
    init: AtomicBool,
    data: MaybeUninit<T>,
}

impl<T> Slot<T> {
    fn new() -> Self {
        Slot {
            locked: AtomicUsize::new(0),
            init: AtomicBool::new(false),
            data: MaybeUninit::uninit(),
        }
    }

    #[inline]
    fn try_lock(&self) -> Result<DataGuard<T>, ()> {
        match self
            .locked
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => Ok(DataGuard { slot: self }),
            Err(_) => Err(()),
        }
    }

    #[inline]
    fn unlock(&self) {
        self.locked.store(0, Ordering::Release);
    }

    #[inline]
    fn set_data(&self, data: T) {
        let ptr = self.data.as_ptr() as *mut T;
        unsafe { ptr.write(data) };
    }
}

impl<T> Drop for Slot<T> {
    fn drop(&mut self) {
        if self.init.load(Ordering::Relaxed) {
            unsafe { self.data.assume_init_drop() };
        }
    }
}

pub struct DataGuard<'a, T> {
    slot: &'a Slot<T>,
}

impl<'a, T> Deref for DataGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { self.slot.data.assume_init_ref() }
    }
}

impl<'a, T> DerefMut for DataGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let ptr = self.slot.data.as_ptr() as *mut T;
        unsafe { &mut *ptr }
    }
}

impl<'a, T> Drop for DataGuard<'a, T> {
    fn drop(&mut self) {
        self.slot.unlock();
    }
}

pub struct CpuLocal<T> {
    slots: Vec<Slot<T>>,
    spin_fn: fn(),
}

impl<T> Default for CpuLocal<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> CpuLocal<T> {
    pub fn new() -> Self {
        let slots = (0..num_cpus::get()).map(|_| Slot::new()).collect();
        // let spin_fn = std::hint::spin_loop;
        let spin_fn = std::thread::yield_now;
        CpuLocal { slots, spin_fn }
    }

    pub fn new_with_spin(spin_fn: fn()) -> Self {
        let slots = (0..num_cpus::get()).map(|_| Slot::new()).collect();
        CpuLocal { slots, spin_fn }
    }

    #[inline]
    pub fn get_or(&self, f: impl FnOnce() -> T) -> DataGuard<T> {
        let pid = unsafe { rdpid() };
        let slot = &unsafe { self.slots.get_unchecked(pid) };
        let data = loop {
            match slot.try_lock() {
                Ok(d) => break d,
                Err(_) => (self.spin_fn)(),
            }
        };

        if !slot.init.load(Ordering::Acquire) {
            slot.set_data(f());
            slot.init.store(true, Ordering::Release);
        }
        data
    }
}
