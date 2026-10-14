//! Persistent local account database for the XavierOS text console.
//! This is a prototype identity system, not a security boundary: the live console is unprotected.

use core::arch::{asm, x86_64::__cpuid};
use core::cell::UnsafeCell;

use crate::{crypto, diskfs, interrupts, vfs};

const MAX_USERS: usize = 8;
const NAME_CAPACITY: usize = 16;
const SALT_SIZE: usize = 16;
const HASH_SIZE: usize = 32;
const DB_PATH: &str = "/DISK/SYSTEM/USERS.DB";
const MAGIC: &[u8; 8] = b"XUSER002";
const HEADER_SIZE: usize = 9;
const RECORD_SIZE: usize = 1 + NAME_CAPACITY + SALT_SIZE + HASH_SIZE;
const DB_CAPACITY: usize = HEADER_SIZE + MAX_USERS * RECORD_SIZE + 4;
const FAILED_LOGIN_LIMIT: u8 = 3;
const LOGIN_LOCKOUT_TICKS: u64 = 500; // PIT runs at 100 Hz: five seconds.
const RDRAND_RETRIES: usize = 10;

#[derive(Clone, Copy)]
struct Account {
    name: [u8; NAME_CAPACITY],
    name_len: u8,
    salt: [u8; SALT_SIZE],
    hash: [u8; HASH_SIZE],
}

impl Account {
    const fn empty() -> Self {
        Self { name: [0; NAME_CAPACITY], name_len: 0, salt: [0; SALT_SIZE], hash: [0; HASH_SIZE] }
    }
    fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
    }
}

#[derive(Clone, Copy)]
struct State {
    accounts: [Account; MAX_USERS],
    count: usize,
    current: i8, // -1 is the temporary live session; 0..count are persistent accounts.
    initialized: bool,
    failed_login_attempts: u8,
    lockout_until_tick: u64,
}

impl State {
    const fn new() -> Self {
        Self { accounts: [Account::empty(); MAX_USERS], count: 0, current: -1, initialized: false, failed_login_attempts: 0, lockout_until_tick: 0 }
    }
}

struct StateCell(UnsafeCell<State>);
unsafe impl Sync for StateCell {}
impl StateCell {
    const fn new() -> Self { Self(UnsafeCell::new(State::new())) }
    fn get(&self) -> *mut State { self.0.get() }
}

static STATE: StateCell = StateCell::new();

pub fn initialize() -> Result<(), &'static str> {
    let mut loaded = State::new();
    // Fail closed before touching on-disk state. If the database is missing/corrupt,
    // do not leave an old session or stale permissions active in memory.
    unsafe { *STATE.get() = loaded; }
    if !diskfs::mount_status() {
        return Err("USER STORE NEEDS THE WRITABLE DISK");
    }
    ensure_directory("/DISK/SYSTEM")?;
    ensure_directory("/DISK/HOME")?;

    match vfs::stat(DB_PATH) {
        Ok(info) if info.is_dir => return Err("USER DATABASE PATH IS A DIRECTORY"),
        Ok(_) => {
            let mut bytes = [0u8; DB_CAPACITY];
            let length = vfs::read_file(DB_PATH, &mut bytes)?;
            loaded = decode_database(&bytes[..length])?;
        }
        Err("PATH NOT FOUND") => save_database(&loaded)?,
        Err(error) => return Err(error),
    }
    loaded.initialized = true;
    loaded.current = -1;
    for index in 0..loaded.count {
        ensure_home(loaded.accounts[index].name())?;
    }
    unsafe { *STATE.get() = loaded; }
    Ok(())
}

pub fn is_initialized() -> bool { unsafe { (*STATE.get()).initialized } }
pub fn is_live() -> bool { unsafe { !(*STATE.get()).initialized || (*STATE.get()).current < 0 } }
/// The first persistent account is the local administrator. With no accounts, the live session
/// can bootstrap the first one; after that, live becomes an unprivileged recovery session.
pub fn can_manage_accounts() -> bool {
    let state = unsafe { &*STATE.get() };
    if !state.initialized { return state.current < 0; }
    if state.count == 0 { return state.current < 0; }
    state.current == 0
}
pub fn is_admin() -> bool {
    let state = unsafe { &*STATE.get() };
    state.initialized && state.count > 0 && state.current == 0
}
pub fn current_name_len() -> usize {
    let state = unsafe { &*STATE.get() };
    if !state.initialized || state.current < 0 { return 4; }
    let index = state.current as usize;
    if index >= state.count || index >= MAX_USERS { return 4; }
    state.accounts[index].name_len as usize
}

