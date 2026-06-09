// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt::Write;

use chrono::{DateTime, TimeZone};
use num_format::{Locale as NumFormatLocale, ToFormattedString};
use rusty_money::{iso, Money};

// Since we are not compiling under Slint, we need to recreate the missing enums
pub enum TimeLength {
    Short,  // HH:MM
    Medium, // HH:MM:SS
    Long,   // HH:MM:SS.sss
}

pub enum TimeFormat {
    TwelveHour,
    TwentyFourHour,
}

pub enum DateLength {
    Short,  // MM-DD-YY, DD-MM-YY, YY-MM-DD
    Medium, // MMM DD, YYYY, DD MMM, YYYY, YYY MM, DD
    Long,   // MMMM DD, YYYY, DD MMMM, YYYY, DD MMMM, YYYY
}

pub enum DateFormat {
    MDY,
    DMY,
    YMD,
}

// Helper function to replace {0}, {1}, etc., with corresponding arguments
pub fn replace_placeholders<S>(translated: &str, args: &[S]) -> String
where
    S: AsRef<str>,
{
    let mut result = String::new();
    let mut remaining = translated;

    while let Some(start) = remaining.find('{') {
        // Copy everything before the '{'
        result.push_str(&remaining[..start]);

        // Look for closing brace
        if let Some(end) = remaining[start + 1..].find('}') {
            let end = start + 1 + end;

            // Try to parse the content between braces
            if let Ok(index) = remaining[start + 1..end].parse::<usize>() {
                if let Some(arg) = args.get(index) {
                    result.push_str(arg.as_ref());
                    remaining = &remaining[end + 1..];
                    continue;
                }
            }
        }

        // If we couldn't parse or replace, include the '{' and continue
        result.push('{');
        remaining = &remaining[start + 1..];
    }

    // Copy any remaining text
    result.push_str(remaining);

    result
}

pub fn format_date<Tz>(date: DateTime<Tz>, date_length: DateLength, date_format: DateFormat) -> String
where
    Tz: TimeZone,                  // Tz must implement TimeZone
    Tz::Offset: std::fmt::Display, // Offset for Tz must implement Display
{
    let format_str = match (date_length, date_format) {
        // Short date formats
        (DateLength::Short, DateFormat::MDY) => "%m/%d/%y", // MM-DD-YY
        (DateLength::Short, DateFormat::DMY) => "%d/%m/%y", // DD-MM-YY
        (DateLength::Short, DateFormat::YMD) => "%y/%m/%d", // YY-MM-DD

        // Medium date formats
        (DateLength::Medium, DateFormat::MDY) => "%b %d, %Y", // MMM DD, YYYY
        (DateLength::Medium, DateFormat::DMY) => "%d %b, %Y", // DD MMM, YYYY
        (DateLength::Medium, DateFormat::YMD) => "%Y %b, %d", // YYYY MMM, DD

        // Long date formats
        (DateLength::Long, DateFormat::MDY) => "%B %d, %Y", // MMMM DD, YYYY
        (DateLength::Long, DateFormat::DMY) => "%d %B, %Y", // DD MMMM, YYYY
        (DateLength::Long, DateFormat::YMD) => "%Y %B %d",  // YYYY MMMM DD
    };

    date.format(format_str).to_string()
}

pub fn format_time<Tz>(time: DateTime<Tz>, time_length: TimeLength, time_format: TimeFormat) -> String
where
    Tz: TimeZone,                  // Tz must implement TimeZone
    Tz::Offset: std::fmt::Display, // Offset for Tz must implement Display
{
    // Build the format string based on the provided enums
    let time_format_str = match (time_length, time_format) {
        // 12-hour format
        (TimeLength::Short, TimeFormat::TwelveHour) => "%I:%M %p", // HH:MM AM/PM
        (TimeLength::Medium, TimeFormat::TwelveHour) => "%I:%M:%S %p", // HH:MM:SS AM/PM
        (TimeLength::Long, TimeFormat::TwelveHour) => "%I:%M:%S%.3f %p", // HH:MM:SS.sss AM/PM

        // 24-hour format
        (TimeLength::Short, TimeFormat::TwentyFourHour) => "%H:%M", // HH:MM
        (TimeLength::Medium, TimeFormat::TwentyFourHour) => "%H:%M:%S", // HH:MM:SS
        (TimeLength::Long, TimeFormat::TwentyFourHour) => "%H:%M:%S%.3f", // HH:MM:SS.ss3
    };

    time.format(time_format_str).to_string()
}

// Function to format integers
pub fn format_int(value: i32, locale_str: &str) -> String {
    let locale = NumFormatLocale::from_name(locale_str).unwrap();
    value.to_formatted_string(&locale).to_string()
}

