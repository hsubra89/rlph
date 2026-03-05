//! Newtype wrappers for domain IDs: [`IssueNumber`], [`PrNumber`], [`CommentId`], [`ReactionId`].

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
            serde::Serialize, serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            pub fn new(value: u64) -> Self {
                Self(value)
            }

            pub fn get(self) -> u64 {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = std::num::ParseIntError;
            fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
                s.parse::<u64>().map(Self)
            }
        }

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }

        impl From<$name> for u64 {
            fn from(id: $name) -> u64 {
                id.0
            }
        }
    };
}

define_id!(
    /// A GitHub issue number.
    IssueNumber
);

define_id!(
    /// A GitHub pull request number.
    PrNumber
);

define_id!(
    /// A GitHub comment ID (issue comment or review comment).
    CommentId
);

define_id!(
    /// A GitHub reaction ID.
    ReactionId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display() {
        assert_eq!(IssueNumber::new(42).to_string(), "42");
        assert_eq!(PrNumber::new(7).to_string(), "7");
        assert_eq!(CommentId::new(123).to_string(), "123");
        assert_eq!(ReactionId::new(321).to_string(), "321");
    }

    #[test]
    fn test_from_str() {
        assert_eq!("42".parse::<IssueNumber>().unwrap(), IssueNumber::new(42));
        assert!("abc".parse::<PrNumber>().is_err());
    }

    #[test]
    fn test_from_u64() {
        let id: CommentId = 99u64.into();
        let raw: u64 = id.into();
        assert_eq!(raw, 99);
    }

    #[test]
    fn test_serde_transparent() {
        let id = PrNumber::new(10);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "10");
        let parsed: PrNumber = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn test_get() {
        assert_eq!(IssueNumber::new(5).get(), 5);
    }
}