/// Copies the current identity into caller-owned storage. This avoids returning a long-lived
/// reference into mutable global account state across login/logout transitions.
pub fn current_name(output: &mut [u8; NAME_CAPACITY]) -> &str {
    let state = unsafe { &*STATE.get() };
    let source = if !state.initialized || state.current < 0 {
        "live"
    } else {
        let index = state.current as usize;
        if index >= state.count || index >= MAX_USERS { "live" } else { state.accounts[index].name() }
    };
    output[..source.len()].copy_from_slice(source.as_bytes());
    core::str::from_utf8(&output[..source.len()]).unwrap_or("live")
}
pub fn count() -> usize { unsafe { (*STATE.get()).count } }

pub fn list<F>(mut visit: F) where F: FnMut(&str) {
    let state = unsafe { &*STATE.get() };
    for account in state.accounts.iter().take(state.count) { visit(account.name()); }
}

pub fn create(name: &str, password: &[u8]) -> Result<(), &'static str> {
    let snapshot = unsafe { *STATE.get() };
    if !snapshot.initialized { return Err("USER DATABASE IS NOT AVAILABLE"); }
    if !can_manage_accounts() { return Err("CREATE USERS FROM FIRST-RUN LIVE SETUP OR ADMINISTRATOR SESSION"); }
    validate_new_username(name)?;
    validate_password(password)?;

    let mut candidate = snapshot;
    let mut account = Account::empty();
    account.name_len = name.len() as u8;
    account.name[..name.len()].copy_from_slice(name.as_bytes());
    fill_salt(&mut account.salt)?;
    account.hash = crypto::pbkdf2_sha256(password, &account.salt, crypto::PASSWORD_ITERATIONS);

    ensure_directory("/DISK/HOME")?;
    ensure_home(name)?;
    candidate.accounts[candidate.count] = account;
    candidate.count += 1;
    save_database(&candidate)?;
    unsafe { *STATE.get() = candidate; }
    Ok(())
}

pub fn login_lockout_remaining_ticks() -> u64 {
    let now = interrupts::timer_ticks();
    let until = unsafe { (*STATE.get()).lockout_until_tick };
    until.saturating_sub(now)
}

fn record_login_failure() {
    let now = interrupts::timer_ticks();
    let state = unsafe { &mut *STATE.get() };
    if state.lockout_until_tick > now {
        return;
    }
    state.failed_login_attempts = state.failed_login_attempts.saturating_add(1);
    if state.failed_login_attempts >= FAILED_LOGIN_LIMIT {
        state.failed_login_attempts = 0;
        state.lockout_until_tick = now.saturating_add(LOGIN_LOCKOUT_TICKS);
    }
}

pub fn authenticate(name: &str, password: &[u8]) -> Result<(), &'static str> {
    if !is_initialized() { return Err("USER DATABASE IS NOT AVAILABLE"); }
    if !is_live() { return Err("LOG OUT BEFORE SWITCHING ACCOUNTS"); }
    if login_lockout_remaining_ticks() > 0 {
        return Err("LOGIN TEMPORARILY LOCKED AFTER FAILED ATTEMPTS");
    }
    if password.is_empty() || password.len() > 63 {
        record_login_failure();
        return Err("INVALID USERNAME OR PASSWORD");
    }

    // Copy the record fields so no reference into mutable global state survives
    // the expensive PBKDF2 operation.
    let found = {
        let state = unsafe { &*STATE.get() };
        state.accounts[..state.count].iter().enumerate()
            .find(|(_, account)| account.name().eq_ignore_ascii_case(name))
            .map(|(index, account)| (index, account.salt, account.hash))
    };
    let Some((index, salt, stored_hash)) = found else {
        // Comparable password-hash work makes unknown names less distinguishable.
        let dummy_salt = [0xA5u8; SALT_SIZE];
        let _ = crypto::pbkdf2_sha256(password, &dummy_salt, crypto::PASSWORD_ITERATIONS);
        record_login_failure();
        return Err("INVALID USERNAME OR PASSWORD");
    };

    let actual = crypto::pbkdf2_sha256(password, &salt, crypto::PASSWORD_ITERATIONS);
    if !crypto::constant_time_eq(&actual, &stored_hash) {
        record_login_failure();
        return Err("INVALID USERNAME OR PASSWORD");
    }

    let state = unsafe { &mut *STATE.get() };
    state.current = index as i8;
    state.failed_login_attempts = 0;
    state.lockout_until_tick = 0;
    Ok(())
}