// Function to format floats
pub fn format_float(value: f32, decimals: i32, locale_str: &str) -> String {
    // Use `format_int()` to format the integer part
    let integer_part = value.trunc() as i32;
    let formatted_integer = format_int(integer_part, locale_str);

    // Format the fractional part with the specified number of decimal places
    let fractional_part = value.fract().abs();
    let fractional_format = format!("{:.*}", decimals as usize, fractional_part);
    let formatted_fractional = &fractional_format[2..]; // Skip "0."

    // Lookup the separator from the current locale
    let decimal_separator = match NumFormatLocale::from_name(locale_str) {
        Ok(locale) => locale.decimal(),
        Err(_e) => ".",
    };

    // Combine the formatted integer and fractional parts
    let mut result = String::new();
    write!(result, "{formatted_integer}{decimal_separator}{formatted_fractional}").unwrap();

    result
}

pub fn format_file_size(bytes: u64, locale_str: &str) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit_index = 0;

    while value >= 1024.0 && unit_index < units.len() - 1 {
        value /= 1024.0;
        unit_index += 1;
    }

    let value = if unit_index == 0 {
        format_u64(bytes, locale_str)
    } else {
        format_file_size_decimal(value, locale_str)
    };

    format!("{value} {}", units[unit_index])
}

fn format_u64(value: u64, locale_str: &str) -> String {
    let locale = NumFormatLocale::from_name(locale_str).unwrap_or(NumFormatLocale::en);
    value.to_formatted_string(&locale)
}

fn format_file_size_decimal(value: f64, locale_str: &str) -> String {
    let rounded = if value < 10.0 { format!("{value:.1}") } else { format!("{value:.0}") };

    let Some((integer, fractional)) = rounded.split_once('.') else {
        return format_u64(rounded.parse().unwrap_or_default(), locale_str);
    };
    let integer = format_u64(integer.parse().unwrap_or_default(), locale_str);
    if fractional == "0" {
        return integer;
    }

    let decimal_separator =
        NumFormatLocale::from_name(locale_str).map(|locale| locale.decimal()).unwrap_or(".");

    format!("{integer}{decimal_separator}{fractional}")
}

