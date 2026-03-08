//! Core task and priority types shared across the crate boundary.

use serde::Serialize;

/// Task priority (1 = highest, 9 = lowest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Priority(u8);

impl Priority {
    pub fn new(value: u8) -> Self {
        Self(value)
    }

    pub fn get(self) -> u8 {
        self.0
    }

    /// Parse priority from a label string.
    /// Recognizes: p1-p9, priority-high, priority-medium, priority-low.
    pub fn from_label(label: &str) -> Option<Self> {
        if label.eq_ignore_ascii_case("priority-high") {
            return Some(Self(1));
        }

        if label.eq_ignore_ascii_case("priority-medium") {
            return Some(Self(5));
        }

        if label.eq_ignore_ascii_case("priority-low") {
            return Some(Self(9));
        }

        let [first, second] = label.as_bytes() else {
            return None;
        };

        if first.eq_ignore_ascii_case(&b'p') && second.is_ascii_digit() {
            let value = second - b'0';
            return (1..=9).contains(&value).then_some(Self(value));
        }

        None
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub url: String,
    pub priority: Option<Priority>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_from_numeric_labels() {
        assert_eq!(Priority::from_label("p1"), Some(Priority::new(1)));
        assert_eq!(Priority::from_label("p5"), Some(Priority::new(5)));
        assert_eq!(Priority::from_label("p9"), Some(Priority::new(9)));
    }

    #[test]
    fn test_priority_from_named_labels() {
        assert_eq!(
            Priority::from_label("priority-high"),
            Some(Priority::new(1))
        );
        assert_eq!(
            Priority::from_label("priority-medium"),
            Some(Priority::new(5))
        );
        assert_eq!(Priority::from_label("priority-low"), Some(Priority::new(9)));
    }

    #[test]
    fn test_priority_case_insensitive() {
        assert_eq!(Priority::from_label("P1"), Some(Priority::new(1)));
        assert_eq!(
            Priority::from_label("Priority-High"),
            Some(Priority::new(1))
        );
        assert_eq!(Priority::from_label("PRIORITY-LOW"), Some(Priority::new(9)));
    }

    #[test]
    fn test_priority_invalid() {
        assert_eq!(Priority::from_label("p0"), None);
        assert_eq!(Priority::from_label("p10"), None);
        assert_eq!(Priority::from_label("bug"), None);
        assert_eq!(Priority::from_label(""), None);
    }
}