pub fn logout() -> Result<(), &'static str> {
    let state_ptr = STATE.get();
    let state = unsafe { &mut *state_ptr };
    if !state.initialized { return Err("USER DATABASE IS NOT AVAILABLE"); }
    if state.current < 0 { return Err("ALREADY IN THE LIVE SESSION"); }
    state.current = -1;
    Ok(())
}

pub fn change_password(old_password: &[u8], new_password: &[u8]) -> Result<(), &'static str> {
    validate_password(new_password)?;
    let snapshot = unsafe { *STATE.get() };
    if !snapshot.initialized { return Err("USER DATABASE IS NOT AVAILABLE"); }
    if snapshot.current < 0 { return Err("LIVE SESSION HAS NO PASSWORD; USE USERADD"); }
    let index = snapshot.current as usize;
    let old_hash = crypto::pbkdf2_sha256(old_password, &snapshot.accounts[index].salt, crypto::PASSWORD_ITERATIONS);
    if !crypto::constant_time_eq(&old_hash, &snapshot.accounts[index].hash) {
        return Err("CURRENT PASSWORD IS INCORRECT");
    }
    let mut candidate = snapshot;
    fill_salt(&mut candidate.accounts[index].salt)?;
    candidate.accounts[index].hash = crypto::pbkdf2_sha256(new_password, &candidate.accounts[index].salt, crypto::PASSWORD_ITERATIONS);
    save_database(&candidate)?;
    unsafe { *STATE.get() = candidate; }
    Ok(())
}

/// Non-live sessions can access the read-only embedded system and their own writable home only.
pub fn may_access_path(path: &str) -> bool {
    if vfs::disk_path(path).is_none() { return true; }
    let state = unsafe { &*STATE.get() };
    if !state.initialized { return false; } // fail closed when the account database is unavailable
    if state.count == 0 && state.current < 0 { return true; } // first-run account setup
    if state.current == 0 { return true; } // first persistent account is administrator
    if state.current < 0 { return false; } // live becomes unprivileged after setup
    let current_index = state.current as usize;
    if current_index >= state.count || current_index >= MAX_USERS { return false; }
    let clean = path.trim_matches('/');
    if clean.split('/').any(|part| part == "." || part == "..") { return false; }
    let mut pieces = clean.split('/');
    let Some(mount) = pieces.next() else { return false; };
    let Some(home) = pieces.next() else { return false; };
    let Some(owner) = pieces.next() else { return false; };
    mount.eq_ignore_ascii_case("DISK")
        && home.eq_ignore_ascii_case("HOME")
        && owner.eq_ignore_ascii_case(state.accounts[current_index].name())
}

pub fn home_path(buffer: &mut [u8; 48]) -> Option<&str> {
    let state = unsafe { &*STATE.get() };
    let name = if !state.initialized || state.current < 0 {
        "live"
    } else {
        let index = state.current as usize;
        if index >= state.count || index >= MAX_USERS { "live" } else { state.accounts[index].name() }
    };
    let prefix = b"/DISK/HOME/";
    let length = prefix.len() + name.len();
    if length > buffer.len() { return None; }
    buffer[..prefix.len()].copy_from_slice(prefix);
    buffer[prefix.len()..length].copy_from_slice(name.as_bytes());
    core::str::from_utf8(&buffer[..length]).ok()
}

