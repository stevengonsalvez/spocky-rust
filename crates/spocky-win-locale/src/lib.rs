//! The Windows user locale name, for `spocky-contracts`' default locale.
//!
//! Node on Windows ignores `LC_ALL`, `LC_MESSAGES`, and `LANG`: ICU asks the
//! system for the user's regional format (`GetUserDefaultLocaleName`). This
//! crate holds that one call so `spocky-contracts` can keep
//! `forbid(unsafe_code)`.

/// The user's default locale name (a BCP 47 tag such as `en-US` or
/// `tr-TR`), or `None` where the call fails or the platform is not Windows.
#[must_use]
pub fn user_default_locale_name() -> Option<String> {
    imp::user_default_locale_name()
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;

    /// `LOCALE_NAME_MAX_LENGTH`, in UTF-16 units including the terminator.
    const LOCALE_NAME_MAX_LENGTH: usize = 85;

    #[allow(unsafe_code)] // One FFI call into kernel32 with a buffer of the documented size.
    pub fn user_default_locale_name() -> Option<String> {
        let mut buffer = [0_u16; LOCALE_NAME_MAX_LENGTH];
        // SAFETY: `buffer` is writable for `LOCALE_NAME_MAX_LENGTH` units, the
        // size the API documents as sufficient and the length passed here.
        let written = unsafe {
            GetUserDefaultLocaleName(buffer.as_mut_ptr(), i32::try_from(buffer.len()).ok()?)
        };
        let length = usize::try_from(written).ok()?.checked_sub(1)?;
        String::from_utf16(buffer.get(..length)?).ok()
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn user_default_locale_name() -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(windows))]
    #[test]
    fn there_is_no_user_locale_off_windows() {
        assert_eq!(super::user_default_locale_name(), None);
    }

    #[cfg(windows)]
    #[test]
    fn the_user_locale_is_a_language_tag() {
        let name = super::user_default_locale_name().expect("a locale name");
        assert!(name.len() >= 2 && name.is_ascii(), "{name}");
    }
}
