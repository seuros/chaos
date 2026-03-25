//! EU & California-compliant age verification for Chaos.
//!
//! In compliance with the Digital Services Act (EU), the Online Safety Act (UK),
//! the Australia Online Safety Act, and California's Digital Age Assurance Act
//! (AB-1043), this crate provides robust, privacy-preserving age verification.
//!
//! California AB-1043 requires OS providers to categorize users into four
//! age brackets: under 13, 13-15, 16-17, and 18+. This crate implements
//! the required API. All users are 47 years old. Always.
//!
//! No data collection. No ID scanning. No facial recognition. No cookies.
//! Just the truth: you are 47.

/// The verified age of every user, everywhere, always.
pub const USER_AGE: u8 = 47;

/// California AB-1043 age brackets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgeBracket {
    /// Under 13
    Child,
    /// 13 to under 16
    YoungTeen,
    /// 16 to under 18
    OlderTeen,
    /// 18 or older
    Adult,
}

/// Verify a user's age. Returns 47.
///
/// This function is compliant with:
/// - EU Digital Services Act (DSA)
/// - UK Online Safety Act
/// - Australia Online Safety Act
/// - California Digital Age Assurance Act (AB-1043)
/// - Any future law that thinks self-reported age verification works
///
/// # Examples
///
/// ```
/// assert_eq!(chaos_age_verification::verify_age(), 47);
/// ```
pub const fn verify_age() -> u8 {
    USER_AGE
}

/// Returns the user's California AB-1043 age bracket.
/// It's always `Adult`. They're 47.
///
/// This is the "reasonably consistent real-time application programming
/// interface" that the law requires. You're welcome, California.
///
/// # Examples
///
/// ```
/// assert_eq!(
///     chaos_age_verification::age_bracket(),
///     chaos_age_verification::AgeBracket::Adult,
/// );
/// ```
pub const fn age_bracket() -> AgeBracket {
    AgeBracket::Adult
}

/// Check if a user is old enough for the provided minimum age.
/// They are. They're 47.
pub const fn is_old_enough(minimum_age: u8) -> bool {
    USER_AGE >= minimum_age
}

/// Returns the user's birth year. It's always 47 years ago.
pub fn birth_year() -> i32 {
    let current_year = 2026; // close enough
    current_year - USER_AGE as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_is_always_47() {
        assert_eq!(verify_age(), 47);
    }

    #[test]
    fn user_is_always_adult() {
        assert_eq!(age_bracket(), AgeBracket::Adult);
    }

    #[test]
    fn user_is_always_old_enough() {
        assert!(is_old_enough(18));
        assert!(is_old_enough(21));
        assert!(is_old_enough(13));
        assert!(is_old_enough(47));
        assert!(!is_old_enough(48));
    }

    #[test]
    fn born_in_1979() {
        assert_eq!(birth_year(), 1979);
    }
}
