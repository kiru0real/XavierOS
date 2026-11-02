//! Fixed-capacity kernel event log. Entries are static strings so logging never allocates.
//! Do not use this for formatted IRQ logging; interrupt handlers should update counters only.

use crate::sync::SpinLock;

pub const CAPACITY: usize = 64;

#[derive(Clone, Copy)]
pub enum Level { Debug, Info, Warn, Error }

impl Level {
    pub fn as_str(self) -> &'static str {
        match self { Self::Debug => "DEBUG", Self::Info => "INFO", Self::Warn => "WARN", Self::Error => "ERROR" }
    }
}

#[derive(Clone, Copy)]
pub struct LogEntry {
    pub tick: u64,
    pub level: Level,
    pub message: &'static str,
}

impl LogEntry {
    const EMPTY: Self = Self { tick: 0, level: Level::Info, message: "" };
}

struct LogBuffer {
    entries: [LogEntry; CAPACITY],
    next: usize,
    len: usize,
    dropped: u64,
}

impl LogBuffer {
    const fn new() -> Self {
        Self { entries: [LogEntry::EMPTY; CAPACITY], next: 0, len: 0, dropped: 0 }
    }
}

static LOG: SpinLock<LogBuffer> = SpinLock::new(LogBuffer::new());

pub fn log(level: Level, message: &'static str) {
    let tick = crate::interrupts::timer_ticks();
    let mut buffer = LOG.lock();
    if buffer.len == CAPACITY { buffer.dropped = buffer.dropped.saturating_add(1); }
    // Keep the index read separate from the mutable array access. This avoids
    // overlapping immutable/mutable borrows under the Rust borrow checker.
    let next = buffer.next;
    buffer.entries[next] = LogEntry { tick, level, message };
    buffer.next = (next + 1) % CAPACITY;
    buffer.len = (buffer.len + 1).min(CAPACITY);
}

/// Copy entries in oldest-first order. The destination is caller-owned and no lock is held
/// while the caller formats or prints the records.
pub fn snapshot(destination: &mut [LogEntry]) -> usize {
    let buffer = LOG.lock();
    let count = buffer.len.min(destination.len());
    let first = (buffer.next + CAPACITY - buffer.len) % CAPACITY;
    for index in 0..count {
        destination[index] = buffer.entries[(first + index) % CAPACITY];
    }
    count
}

pub fn dropped_count() -> u64 {
    LOG.lock().dropped
}

pub fn len() -> usize {
    LOG.lock().len
}
