use super::detection::Detection;

pub fn deduplicate(mut detections: Vec<Detection>) -> Vec<Detection> {
    // Sort by span length descending so longest spans are processed first (longest-span-wins).
    // Use start ascending as tiebreaker for deterministic output.
    detections.sort_by(|a, b| {
        let len_a = a.end.saturating_sub(a.start);
        let len_b = b.end.saturating_sub(b.start);
        len_b.cmp(&len_a).then(a.start.cmp(&b.start))
    });

    let mut kept: Vec<Detection> = Vec::new();
    for det in detections {
        let overlaps = kept.iter().any(|k| det.start < k.end && det.end > k.start);
        if !overlaps {
            kept.push(det);
        }
    }

    // Re-sort kept spans by start position for stable output order.
    kept.sort_by_key(|d| d.start);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::detection::DetectionLayer;
    use super::super::entity_category::EntityCategory;

    fn make_det(start: usize, end: usize) -> Detection {
        Detection {
            value:      "x".to_string(),
            category:   EntityCategory::Per,
            start,
            end,
            confidence: 1.0,
            layer:      DetectionLayer::L1Regex,
        }
    }

    #[test]
    fn short_inside_long_only_long_kept() {
        let detections = vec![
            make_det(0, 10),
            make_det(2, 5),
        ];
        let result = deduplicate(detections);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].start, 0);
        assert_eq!(result[0].end, 10);
    }

    #[test]
    fn same_start_long_wins_over_short() {
        let detections = vec![
            make_det(0, 3),
            make_det(0, 10),
        ];
        let result = deduplicate(detections);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].end, 10);
    }

    #[test]
    fn non_overlapping_both_kept() {
        let detections = vec![
            make_det(0, 5),
            make_det(10, 15),
        ];
        let result = deduplicate(detections);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn empty_input_empty_output() {
        let result = deduplicate(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_longer_span_wins_when_shorter_has_lower_start() {
        // (0,3) is shorter, (1,10) is longer — longer must win even though shorter starts earlier
        let detections = vec![
            make_det(0, 3),
            make_det(1, 10),
        ];
        let result = deduplicate(detections);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].start, 1);
        assert_eq!(result[0].end, 10);
    }
}
