//! `-adaptation_sets`'s `"id=0,streams=0,1,2 id=1,streams=3,4"` grouping
//! syntax (measured against `ffmpeg -h muxer=dash`).

/// Parse `spec` into groups of stream indices, one group per `id=...`
/// clause, in the order named. `None` (the option was not given) or a
/// `spec` naming no streams at all falls back to one group per stream
/// (`0..n_streams`, each alone) — the shape a manifest with no explicit
/// grouping should have: every stream individually switchable.
#[must_use]
pub fn parse_adaptation_sets(spec: Option<&str>, n_streams: usize) -> Vec<Vec<u32>> {
    let default = || (0..n_streams as u32).map(|i| vec![i]).collect();
    let Some(spec) = spec else {
        return default();
    };
    // Each whitespace-separated clause is `id=N,streams=A,B,C`: the
    // `streams=` list is itself comma-joined, so it cannot be split apart
    // from `id=N` by a naive `clause.split(',')` — everything from
    // `streams=` to the end of the clause is the list.
    let mut groups = Vec::new();
    for clause in spec.split_whitespace() {
        let Some(list) = clause
            .find("streams=")
            .and_then(|i| clause.get(i + "streams=".len()..))
        else {
            continue;
        };
        let streams: Vec<u32> = list
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        if !streams.is_empty() {
            groups.push(streams);
        }
    }
    if groups.is_empty() { default() } else { groups }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_spec_is_one_group_per_stream() {
        assert_eq!(
            parse_adaptation_sets(None, 3),
            vec![vec![0], vec![1], vec![2]]
        );
    }

    #[test]
    fn parses_explicit_groups() {
        let groups = parse_adaptation_sets(Some("id=0,streams=0,1 id=1,streams=2"), 3);
        assert_eq!(groups, vec![vec![0, 1], vec![2]]);
    }

    #[test]
    fn a_spec_naming_no_streams_falls_back_to_the_default() {
        assert_eq!(
            parse_adaptation_sets(Some("id=0"), 2),
            vec![vec![0], vec![1]]
        );
    }
}