pub fn format_currency(major: i32, minor: i32, currency: &str) -> String {
    // Use the rusty_money crate to format the currency
    match iso::find(currency) {
        Some(iso_curr) => {
            let full_major: i64 = major as i64 * 10_i64.pow(iso_curr.exponent as u32);
            let full_minor =
                if full_major >= 0 { full_major + minor as i64 } else { full_major - minor as i64 };
            Money::from_minor(full_minor, iso_curr).to_string()
        }
        None => "INVALID CURRENCY".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    //=========================================================================
    // Date and Time formatting tests
    //=========================================================================
    #[test]
    fn test_format_date_dmy() {
        let date = Utc.with_ymd_and_hms(2022, 12, 31, 12, 0, 9).unwrap();
        assert_eq!(format_date::<Utc>(date, DateLength::Short, DateFormat::DMY), "31/12/22");
        assert_eq!(format_date::<Utc>(date, DateLength::Medium, DateFormat::DMY), "31 Dec, 2022");
        assert_eq!(format_date::<Utc>(date, DateLength::Long, DateFormat::DMY), "31 December, 2022");
    }

    #[test]
    fn test_format_date_mdy() {
        let date = Utc.with_ymd_and_hms(2022, 12, 31, 12, 0, 9).unwrap();
        assert_eq!(format_date::<Utc>(date, DateLength::Short, DateFormat::MDY), "12/31/22");
        assert_eq!(format_date::<Utc>(date, DateLength::Medium, DateFormat::MDY), "Dec 31, 2022");
        assert_eq!(format_date::<Utc>(date, DateLength::Long, DateFormat::MDY), "December 31, 2022");
    }

    #[test]
    fn test_format_date_ymd() {
        let date = Utc.with_ymd_and_hms(2022, 12, 31, 12, 0, 9).unwrap();

        assert_eq!(format_date::<Utc>(date, DateLength::Short, DateFormat::YMD), "22/12/31");
        assert_eq!(format_date::<Utc>(date, DateLength::Medium, DateFormat::YMD), "2022 Dec, 31");
        assert_eq!(format_date::<Utc>(date, DateLength::Long, DateFormat::YMD), "2022 December 31");
    }

    #[test]
    fn test_format_time_12() {
        let time = Utc.with_ymd_and_hms(2014, 11, 28, 23, 59, 59).unwrap();
        // let tz: Option<Tz> = Some(chrono_tz::America::New_York);

        // Time in 12-hour format
        assert_eq!(format_time::<Utc>(time, TimeLength::Short, TimeFormat::TwelveHour), "11:59 PM");
        assert_eq!(format_time::<Utc>(time, TimeLength::Medium, TimeFormat::TwelveHour), "11:59:59 PM");
        assert_eq!(format_time::<Utc>(time, TimeLength::Long, TimeFormat::TwelveHour), "11:59:59.000 PM");
    }

    #[test]
    fn test_format_time_24() {
        let time = Utc.with_ymd_and_hms(2014, 11, 28, 23, 59, 59).unwrap();
        // let tz: Option<Tz> = Some(chrono_tz::America::New_York);

        // Time in 24-hour format
        assert_eq!(format_time::<Utc>(time, TimeLength::Short, TimeFormat::TwentyFourHour), "23:59");
        assert_eq!(format_time::<Utc>(time, TimeLength::Medium, TimeFormat::TwentyFourHour), "23:59:59");
        assert_eq!(format_time::<Utc>(time, TimeLength::Long, TimeFormat::TwentyFourHour), "23:59:59.000");
    }

    //=========================================================================
    // Number formatting tests
    //=========================================================================
    #[test]
    fn test_format_int_en() {
        let locale = "en";
        assert_eq!(format_int(1234567, &locale), "1,234,567");
        assert_eq!(format_int(123456789, &locale), "123,456,789");
        assert_eq!(format_int(2045678901, &locale), "2,045,678,901");
    }

    #[test]
    fn test_format_int_es() {
        let locale = "es";
        assert_eq!(format_int(1234567, &locale), "1.234.567");
        assert_eq!(format_int(123456789, &locale), "123.456.789");
        assert_eq!(format_int(2045678901, &locale), "2.045.678.901");
    }

    #[test]
    fn test_format_float_en() {
        let locale = "en";
        assert_eq!(format_float(1234.56789, 2, &locale), "1,234.57");
        assert_eq!(format_float(1234.0, 2, &locale), "1,234.00");
        assert_eq!(format_float(1234.123, 5, &locale), "1,234.12305"); // Differs due to floating point limits
    }

    #[test]
    fn test_format_float_es() {
        let locale = "es";
        assert_eq!(format_float(1234.56789, 2, &locale), "1.234,57");
        assert_eq!(format_float(1234.0, 2, &locale), "1.234,00");
        assert_eq!(format_float(1234.123, 5, &locale), "1.234,12305"); // Differs due to floating point limits
    }

    #[test]
    fn test_format_file_size_en() {
        assert_eq!(format_file_size(512, "en"), "512 B");
        assert_eq!(format_file_size(1024, "en"), "1 KiB");
        assert_eq!(format_file_size(1536, "en"), "1.5 KiB");
        assert_eq!(format_file_size(1_234_567, "en"), "1.2 MiB");
    }

    #[test]
    fn test_format_file_size_es() {
        assert_eq!(format_file_size(1536, "es"), "1,5 KiB");
        assert_eq!(format_file_size(1_234_567, "es"), "1,2 MiB");
    }

    //=========================================================================
    // Currency formatting tests
    //=========================================================================
    #[test]
    fn test_format_usd_currency() {
        let result = format_currency(1234, 56, "USD");
        assert_eq!(result, "$1,234.56", "Currency formatting for USD failed");
    }

    #[test]
    fn test_format_eur_currency() {
        let result = format_currency(1234, 56, "EUR");
        assert_eq!(result, "€1.234,56", "Currency formatting for EUR failed");
    }

    #[test]
    fn test_format_jpy_currency() {
        let result = format_currency(123456, 0, "JPY"); // No decimal places for JPY
        assert_eq!(result, "¥123,456", "Currency formatting for JPY failed");
    }

    #[test]
    fn test_invalid_currency() {
        let result = format_currency(1234, 56, "INVALID");
        println!("**** Invalid test result: {}", result);
        assert_eq!(result, "INVALID CURRENCY", "Expected INVALID CURRENCY for invalid currency code");
    }

    #[test]
    fn test_large_amount_currency() {
        let result = format_currency(1_000_000_000, 99, "USD");
        assert_eq!(result, "$1,000,000,000.99", "Currency formatting for large amounts failed");
    }

    #[test]
    fn test_negative_amount_currency() {
        let result = format_currency(-1234, 56, "USD");
        assert_eq!(result, "-$1,234.56", "Currency formatting for negative amounts failed");
    }

    #[test]
    fn test_replace_placeholders() {
        // Basic single digit placeholders
        assert_eq!(replace_placeholders("Hello {0}!", &["World"]), "Hello World!");

        // Multiple placeholders
        assert_eq!(replace_placeholders("{0} {1} {2}", &["A", "B", "C"]), "A B C");

        // Multi-digit placeholders
        assert_eq!(
            replace_placeholders(
                "Item {10} of {11}",
                &["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "ten", "eleven"]
            ),
            "Item ten of eleven"
        );

        // Out of bounds placeholder
        assert_eq!(replace_placeholders("Hello {5}!", &["World"]), "Hello {5}!");

        // Incomplete placeholder (no closing brace)
        assert_eq!(replace_placeholders("Hello {0", &["World"]), "Hello {0");

        // Non-digit after opening brace
        assert_eq!(replace_placeholders("Hello {abc}", &["World"]), "Hello {abc}");

        // Empty placeholders
        assert_eq!(replace_placeholders("Hello {}", &["World"]), "Hello {}");

        assert_eq!(replace_placeholders("{0} of {1} Keycard Backup", &["2", "3"]), "2 of 3 Keycard Backup");
    }
}
