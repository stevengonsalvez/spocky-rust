//! Wall-clock ISO strings and generated ids as the baseline produces them.

use std::time::{SystemTime, UNIX_EPOCH};

/// `Date.now()`: epoch milliseconds.
#[must_use]
pub fn now_millis() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    i64::try_from(millis).unwrap_or(i64::MAX)
}

/// `new Date().toISOString()` for the current time.
#[must_use]
pub fn now_iso() -> String {
    iso_from_millis(now_millis())
}

/// `new Date(millis).toISOString()` within the four-digit year range.
#[must_use]
pub fn iso_from_millis(millis: i64) -> String {
    let days = millis.div_euclid(86_400_000);
    let of_day = millis.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        of_day / 3_600_000,
        of_day / 60_000 % 60,
        of_day / 1000 % 60,
        of_day % 1000
    )
}

/// Proleptic Gregorian date for days since 1970-01-01 (Howard Hinnant).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `randomBytes(count).toString("hex")`, from the OS random source.
#[must_use]
pub fn random_hex(count: usize) -> String {
    let mut bytes = Vec::with_capacity(count + 16);
    while bytes.len() < count {
        // Bytes 0..6 of a v4 UUID are fully random (version bits live in byte 6).
        bytes.extend_from_slice(&uuid::Uuid::new_v4().as_bytes()[..6]);
    }
    let mut hex = String::with_capacity(count * 2);
    for byte in &bytes[..count] {
        hex.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        hex.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    hex
}

/// `generateWorkspaceId`: `wks_` + 16 hex.
#[must_use]
pub fn generate_workspace_id() -> String {
    format!("wks_{}", random_hex(8))
}

/// `generateProjectId`: `prj_` + 16 hex.
#[must_use]
pub fn generate_project_id() -> String {
    format!("prj_{}", random_hex(8))
}

/// `randomUUID()`: a lowercase v4 UUID.
#[must_use]
pub fn random_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::{generate_project_id, generate_workspace_id, iso_from_millis, random_uuid};

    #[test]
    fn iso_strings_match_to_iso_string() {
        // node: new Date(ms).toISOString()
        assert_eq!(iso_from_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            iso_from_millis(1_790_858_096_789),
            "2026-10-01T12:34:56.789Z"
        );
        assert_eq!(iso_from_millis(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(iso_from_millis(-1), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn generated_ids_have_baseline_shape() {
        let workspace = generate_workspace_id();
        assert!(workspace.starts_with("wks_") && workspace.len() == 20);
        assert!(
            workspace[4..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_ne!(workspace, generate_workspace_id());
        assert!(generate_project_id().starts_with("prj_"));
        let uuid = random_uuid();
        assert_eq!(uuid.len(), 36);
        assert_eq!(&uuid[14..15], "4");
    }
}
