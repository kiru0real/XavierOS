//! Wall-clock support for XavierOS.
//!
//! The CMOS RTC is used as a fallback UTC source. An NTP query can establish a
//! better UTC baseline, which then advances using the monotonic PIT tick count.
//! Time-zone configuration is a manual UTC offset and is session-local.

use core::arch::asm;
use core::sync::atomic::{AtomicI32, AtomicU8, AtomicU64, Ordering};

use crate::interrupts;

const PIT_HZ: u64 = 100;
static NTP_SYNCED: AtomicU8 = AtomicU8::new(0);
static NTP_BASE_EPOCH: AtomicU64 = AtomicU64::new(0);
static NTP_BASE_TICK: AtomicU64 = AtomicU64::new(0);
static TIMEZONE_OFFSET_MINUTES: AtomicI32 = AtomicI32::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RtcSnapshot {
    second: u8,
    minute: u8,
    hour: u8,
    day: u8,
    month: u8,
    year: u8,
    status_b: u8,
}

pub fn is_ntp_synchronized() -> bool {
    NTP_SYNCED.load(Ordering::Acquire) != 0
}

pub fn source_name() -> &'static str {
    if is_ntp_synchronized() { "NTP" } else { "CMOS RTC" }
}

pub fn timezone_offset_minutes() -> i32 {
    TIMEZONE_OFFSET_MINUTES.load(Ordering::Acquire)
}

/// Set a manual fixed offset: UTC, +HH:MM or -HH:MM. DST is not inferred.
pub fn set_timezone(value: &str) -> Result<i32, &'static str> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("UTC") || trimmed == "Z" || trimmed == "+00:00" || trimmed == "-00:00" {
        TIMEZONE_OFFSET_MINUTES.store(0, Ordering::Release);
        return Ok(0);
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() < 5 || (bytes[0] != b'+' && bytes[0] != b'-') {
        return Err("use UTC, +HH:MM, or +HHMM (for example +0300)");
    }
    // Accept both conventional offsets (+03:00) and compact offsets (+0300).
    // This is also useful on keyboard layouts where entering ':' in the shell is awkward.
    let (minute_start, valid_shape) = if bytes.len() == 6 && bytes[3] == b':' {
        (4usize, true)
    } else if bytes.len() == 5 {
        (3usize, true)
    } else {
        (0usize, false)
    };
    if !valid_shape
        || !bytes[1].is_ascii_digit() || !bytes[2].is_ascii_digit()
        || !bytes[minute_start].is_ascii_digit() || !bytes[minute_start + 1].is_ascii_digit()
    {
        return Err("use UTC, +HH:MM, or +HHMM (for example +0300)");
    }
    let hours = ((bytes[1] - b'0') as i32) * 10 + (bytes[2] - b'0') as i32;
    let minutes = ((bytes[minute_start] - b'0') as i32) * 10 + (bytes[minute_start + 1] - b'0') as i32;
    if hours > 14 || minutes > 59 || (hours == 14 && minutes != 0) {
        return Err("timezone offset must be between -14:00 and +14:00");
    }
    let magnitude = hours * 60 + minutes;
    let offset = if bytes[0] == b'-' { -magnitude } else { magnitude };
    TIMEZONE_OFFSET_MINUTES.store(offset, Ordering::Release);
    Ok(offset)
}

/// Synchronize from an NTP server and retain a monotonic baseline in RAM.
pub fn synchronize(server: &str) -> Result<u64, &'static str> {
    // Preserve the last good time if an NTP attempt fails. Retry once for transient packet loss/timeouts.
    let epoch = match crate::net::ntp_query(server) {
        Ok(value) => value,
        Err(first_error) if !first_error.contains("invalid timestamp") => {
            let start = interrupts::timer_ticks();
            let mut spins = 0usize;
            while interrupts::timer_ticks().wrapping_sub(start) < 10 && spins < 2_000_000 {
                spins += 1;
                core::hint::spin_loop();
            }
            crate::net::ntp_query(server).map_err(|_| first_error)?
        }
        Err(error) => return Err(error),
    };
    NTP_BASE_EPOCH.store(epoch, Ordering::Relaxed);
    NTP_BASE_TICK.store(interrupts::timer_ticks(), Ordering::Relaxed);
    NTP_SYNCED.store(1, Ordering::Release);
    Ok(epoch)
}

/// Return the wall-clock date/time using NTP if synchronized, otherwise CMOS RTC.
pub fn now_local() -> Result<DateTime, &'static str> {
    let utc = current_utc_epoch()?;
    let offset = TIMEZONE_OFFSET_MINUTES.load(Ordering::Acquire) as i64 * 60;
    let local = (utc as i64).saturating_add(offset);
    Ok(civil_from_epoch(local))
}