pub fn self_test() -> Result<(), &'static str> {
    if !is_initialized() { return Err("USER DATABASE IS NOT INITIALIZED"); }
    crypto::self_test()?;
    let state = unsafe { &*STATE.get() };
    if state.count > MAX_USERS { return Err("USER COUNT EXCEEDS DATABASE CAPACITY"); }
    for i in 0..state.count {
        let account = &state.accounts[i];
        validate_name(account.name())?;
        if account.hash.iter().all(|byte| *byte == 0) { return Err("ACCOUNT HASH IS EMPTY"); }
        if state.accounts[..i].iter().any(|other| other.name().eq_ignore_ascii_case(account.name())) {
            return Err("DUPLICATE ACCOUNT NAME");
        }
    }
    Ok(())
}

pub fn validate_new_username(name: &str) -> Result<(), &'static str> {
    if !is_initialized() { return Err("USER DATABASE IS NOT AVAILABLE"); }
    validate_name(name)?;
    if name.eq_ignore_ascii_case("live") || name.eq_ignore_ascii_case("system") {
        return Err("THIS USERNAME IS RESERVED");
    }
    let state = unsafe { &*STATE.get() };
    if state.accounts[..state.count].iter().any(|account| account.name().eq_ignore_ascii_case(name)) {
        return Err("USER ALREADY EXISTS");
    }
    if state.count >= MAX_USERS { return Err("USER DATABASE IS FULL (MAX 8 USERS)"); }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > NAME_CAPACITY - 1 { return Err("USERNAME MUST BE 1-15 CHARACTERS"); }
    if !name.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-') {
        return Err("USERNAME: USE LOWERCASE LETTERS, DIGITS, '_' OR '-'");
    }
    Ok(())
}

fn validate_password(password: &[u8]) -> Result<(), &'static str> {
    if password.len() < 8 { return Err("PASSWORD MUST BE AT LEAST 8 CHARACTERS"); }
    if password.len() > 63 { return Err("PASSWORD MUST BE AT MOST 63 CHARACTERS"); }
    if !password.iter().all(|byte| (0x20..=0x7E).contains(byte)) { return Err("PASSWORD MUST USE PRINTABLE ASCII"); }
    Ok(())
}

fn ensure_directory(path: &str) -> Result<(), &'static str> {
    match vfs::stat(path) {
        Ok(entry) if entry.is_dir => Ok(()),
        Ok(_) => Err("EXPECTED A DIRECTORY"),
        Err("PATH NOT FOUND") => vfs::mkdir(path),
        Err(error) => Err(error),
    }
}

fn ensure_home(name: &str) -> Result<(), &'static str> {
    let mut bytes = [0u8; 48];
    let path = home_path_for(name, &mut bytes)?;
    ensure_directory(path)
}

fn home_path_for<'a>(name: &str, buffer: &'a mut [u8; 48]) -> Result<&'a str, &'static str> {
    if name.is_empty() || name.len() > NAME_CAPACITY - 1 { return Err("INVALID USERNAME FOR HOME PATH"); }
    let prefix = b"/DISK/HOME/";
    let length = prefix.len() + name.len();
    buffer[..prefix.len()].copy_from_slice(prefix);
    buffer[prefix.len()..length].copy_from_slice(name.as_bytes());
    core::str::from_utf8(&buffer[..length]).map_err(|_| "INVALID HOME PATH")
}

pub fn secure_salt_available() -> bool {
    (__cpuid(1).ecx & (1 << 30)) != 0
}

/// Obtain salt bytes only from the CPU's RDRAND generator. A clock/TSC fallback
/// is intentionally forbidden: predictability would undermine password salts.
fn fill_salt(salt: &mut [u8; SALT_SIZE]) -> Result<(), &'static str> {
    if !secure_salt_available() {
        salt.fill(0);
        return Err("CPU RDRAND IS UNAVAILABLE; SECURE ACCOUNT CREATION IS DISABLED");
    }
    let mut bytes = [0u8; SALT_SIZE];
    for chunk in bytes.chunks_exact_mut(8) {
        let mut obtained = false;
        for _ in 0..RDRAND_RETRIES {
            let mut word: u64 = 0;
            let mut success: u8 = 0;
            unsafe {
                asm!("rdrand {word}", "setc {success}",
                    word = lateout(reg) word,
                    success = lateout(reg_byte) success,
                    options(nomem, nostack));
            }
            if success != 0 {
                chunk.copy_from_slice(&word.to_le_bytes());
                obtained = true;
                break;
            }
        }
        if !obtained {
            salt.fill(0);
            return Err("CPU RDRAND FAILED; SECURE ACCOUNT OPERATION ABORTED");
        }
    }
    *salt = bytes;
    Ok(())
}

