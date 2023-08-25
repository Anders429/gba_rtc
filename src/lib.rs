#![no_std]

mod bcd;

use bcd::Bcd;
use core::ops::{BitAnd, BitOr};
use time::{Duration, Month, PrimitiveDateTime};

/// I/O Port Data.
///
/// Used for sending data directly to the RTC chip.
const DATA: *mut Data = 0x080000c4 as *mut Data;

/// I/O Port Direction.
///
/// This specifies which bits are writable and which bits are readable.
const RW_MODE: *mut RwMode = 0x080000c6 as *mut RwMode;

/// I/O Port Control.
///
/// By setting this to `1`, the General Purpose I/O (GPIO) will be both readable and writable.
const ENABLE: *mut u16 = 0x080000c8 as *mut u16;

/// Interrupt Master Enable.
///
/// This register allows enabling and disabling interrupts.
const IME: *mut bool = 0x0400_0208 as *mut bool;

/// Errors that may occur when interacting with the RTC.
#[derive(Debug)]
pub enum Error {
    PowerFailure,
    TestMode,
    AmPmBitPresent,
    InvalidStatus,
    InvalidDay,
    InvalidHour,
    InvalidMinute,
    InvalidSecond,
    InvalidBinaryCodedDecimal,
    TimeComponentRange(time::error::ComponentRange),
    Overflow,
}

/// A command used to interact with the RTC.
///
/// These commands are defined in the S-3511A specification.
enum Command {
    Reset = 0x60,
    WriteStatus = 0x62,
    ReadStatus = 0x63,
    ReadDateTime = 0x65,
    ReadTime = 0x67,
}

/// Configurations for I/O port direction.
///
/// There are three relevant bits for RTC:
/// - 0: SCK (Serial Clock Input)
/// - 1: SIO (Serial Data Input/Output)
/// - 2: CS (Chip Select)
///
/// Both SCK and CS should always be set high. Therefore, the only relevant bit is SIO, which can
/// either be set low to receive data or set high to send data, a single bit at a time.
#[repr(u16)]
enum RwMode {
    /// Sets SIO low, allowing data to be received from the RTC.
    Read = 5,
    /// Sets SIO high, allowing data to be sent to the RTC.
    Write = 7,
}

/// Data written to or received from the RTC.
///
/// While this is a 16-bit value, only the lowest 3 bits are used. This is because the RTC only
/// uses 3 of the 4 possible bits for interacting with the GPIO.
struct Data(u16);

impl Data {
    /// Serial Clock Input.
    const SCK: Data = Data(0b0000_0000_0000_0001);
    /// Serial Data Input/Output.
    const SIO: Data = Data(0b0000_0000_0000_0010);
    /// Chip Select.
    const CS: Data = Data(0b0000_0000_0000_0100);
}

impl BitOr for Data {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl BitOr<u8> for Data {
    type Output = Self;

    fn bitor(self, other: u8) -> Self::Output {
        Self(self.0 | other as u16)
    }
}

impl BitOr<Data> for u8 {
    type Output = Data;

    fn bitor(self, other: Data) -> Self::Output {
        Data(self as u16 | other.0)
    }
}

impl BitAnd for Data {
    type Output = Self;

    fn bitand(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }
}

impl From<Data> for u8 {
    fn from(data: Data) -> Self {
        data.0 as u8
    }
}

/// Send a command to the RTC.
///
/// This must be called before every interaction with the RTC. See the `Command` variants for more
/// information.
fn send_command(command: Command) {
    let bits = (command as u8) << 1;
    // Bits must be sent from highest to lowest.
    for i in (0..8).rev() {
        let bit = (bits >> i) & 2;
        unsafe {
            DATA.write_volatile(Data::CS | bit);
            DATA.write_volatile(Data::CS | bit);
            DATA.write_volatile(Data::CS | bit);
            DATA.write_volatile(Data::CS | Data::SCK | bit);
        }
    }
}

/// Read a single byte.
fn read_byte() -> u8 {
    let mut byte: u8 = 0;
    for _ in 0..8 {
        unsafe {
            DATA.write_volatile(Data::CS);
            DATA.write_volatile(Data::CS);
            DATA.write_volatile(Data::CS);
            DATA.write_volatile(Data::CS);
            DATA.write_volatile(Data::CS);
            DATA.write_volatile(Data::CS | Data::SCK);
            byte = (byte >> 1) | (((u8::from(DATA.read_volatile() & Data::SIO)) >> 1) << 7);
        }
    }
    byte
}

// Write a single byte.
fn write_byte(byte: u8) {
    for i in 0..8 {
        unsafe {
            let bit = (byte << i) & 1;
            DATA.write_volatile(bit | Data::CS);
            DATA.write_volatile(bit | Data::CS);
            DATA.write_volatile(bit | Data::CS);
            DATA.write_volatile(bit | Data::CS | Data::SCK);
        }
    }
}

/// The RTC's status register.
///
/// This is an 8-bit representation of the various modes and states stored in the RTC itself. All
/// bits except `POWER` are writable. Bits 0, 2, and 4 are unused and therefore should never be
/// set.
struct Status(u8);

impl Status {
    const POWER: Status = Status(0b1000_0000);
    const HOUR_24: Status = Status(0b0100_0000);

