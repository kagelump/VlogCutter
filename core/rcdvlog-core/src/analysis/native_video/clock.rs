//! Strict parsing for source-local clock timestamps returned by the model.

use anyhow::{Result, bail, ensure};

/// Parse an unambiguous `MM:SS[.fraction]` or `HH:MM:SS[.fraction]` clock.
///
/// A literal `0` is also accepted as the zero timestamp. This parser does not
/// trim, normalize, clamp, or interpret prose around a clock.
pub(super) fn parse_local_clock(value: &str) -> Result<f64> {
    ensure!(value.len() <= 32, "local clock is longer than 32 bytes");
    if value == "0" {
        return Ok(0.0);
    }

    let parts: Vec<&str> = value.split(':').collect();
    ensure!(
        (parts.len() == 2 || parts.len() == 3) && parts.iter().all(|part| !part.is_empty()),
        "local clock must have two or three colon-separated fields"
    );

    let (last, fraction) = match parts
        .last()
        .expect("validated nonempty parts")
        .split_once('.')
    {
        Some((whole, fraction)) => {
            ensure!(
                !fraction.is_empty()
                    && fraction.len() <= 9
                    && fraction.bytes().all(|byte| byte.is_ascii_digit()),
                "local clock fraction must contain one to nine ASCII digits"
            );
            (whole, Some(fraction))
        }
        None => (*parts.last().expect("validated nonempty parts"), None),
    };

    let mut whole_fields = parts[..parts.len() - 1].to_vec();
    whole_fields.push(last);
    let mut numbers = Vec::with_capacity(whole_fields.len());
    for field in whole_fields {
        ensure!(
            !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit()),
            "local clock fields must contain ASCII digits only"
        );
        let mut number = 0_u64;
        for digit in field.bytes() {
            number = number
                .checked_mul(10)
                .and_then(|number| number.checked_add(u64::from(digit - b'0')))
                .ok_or_else(|| anyhow::anyhow!("local clock integer overflow"))?;
        }
        numbers.push(number);
    }

    let seconds = *numbers.last().expect("clock has fields");
    ensure!(seconds < 60, "local clock seconds must be below 60");
    if numbers.len() == 3 {
        ensure!(numbers[1] < 60, "local clock minutes must be below 60");
    }

    let whole_seconds = match numbers.as_slice() {
        [minutes, seconds] => minutes
            .checked_mul(60)
            .and_then(|value| value.checked_add(*seconds)),
        [hours, minutes, seconds] => hours
            .checked_mul(60)
            .and_then(|value| value.checked_add(*minutes))
            .and_then(|value| value.checked_mul(60))
            .and_then(|value| value.checked_add(*seconds)),
        _ => None,
    }
    .ok_or_else(|| anyhow::anyhow!("local clock duration overflow"))?;

    let fractional_seconds = if let Some(fraction) = fraction {
        let digits = fraction.len() as u32;
        let mut numerator = 0_u64;
        for digit in fraction.bytes() {
            numerator = numerator
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(digit - b'0')))
                .ok_or_else(|| anyhow::anyhow!("local clock fraction overflow"))?;
        }
        numerator as f64 / 10_f64.powi(digits as i32)
    } else {
        0.0
    };
    let result = whole_seconds as f64 + fractional_seconds;
    ensure!(result.is_finite(), "local clock duration is not finite");
    if result < 0.0 {
        bail!("local clock duration must not be negative");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::parse_local_clock;

    #[test]
    fn parses_strict_local_clocks() {
        let cases = [
            ("0", Some(0.0)),
            ("00:00.500", Some(0.5)),
            ("1:02.125", Some(62.125)),
            ("01:02:03.000000001", Some(3723.000000001)),
            ("-1:02", None),
            ("", None),
            ("1", None),
            ("1:2:3:4", None),
            ("00:60", None),
            ("01:60:00", None),
            ("NaN", None),
            ("00:00.1234567890", None),
            ("00:00.1.2", None),
            ("00:00 suffix", None),
            ("０１:02", None),
            (" 01:02", None),
            ("01:02 ", None),
            ("9999999999999999999999999999999:00", None),
            ("000000000000000000000000000000000", None),
        ];

        for (input, expected) in cases {
            match expected {
                Some(seconds) => {
                    assert_eq!(parse_local_clock(input).unwrap(), seconds, "{input:?}")
                }
                None => assert!(parse_local_clock(input).is_err(), "{input:?}"),
            }
        }
    }
}
