fn check(plural: &str) {
    debug_assert!(
        !plural.ends_with('.'),
        "an empty-state noun must not carry a trailing period: {plural:?}"
    );
    debug_assert!(
        plural.chars().all(|c| !c.is_uppercase()),
        "an empty-state noun must be lowercase: {plural:?}"
    );
}

pub fn no_saved(plural: &str) -> String {
    check(plural);
    format!("No saved {plural}")
}

pub fn no_items(plural: &str) -> String {
    check(plural);
    format!("No {plural}")
}

pub fn no_matching(plural: &str) -> String {
    check(plural);
    format!("No matching {plural}")
}

#[cfg(test)]
mod tests {
    use super::{no_items, no_matching, no_saved};

    #[test]
    fn saved_text_names_the_plural_type() {
        assert_eq!(no_saved("layouts"), "No saved layouts");
        assert_eq!(no_saved("sequences"), "No saved sequences");
    }

    #[test]
    fn absent_text_drops_the_saved_qualifier() {
        assert_eq!(no_items("topics"), "No topics");
    }

    #[test]
    fn matching_text_covers_a_list_the_filter_emptied() {
        assert_eq!(no_matching("fields"), "No matching fields");
    }

    #[test]
    #[should_panic(expected = "lowercase")]
    fn a_capitalised_noun_is_rejected() {
        let _ = no_saved("Layouts");
    }

    #[test]
    #[should_panic(expected = "trailing")]
    fn a_trailing_period_is_rejected() {
        let _ = no_items("topics.");
    }
}