    fn contains(&self, other: &Self) -> bool {
        self.0 & other.0 != 0
    }
}

impl TryFrom<u8> for Status {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        // Check for unused bits.
        if value & 0b0001_0101 != 0 {
            Err(Error::InvalidStatus)
        } else {
            Ok(Status(value))
        }
    }
}

/// Attempt to obtain the `Status` register from the RTC.
fn try_read_status() -> Result<Status, Error> {
    // Disable interrupts, storing the previous value.
    //
    // This prevents interrupts while reading data from the device. This is necessary because GPIO
    // reads data one bit at a time.
    let previous_ime = unsafe { IME.read_volatile() };
    unsafe { IME.write_volatile(false) };

    // Request status.
    unsafe {
        DATA.write_volatile(Data::SCK);
        DATA.write_volatile(Data::CS | Data::SCK);
        RW_MODE.write_volatile(RwMode::Write);
    }
    send_command(Command::ReadStatus);

    // Receive status.
    unsafe {
        RW_MODE.write_volatile(RwMode::Read);
    }
    let status = read_byte();
    unsafe {
        DATA.write_volatile(Data::SCK);
        DATA.write_volatile(Data::SCK);
    }

    // Restore the previous interrupt enable value.
    unsafe {
        IME.write_volatile(previous_ime);
    }

    status.try_into()
}

/// A calendar year.
///
/// The contained value must be less than `99`.
struct Year(u8);

/// A day within a month.
///
/// The contained value must not be `0` and must be less than `32`.
struct Day(u8);

/// An hour of the day.
///
/// The contained value must be less than `24`.
struct Hour(u8);

/// A minute within an hour.
///
/// The contained value must be less than `60`.
struct Minute(u8);

/// A second within a minute.
///
/// The contained value must be less than `60`.
struct Second(u8);

fn reset() {
    // Disable interrupts, storing the previous value.
    //
    // This prevents interrupts while reading data from the device. This is necessary because GPIO
    // reads data one bit at a time.
    let previous_ime = unsafe { IME.read_volatile() };
    unsafe { IME.write_volatile(false) };

    // Request reset.
    unsafe {
        DATA.write_volatile(Data::SCK);
        DATA.write_volatile(Data::CS | Data::SCK);
        RW_MODE.write_volatile(RwMode::Write);
    }
    send_command(Command::Reset);
    unsafe {
        DATA.write_volatile(Data::SCK);
        DATA.write_volatile(Data::SCK);
    }

    // Restore the previous interrupt enable value.
    unsafe {
        IME.write_volatile(previous_ime);
    }
}

/// Attempt to read the date and time from the RTC.
fn try_read_datetime() -> Result<(Year, Month, Day, Hour, Minute, Second), Error> {
    // Disable interrupts, storing the previous value.
    //
    // This prevents interrupts while reading data from the device. This is necessary because GPIO
    // reads data one bit at a time.
    let previous_ime = unsafe { IME.read_volatile() };
    unsafe { IME.write_volatile(false) };

    // Request datetime.
    unsafe {
        DATA.write_volatile(Data::SCK);
        DATA.write_volatile(Data::CS | Data::SCK);
        RW_MODE.write_volatile(RwMode::Write);
    }
    send_command(Command::ReadDateTime);

    // Receive datetime.
    unsafe {
        RW_MODE.write_volatile(RwMode::Read);
    }
    let year = read_byte();
    let month = read_byte();
    let day = read_byte();
    let _weekday = read_byte();
    let hour = read_byte();
    let minute = read_byte();
    let second = read_byte();
    unsafe {
        DATA.write_volatile(Data::SCK);
        DATA.write_volatile(Data::SCK);
    }

    // Restore the previous interrupt enable value.
    unsafe {
        IME.write_volatile(previous_ime);
    }

    Ok((
        Bcd::try_from(year)?.into(),
        Bcd::try_from(month)?.try_into()?,
        Bcd::try_from(day)?.try_into()?,
        Bcd::try_from(hour)?.try_into()?,
        Bcd::try_from(minute)?.try_into()?,
        Bcd::try_from(second)?.try_into()?,
    ))
}

fn is_test_mode() -> bool {
    // Disable interrupts, storing the previous value.
    //
    // This prevents interrupts while reading data from the device. This is necessary because GPIO
    // reads data one bit at a time.
    let previous_ime = unsafe { IME.read_volatile() };
    unsafe { IME.write_volatile(false) };

    // Request datetime.
    unsafe {
        DATA.write_volatile(Data::SCK);
        DATA.write_volatile(Data::CS | Data::SCK);
        RW_MODE.write_volatile(RwMode::Write);
    }
    send_command(Command::ReadTime);

    let _hour = read_byte();
    let _minute = read_byte();
    let second = read_byte();

    // Restore the previous interrupt enable value.
    unsafe {
        IME.write_volatile(previous_ime);
    }

    // Check whether the test flag is set.
    second & 0b1000_0000 != 0
}

