//! ISBN normalisation and check digits (REQ-36a, REQ-36b).

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IsbnError {
    #[error("wrong length")]
    Length,
    #[error("bad character")]
    Character,
    #[error("failed check digit")]
    CheckDigit,
}

/// Strips spaces and hyphens, upper-cases a final X, and validates the result.
pub fn normalize(input: &str) -> Result<String, IsbnError> {
    let cleaned: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();

    match cleaned.len() {
        10 => check_10(&cleaned)?,
        13 => check_13(&cleaned)?,
        _ => return Err(IsbnError::Length),
    }
    Ok(cleaned)
}

fn check_10(isbn: &str) -> Result<(), IsbnError> {
    let mut sum = 0u32;
    for (position, c) in isbn.chars().enumerate() {
        let value = match c {
            'X' if position == 9 => 10,
            _ => c.to_digit(10).ok_or(IsbnError::Character)?,
        };
        sum += (10 - position as u32) * value;
    }
    if sum % 11 == 0 { Ok(()) } else { Err(IsbnError::CheckDigit) }
}

fn check_13(isbn: &str) -> Result<(), IsbnError> {
    let mut sum = 0u32;
    for (position, c) in isbn.chars().enumerate() {
        let value = c.to_digit(10).ok_or(IsbnError::Character)?;
        sum += if position % 2 == 0 { value } else { value * 3 };
    }
    if sum % 10 == 0 { Ok(()) } else { Err(IsbnError::CheckDigit) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vectors from Appendix F.
    #[test]
    fn appendix_f_vectors() {
        assert_eq!(normalize("978-0-14-032872-1").as_deref(), Ok("9780140328721"));
        assert_eq!(normalize("9780140328722"), Err(IsbnError::CheckDigit));
        assert_eq!(normalize("0 14 032872 6").as_deref(), Ok("0140328726"));
        assert_eq!(normalize("0140328727"), Err(IsbnError::CheckDigit));
        assert_eq!(normalize("080442957x").as_deref(), Ok("080442957X"));
        assert_eq!(normalize("12345"), Err(IsbnError::Length));
    }

    #[test]
    fn x_is_only_valid_as_the_last_character_of_isbn_10() {
        assert_eq!(normalize("X804429570"), Err(IsbnError::Character));
        assert_eq!(normalize("978014032872X"), Err(IsbnError::Character));
    }
}
