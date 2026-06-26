/*
 * Shard key extraction from SQL — shared by router gateway and 2PC batches.
 */

/// Extract a routing key from SQL (minimal — planner hints come later).
pub fn extract_shard_key(sql: &str) -> u64 {
    let trimmed = sql.trim();
    let up = trimmed.to_ascii_uppercase();

    if let Some(rest) = up.strip_prefix("SET QM.SHARD_KEY") {
        if let Some(num) = rest
            .split('=')
            .nth(1)
            .and_then(|s| s.trim().trim_end_matches(';').parse::<u64>().ok())
        {
            return num;
        }
    }

    for token in [" WHERE ID = ", " WHERE ID=", " VALUES (", " VALUES("] {
        if let Some(idx) = up.find(token) {
            let tail = &trimmed[idx + token.len()..];
            let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<u64>() {
                return n;
            }
        }
    }

    let mut h: u64 = 0x9e37_79b9_7f4a_7c15;
    for b in trimmed.as_bytes().iter().take(64) {
        h = h.wrapping_mul(0x100000001b3).wrapping_add(u64::from(*b));
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_from_where_id() {
        assert_eq!(extract_shard_key("SELECT * FROM users WHERE id = 42"), 42);
    }
}
