use std::io::Read;

#[derive(Debug)]
pub(crate) enum ReadError {
    Io,
    TooLarge,
}

/// Read complete bytes, accepting the limit inclusively. One extra byte detects
/// overflow without consuming the rest of an oversized input.
pub(crate) fn read_bounded(reader: impl Read, limit: u64) -> Result<Vec<u8>, ReadError> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ReadError::Io)?;
    if bytes.len() as u64 > limit {
        return Err(ReadError::TooLarge);
    }
    Ok(bytes)
}
