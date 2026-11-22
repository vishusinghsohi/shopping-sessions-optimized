pub const PARTITION_BUFFER_BYTES: usize = 256 * 1024;

pub fn parse_line<const COLUMNS: usize>(line: &[u8]) -> Option<[&[u8]; COLUMNS]> {
    if line.is_empty() {
        return None;
    }

    let mut fields = [b"".as_slice(); COLUMNS];
    let mut parts = line.split(|&b| b == b',');

    for field in &mut fields {
        let part = parts.next()?;
        *field = if part.first() == Some(&b'"') {
            // Quoted fields must have a closing quote.
            if part.len() < 2 || part.last() != Some(&b'"') {
                return None;
            }

            let inner = &part[1..part.len() - 1];

            if inner.contains(&b'"') {
                return None;
            }

            inner
        } else {
            // Quotes inside unquoted fields are invalid.
            if part.contains(&b'"') {
                return None;
            }

            part
        };
    }

    if parts.next().is_some() {
        return None;
    }

    Some(fields)
}

#[cfg(test)]
mod tests {
    use super::parse_line;

    #[test]
    fn preserves_spaces_and_carriage_returns() {
        let row = parse_line::<3>(b"e1,\" u1\",1000\r").unwrap();

        assert_eq!(row[0], b"e1");
        assert_eq!(row[1], b" u1");
        assert_eq!(row[2], b"1000\r");
    }

    #[test]
    fn handles_quoted_empty_values() {
        let row = parse_line::<2>(b"\"\",x").unwrap();
        assert_eq!(row[0], b"");
    }

    #[test]
    fn rejects_malformed_rows() {
        assert!(parse_line::<2>(b"").is_none());
        assert!(parse_line::<2>(b"u1").is_none());
        assert!(parse_line::<2>(b"u1,a,b").is_none());
        assert!(parse_line::<2>(b"u1,\"bad\"extra").is_none());
        assert!(parse_line::<2>(b"u1,\"bad").is_none());
        assert!(parse_line::<2>(b"u1,\"a,b\"").is_none());
    }
}
