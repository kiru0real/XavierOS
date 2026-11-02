//! IRQ-safe synchronization primitives for the current single-core kernel.
//!
//! Lock acquisition disables local interrupts before spinning. This prevents an IRQ
//! handler on the same CPU from deadlocking on a lock held by the interrupted code.
//! The primitive is SMP-capable as a lock, but the kernel as a whole is not SMP-safe yet.

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const INTERRUPT_FLAG: u64 = 1 << 9;
static ACQUISITIONS: AtomicU64 = AtomicU64::new(0);
static CONTENDED: AtomicU64 = AtomicU64::new(0);
static SPIN_ITERATIONS: AtomicU64 = AtomicU64::new(0);

pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
    restore_interrupts: bool,
    // Interrupt state belongs to the CPU that acquired the lock. A guard must not move.
    _cpu_local: PhantomData<*mut ()>,
}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self { locked: AtomicBool::new(false), value: UnsafeCell::new(value) }
    }

    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        let restore_interrupts = disable_local_interrupts();
        let mut waited = false;
        loop {
            match self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) => break,
                Err(_) => {
                    waited = true;
                    SPIN_ITERATIONS.fetch_add(1, Ordering::Relaxed);
                    spin_loop();
                }
            }
        }
        ACQUISITIONS.fetch_add(1, Ordering::Relaxed);
        if waited { CONTENDED.fetch_add(1, Ordering::Relaxed); }
        SpinLockGuard { lock: self, restore_interrupts, _cpu_local: PhantomData }
    }

    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        let restore_interrupts = disable_local_interrupts();
        match self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => {
                ACQUISITIONS.fetch_add(1, Ordering::Relaxed);
                Some(SpinLockGuard { lock: self, restore_interrupts, _cpu_local: PhantomData })
            }
            Err(_) => {
                restore_local_interrupts(restore_interrupts);
                None
            }
        }
    }

    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }
}

impl<T> Deref for SpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        // The guard owns the lock for its full lifetime.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // The guard owns the lock for its full lifetime.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        restore_local_interrupts(self.restore_interrupts);
    }
}

/// Return whether IF was set, then disable local maskable interrupts.
#[inline]
fn disable_local_interrupts() -> bool {
    let flags: u64;
    unsafe {
        core::arch::asm!("pushfq", "pop {}", "cli", out(reg) flags);
    }
    flags & INTERRUPT_FLAG != 0
}

#[inline]
fn restore_local_interrupts(enabled: bool) {
    if enabled {
        unsafe { core::arch::asm!("sti", options(nomem, nostack)); }
    }
}

#[derive(Clone, Copy)]
pub struct SyncStats {
    pub acquisitions: u64,
    pub contended_acquisitions: u64,
    pub spin_iterations: u64,
}

pub fn stats() -> SyncStats {
    SyncStats {
        acquisitions: ACQUISITIONS.load(Ordering::Acquire),
        contended_acquisitions: CONTENDED.load(Ordering::Acquire),
        spin_iterations: SPIN_ITERATIONS.load(Ordering::Acquire),
    }
}

/// Exercise lock acquisition, mutual exclusion, try_lock, and guard release.
fn local_interrupts_enabled() -> bool {
    let flags: u64;
    unsafe { core::arch::asm!("pushfq", "pop {}", out(reg) flags, options(nomem, nostack, preserves_flags)); }
    flags & INTERRUPT_FLAG != 0
}

pub fn self_test() -> Result<(), &'static str> {
    let interrupts_were_enabled = local_interrupts_enabled();
    let lock = SpinLock::new(0u64);
    {
        let mut guard = lock.lock();
        if local_interrupts_enabled() { return Err("SPINLOCK DID NOT DISABLE LOCAL INTERRUPTS"); }
        *guard = 0x5841_5649_4552_4F53;
        if lock.try_lock().is_some() { return Err("TRY_LOCK ACQUIRED AN ALREADY-HELD LOCK"); }
        if local_interrupts_enabled() { return Err("FAILED TRY_LOCK ENABLED INTERRUPTS INSIDE A LOCKED SECTION"); }
    }
    if local_interrupts_enabled() != interrupts_were_enabled {
        return Err("SPINLOCK DID NOT RESTORE THE PREVIOUS INTERRUPT STATE");
    }
    let guard = lock.try_lock().ok_or("TRY_LOCK FAILED ON AN UNLOCKED SPINLOCK")?;
    if *guard != 0x5841_5649_4552_4F53 { return Err("SPINLOCK VALUE DID NOT SURVIVE RELEASE"); }
    drop(guard);
    if local_interrupts_enabled() != interrupts_were_enabled {
        return Err("TRY_LOCK DID NOT RESTORE THE PREVIOUS INTERRUPT STATE");
    }
    Ok(())
}
