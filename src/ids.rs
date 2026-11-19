//! Salesforce record ids: a 3-character key prefix, a 2-character pod, a
//! reserved character and a 9-character base-62 sequence (15 characters, case
//! sensitive), plus a 3-character checksum that makes the 18-character form
//! case-insensitive.

const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const SUFFIX: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ012345";

/// The pod characters every localforce id carries ("Lf" for localforce).
const POD: &str = "Lf";

/// Build the 18-character id for sequence number `seq` with the given key prefix.
pub fn make(prefix: &str, seq: u64) -> String {
    let mut digits = [b'0'; 9];
    let mut n = seq;
    for d in digits.iter_mut().rev() {
        *d = BASE62[(n % 62) as usize];
        n /= 62;
    }
    let id15 = format!("{prefix}{POD}0{}", std::str::from_utf8(&digits).unwrap());
    to18(&id15).expect("generated ids are well formed")
}

/// Normalise a 15- or 18-character id to 18 characters. `None` if it isn't an id.
pub fn to18(id: &str) -> Option<String> {
    if !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    match id.len() {
        15 => {
            let mut out = id.to_string();
            for chunk in id.as_bytes().chunks(5) {
                let mut flags = 0;
                for (i, b) in chunk.iter().enumerate() {
                    if b.is_ascii_uppercase() {
                        flags |= 1 << i;
                    }
                }
                out.push(SUFFIX[flags] as char);
            }
            Some(out)
        }
        // The checksum makes 18-character ids case-insensitive: restore the
        // case of the first 15 characters from it.
        18 => {
            let (base, suffix) = id.split_at(15);
            let mut fixed = String::with_capacity(18);
            for (chunk, s) in base.as_bytes().chunks(5).zip(suffix.bytes()) {
                let flags = SUFFIX.iter().position(|c| *c == s.to_ascii_uppercase())?;
                for (i, b) in chunk.iter().enumerate() {
                    let upper = flags & (1 << i) != 0;
                    fixed.push(if upper { b.to_ascii_uppercase() } else { b.to_ascii_lowercase() } as char);
                }
            }
            to18(&fixed)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_matches_salesforce() {
        // A real-world example pair.
        assert_eq!(to18("001A0000006Vm9r").unwrap(), "001A0000006Vm9rIAC");
        assert_eq!(to18("001a0000006vm9riac").unwrap(), "001A0000006Vm9rIAC");
    }

    #[test]
    fn generated_ids_round_trip() {
        let id = make("001", 12345);
        assert_eq!(id.len(), 18);
        assert_eq!(to18(&id[..15]).unwrap(), id);
    }
}