fn save_database(state: &State) -> Result<(), &'static str> {
    if state.count > MAX_USERS { return Err("USER DATABASE CAPACITY EXCEEDED"); }
    let mut bytes = [0u8; DB_CAPACITY];
    bytes[..8].copy_from_slice(MAGIC);
    bytes[8] = state.count as u8;
    let mut offset = HEADER_SIZE;
    for account in state.accounts.iter().take(state.count) {
        bytes[offset] = account.name_len;
        offset += 1;
        bytes[offset..offset + NAME_CAPACITY].copy_from_slice(&account.name); offset += NAME_CAPACITY;
        bytes[offset..offset + SALT_SIZE].copy_from_slice(&account.salt); offset += SALT_SIZE;
        bytes[offset..offset + HASH_SIZE].copy_from_slice(&account.hash); offset += HASH_SIZE;
    }
    let checksum = checksum32(&bytes[..offset]);
    bytes[offset..offset + 4].copy_from_slice(&checksum.to_le_bytes());
    vfs::write_file(DB_PATH, &bytes[..offset + 4])
}

fn decode_database(bytes: &[u8]) -> Result<State, &'static str> {
    if bytes.len() < HEADER_SIZE + 4 || bytes.get(..8) != Some(MAGIC.as_slice()) {
        return Err("USER DATABASE HEADER IS INVALID");
    }
    let count = bytes[8] as usize;
    if count > MAX_USERS { return Err("USER DATABASE CONTAINS TOO MANY ACCOUNTS"); }
    let expected_length = HEADER_SIZE + count * RECORD_SIZE + 4;
    if bytes.len() != expected_length { return Err("USER DATABASE SIZE IS INVALID"); }
    let checksum_offset = expected_length - 4;
    let stored = u32::from_le_bytes([bytes[checksum_offset], bytes[checksum_offset + 1], bytes[checksum_offset + 2], bytes[checksum_offset + 3]]);
    if checksum32(&bytes[..checksum_offset]) != stored { return Err("USER DATABASE CHECKSUM FAILED"); }
    let mut state = State::new();
    state.count = count;
    state.initialized = true;
    let mut offset = HEADER_SIZE;
    for index in 0..count {
        let name_len = bytes[offset] as usize; offset += 1;
        if name_len == 0 || name_len > NAME_CAPACITY - 1 { return Err("USER DATABASE NAME LENGTH IS INVALID"); }
        let mut account = Account::empty();
        account.name_len = name_len as u8;
        account.name.copy_from_slice(&bytes[offset..offset + NAME_CAPACITY]); offset += NAME_CAPACITY;
        if account.name[name_len..].iter().any(|byte| *byte != 0) { return Err("USER DATABASE NAME PADDING IS INVALID"); }
        if core::str::from_utf8(&account.name[..name_len]).is_err() { return Err("USER DATABASE NAME IS NOT UTF-8"); }
        validate_name(account.name())?;
        account.salt.copy_from_slice(&bytes[offset..offset + SALT_SIZE]); offset += SALT_SIZE;
        account.hash.copy_from_slice(&bytes[offset..offset + HASH_SIZE]); offset += HASH_SIZE;
        if account.hash.iter().all(|byte| *byte == 0) { return Err("USER DATABASE HASH IS EMPTY"); }
        if state.accounts[..index].iter().any(|other| other.name().eq_ignore_ascii_case(account.name())) {
            return Err("USER DATABASE HAS DUPLICATE NAMES");
        }
        state.accounts[index] = account;
    }
    state.current = -1;
    Ok(state)
}

fn checksum32(bytes: &[u8]) -> u32 {
    let mut hash = 0x811c9dc5u32;
    for byte in bytes { hash = (hash ^ (*byte as u32)).wrapping_mul(0x01000193); }
    hash
}
