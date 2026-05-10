/// Validate the `Authorization` header value against the configured bearer
/// token. Returns `Ok(())` on success or an error description on failure.
///
/// Never logs the configured token or the incoming header value.
pub fn validate_bearer(
    header_value: Option<&str>,
    expected_token: &str,
) -> Result<(), &'static str> {
    let value = header_value.ok_or("unauthorized")?;

    // Must start with exactly "Bearer " (case-sensitive, single space).
    let token = value.strip_prefix("Bearer ").ok_or("unauthorized")?;

    if token.is_empty() {
        return Err("unauthorized");
    }

    if !constant_time_eq(token.as_bytes(), expected_token.as_bytes()) {
        return Err("unauthorized");
    }

    Ok(())
}

/// Constant-time byte comparison to prevent timing side-channels on token
/// validation. Returns `true` iff slices are equal. Work is always
/// proportional to the *expected* token length so attacker-controlled input
/// cannot influence the number of iterations.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u64;
    // Always iterate the expected token length (b). If a is shorter, the
    // out-of-bounds index wraps to 0 via modular arithmetic; the length
    // XOR already guarantees a non-zero diff in that case.
    let a_len = a.len().max(1); // avoid division by zero
    for (i, &y) in b.iter().enumerate() {
        diff |= (a[i % a_len] ^ y) as u64;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "s3cret-tok3n";

    #[test]
    fn accepts_correct_bearer() {
        assert!(validate_bearer(Some("Bearer s3cret-tok3n"), TOKEN).is_ok());
    }

    #[test]
    fn rejects_missing_header() {
        assert_eq!(validate_bearer(None, TOKEN), Err("unauthorized"));
    }

    #[test]
    fn rejects_wrong_scheme() {
        assert_eq!(
            validate_bearer(Some("Basic abc"), TOKEN),
            Err("unauthorized")
        );
    }

    #[test]
    fn rejects_wrong_token() {
        assert_eq!(
            validate_bearer(Some("Bearer wrong"), TOKEN),
            Err("unauthorized")
        );
    }

    #[test]
    fn rejects_empty_bearer_value() {
        assert_eq!(validate_bearer(Some("Bearer "), TOKEN), Err("unauthorized"));
    }

    #[test]
    fn rejects_lowercase_bearer() {
        assert_eq!(
            validate_bearer(Some("bearer s3cret-tok3n"), TOKEN),
            Err("unauthorized")
        );
    }

    #[test]
    fn rejects_no_space_after_bearer() {
        assert_eq!(
            validate_bearer(Some("Bearertoken"), TOKEN),
            Err("unauthorized")
        );
    }
}
