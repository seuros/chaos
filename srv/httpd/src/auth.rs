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
mod tests;