fn current_utc_epoch() -> Result<u64, &'static str> {
    if NTP_SYNCED.load(Ordering::Acquire) != 0 {
        let base_epoch = NTP_BASE_EPOCH.load(Ordering::Acquire);
        let base_tick = NTP_BASE_TICK.load(Ordering::Acquire);
        let elapsed_ticks = interrupts::timer_ticks().wrapping_sub(base_tick);
        return Ok(base_epoch.saturating_add(elapsed_ticks / PIT_HZ));
    }
    rtc_utc_epoch().ok_or("CMOS RTC is unavailable or returned an invalid date")
}

fn rtc_utc_epoch() -> Option<u64> {
    let raw = read_stable_rtc()?;
    let binary = raw.status_b & 0x04 != 0;
    let hour_24 = raw.status_b & 0x02 != 0;
    let mut second = raw.second;
    let mut minute = raw.minute;
    let mut hour = raw.hour;
    let mut day = raw.day;
    let mut month = raw.month;
    let mut year = raw.year;
    let pm = !hour_24 && (hour & 0x80) != 0;
    if !binary {
        second = bcd_to_binary(second)?;
        minute = bcd_to_binary(minute)?;
        hour = bcd_to_binary(hour & 0x7F)?;
        day = bcd_to_binary(day)?;
        month = bcd_to_binary(month)?;
        year = bcd_to_binary(year)?;
    } else if !hour_24 {
        hour &= 0x7F;
    }
    if !hour_24 {
        if hour == 0 || hour > 12 { return None; }
        hour %= 12;
        if pm { hour += 12; }
    }
    let full_year = if year >= 70 { 1900 + year as i64 } else { 2000 + year as i64 };
    epoch_from_components(full_year, month as u32, day as u32, hour as u32, minute as u32, second as u32)
}

fn read_stable_rtc() -> Option<RtcSnapshot> {
    for _ in 0..16 {
        let mut ready = false;
        for _ in 0..100_000 {
            if read_cmos(0x0A) & 0x80 == 0 { ready = true; break; }
            core::hint::spin_loop();
        }
        if !ready { return None; }
        let first = read_rtc_snapshot_once();
        let second = read_rtc_snapshot_once();
        if first == second && read_cmos(0x0A) & 0x80 == 0 { return Some(second); }
    }
    None
}

fn read_rtc_snapshot_once() -> RtcSnapshot {
    RtcSnapshot {
        second: read_cmos(0x00),
        minute: read_cmos(0x02),
        hour: read_cmos(0x04),
        day: read_cmos(0x07),
        month: read_cmos(0x08),
        year: read_cmos(0x09),
        status_b: read_cmos(0x0B),
    }
}

fn read_cmos(register: u8) -> u8 {
    let value: u8;
    unsafe {
        asm!("out dx, al", in("dx") 0x70u16, in("al") (register & 0x7F), options(nomem, nostack, preserves_flags));
        asm!("in al, dx", in("dx") 0x71u16, out("al") value, options(nomem, nostack, preserves_flags));
    }
    value
}

fn bcd_to_binary(value: u8) -> Option<u8> {
    let high = value >> 4;
    let low = value & 0x0F;
    if high > 9 || low > 9 { return None; }
    Some(high * 10 + low)
}

fn epoch_from_components(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> Option<u64> {
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 { return None; }
    let month_days = days_in_month(year, month)?;
    if day == 0 || day > month_days { return None; }
    let days = days_from_civil(year, month, day);
    let seconds = days.checked_mul(86_400)?
        .checked_add((hour * 3600 + minute * 60 + second) as i64)?;
    u64::try_from(seconds).ok()
}

fn days_in_month(year: i64, month: u32) -> Option<u32> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return None,
    })
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = year - if month <= 2 { 1 } else { 0 };
    let era = (if adjusted_year >= 0 { adjusted_year } else { adjusted_year - 399 }) / 400;
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = month as i64 + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_epoch(epoch: i64) -> DateTime {
    let days = epoch.div_euclid(86_400);
    let day_seconds = epoch.rem_euclid(86_400) as u32;
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };
    DateTime {
        year,
        month: month as u32,
        day: day as u32,
        hour: day_seconds / 3600,
        minute: (day_seconds % 3600) / 60,
        second: day_seconds % 60,
    }
}

#[cfg(test)]
mod tests {
    use super::{civil_from_epoch, days_from_civil, epoch_from_components};

    #[test]
    fn unix_epoch_and_leap_day_round_trip() {
        assert_eq!(epoch_from_components(1970, 1, 1, 0, 0, 0), Some(0));
        let leap = epoch_from_components(2000, 2, 29, 12, 34, 56).unwrap();
        assert_eq!(civil_from_epoch(leap as i64), super::DateTime {
            year: 2000, month: 2, day: 29, hour: 12, minute: 34, second: 56,
        });
    }

    #[test]
    fn civil_day_conversion_matches_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
        assert_eq!(days_from_civil(2024, 3, 1) - days_from_civil(2024, 2, 28), 2);
    }
}
