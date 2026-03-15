use super::detection::Detection;

pub fn deduplicate(mut detections: Vec<Detection>) -> Vec<Detection> {
    // Sort: (start asc, length desc) — longest span first within same start position
    detections.sort_by_key(|d| (d.start, usize::MAX - (d.end - d.start)));
    let mut result: Vec<Detection> = Vec::new();
    for det in detections {
        let overlaps = result.iter().any(|kept| {
            det.start < kept.end && det.end > kept.start
        });
        if !overlaps {
            result.push(det);
        }
    }
    result
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
}