fn set_status(status: Status) {
    // Disable interrupts, storing the previous value.
    //
    // This prevents interrupts while reading data from the device. This is necessary because GPIO
    // reads data one bit at a time.
    let previous_ime = unsafe { IME.read_volatile() };
    unsafe { IME.write_volatile(false) };

    // Request status write.
    unsafe {
        DATA.write_volatile(Data::SCK);
        DATA.write_volatile(Data::CS | Data::SCK);
        RW_MODE.write_volatile(RwMode::Write);
    }
    send_command(Command::WriteStatus);

    // Write the status.
    write_byte(status.0);
    unsafe {
        DATA.write_volatile(Data::SCK);
        DATA.write_volatile(Data::SCK);
    }

    // Restore the previous interrupt enable value.
    unsafe {
        IME.write_volatile(previous_ime);
    }
}

/// Calculates the number of seconds since the RTC's origin date.
fn calculate_rtc_offset(
    year: Year,
    month: Month,
    day: Day,
    hour: Hour,
    minute: Minute,
    second: Second,
) -> u32 {
    let days = year.0 as u32 * 365
        + (year.0 as u32 - 1) / 4
        + 1
        + match month {
            Month::January => 0,
            Month::February => 31,
            Month::March => 59,
            Month::April => 90,
            Month::May => 120,
            Month::June => 151,
            Month::July => 181,
            Month::August => 212,
            Month::September => 243,
            Month::October => 273,
            Month::November => 304,
            Month::December => 334,
        }
        + if year.0 % 4 == 0 { 1 } else { 0 }
        + day.0 as u32;
    second.0 as u32 + minute.0 as u32 * 60 + hour.0 as u32 + days * 86400
}

/// Access to the Real Time Clock.
///
/// Instantiating a `Clock` initializes the relevant registers for interacting with the RTC,
/// allowing subsequent reads of the RTC's stored date and time. Dates and times are represented
/// using types from the [`time`] crate.
#[derive(Debug)]
pub struct Clock {
    /// The base date and time.
    ///
    /// The date and time are read by calculating the amount of time that has elapsed from this value.
    datetime_offset: PrimitiveDateTime,
    /// The RTC's time, in seconds, corresponding to the stored `datetime_offset`.
    ///
    /// When calculating the current date and time, the current RTC value is offset by this value,
    /// and the difference is added to the stored `datetime_offset`.
    rtc_offset: u32,
}

impl Clock {
    /// Creates a new `Clock` set at the given `datetime`.
    ///
    /// Note that this does not actually change the stored date and time in the RTC itself. While
    /// RTC values are writable on real hardware, they are often not writable in GBA emulators.
    /// Therefore, the date and time are stored as being offset from the current RTC date and time
    /// to maintain maximum compatibility.
    pub fn new(datetime: PrimitiveDateTime) -> Result<Self, Error> {
        // Enable operations with the RTC via General Purpose I/O (GPIO).
        unsafe {
            ENABLE.write_volatile(1);
        }

        // Initialize the RTC itself.
        reset();
        // If the power bit is active, we need to reset.
        let status = try_read_status()?;
        if status.contains(&Status::POWER) {
            reset();
        }
        // If we are in test mode, we need to reset.
        if is_test_mode() {
            reset();
        }
        // Set to 24-hour time.
        set_status(Status::HOUR_24);

        let (year, month, day, hour, minute, second) = try_read_datetime()?;
        let rtc_offset = calculate_rtc_offset(year, month, day, hour, minute, second);

        Ok(Self {
            datetime_offset: datetime,
            rtc_offset,
        })
    }

    /// Reads the currently stored date and time.
    pub fn read_datetime(&self) -> Result<PrimitiveDateTime, Error> {
        let (year, month, day, hour, minute, second) = try_read_datetime()?;
        let rtc_offset = calculate_rtc_offset(year, month, day, hour, minute, second);
        let duration = if rtc_offset >= self.rtc_offset {
            Duration::seconds((rtc_offset - self.rtc_offset).into())
        } else {
            Duration::seconds((3_155_760_000 - self.rtc_offset + rtc_offset).into())
        };

        self.datetime_offset
            .checked_add(duration)
            .ok_or(Error::Overflow)
    }

    /// Writes a new date and time.
    ///
    /// Note that this does not actually change the stored date and time in the RTC itself. While
    /// RTC values are writable on real hardware, they are often not writable in GBA emulators.
    /// Therefore, the date and time are stored as being offset from the current RTC date and time
    /// to maintain maximum compatibility.
    pub fn write_datetime(&mut self, datetime: PrimitiveDateTime) -> Result<(), Error> {
        let (year, month, day, hour, minute, second) = try_read_datetime()?;
        let rtc_offset = calculate_rtc_offset(year, month, day, hour, minute, second);
        self.datetime_offset = datetime;
        self.rtc_offset = rtc_offset;
        Ok(())
    }
}
