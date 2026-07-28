pub(crate) fn fuzzy_match_score(query: &str, candidate: &str) -> Option<u32> {
    let candidate = candidate.to_lowercase();
    let mut matched_any = false;
    let mut total = 0_u32;
    for token in query.split_whitespace().map(str::to_lowercase) {
        if token.is_empty() {
            continue;
        }
        matched_any = true;
        total = total.checked_add(fuzzy_token_score(&token, &candidate)?)?;
    }
    matched_any.then_some(total)
}

pub(crate) fn fuzzy_token_score(token: &str, candidate: &str) -> Option<u32> {
    if let Some(position) = candidate.find(token) {
        return u32::try_from(
            position.saturating_mul(2) + candidate.len().saturating_sub(token.len()),
        )
        .ok();
    }

    let wanted: Vec<_> = token.chars().collect();
    let mut next = 0;
    let mut start = None;
    let mut previous = None;
    let mut gaps = 0_usize;
    for (index, character) in candidate.chars().enumerate() {
        if wanted.get(next) != Some(&character) {
            continue;
        }
        start.get_or_insert(index);
        if let Some(previous) = previous {
            gaps = gaps.saturating_add(index.saturating_sub(previous + 1));
        }
        previous = Some(index);
        next += 1;
        if next == wanted.len() {
            let score = 100_usize
                .saturating_add(start.unwrap_or_default())
                .saturating_add(gaps.saturating_mul(4))
                .saturating_add(candidate.chars().count());
            return u32::try_from(score).ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_field_search_matches_tokens_and_ranks_tighter_paths_first() {
        let tight = fuzzy_match_score("gps lat", "GPS › latitude").unwrap();
        let loose = fuzzy_match_score("gps lat", "MyGpsTopic › vehicle_latitude_raw").unwrap();
        assert!(tight < loose);
        assert!(fuzzy_match_score("gpt lat", "GPS › latitude").is_some());
        assert_eq!(fuzzy_match_score("gyro", "GPS › latitude"), None);
    }
}
