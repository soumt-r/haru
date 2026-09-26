//! Release numbers like 1.2.3 (a leading "v", as git tags have it, is
//! dropped). No pre-release or build suffixes: a tag is a version or it is not.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    pub fn parse(s: &str) -> Result<Version, String> {
        let bad = || format!("{s:?} is not a version like 1.2.3");
        let t = s.strip_prefix('v').unwrap_or(s);
        let parts: Vec<&str> = t.split('.').collect();
        if parts.len() != 3 {
            return Err(bad());
        }
        let mut n = [0u64; 3];
        for (i, p) in parts.iter().enumerate() {
            let ok = !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()) && !(p.len() > 1 && p.starts_with('0'));
            if !ok {
                return Err(bad());
            }
            n[i] = p.parse().map_err(|_| bad())?;
        }
        Ok(Version(n[0], n[1], n[2]))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[cfg(test)]
mod tests {
    use super::Version;

    #[test]
    fn versions() {
        assert_eq!(Version::parse("v1.2.3"), Ok(Version(1, 2, 3)));
        assert!(Version::parse("1.02.3").is_err());
        assert!(Version::parse("1.2").is_err());
        assert!(Version::parse("1.2.3-rc1").is_err());
        assert!(Version(1, 10, 0) > Version(1, 9, 9));
    }
}
