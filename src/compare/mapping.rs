#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizedPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitOrientation {
    Vertical,
    Horizontal,
}

pub fn map_corresponding_point(
    source_point: (f64, f64),
    source_dimensions: (u32, u32),
    target_dimensions: (u32, u32),
) -> Option<(f64, f64)> {
    if source_dimensions.0 == 0
        || source_dimensions.1 == 0
        || target_dimensions.0 == 0
        || target_dimensions.1 == 0
    {
        return None;
    }
    let normalized = NormalizedPoint {
        x: (source_point.0 / f64::from(source_dimensions.0)).clamp(0.0, 1.0),
        y: (source_point.1 / f64::from(source_dimensions.1)).clamp(0.0, 1.0),
    };
    Some((
        normalized.x * f64::from(target_dimensions.0),
        normalized.y * f64::from(target_dimensions.1),
    ))
}

pub fn choose_split(first: (u32, u32), second: (u32, u32)) -> SplitOrientation {
    let landscape_count = u8::from(first.0 >= first.1) + u8::from(second.0 >= second.1);
    if landscape_count == 2 {
        return SplitOrientation::Vertical;
    }
    if landscape_count == 0 {
        return SplitOrientation::Horizontal;
    }
    let vertical_scale = fit_score(first, 0.5, 1.0) + fit_score(second, 0.5, 1.0);
    let horizontal_scale = fit_score(first, 1.0, 0.5) + fit_score(second, 1.0, 0.5);
    if vertical_scale >= horizontal_scale {
        SplitOrientation::Vertical
    } else {
        SplitOrientation::Horizontal
    }
}

fn fit_score(dimensions: (u32, u32), available_width: f64, available_height: f64) -> f64 {
    if dimensions.0 == 0 || dimensions.1 == 0 {
        return 0.0;
    }
    let scale =
        (available_width / f64::from(dimensions.0)).min(available_height / f64::from(dimensions.1));
    f64::from(dimensions.0) * f64::from(dimensions.1) * scale * scale
}

#[cfg(test)]
mod tests {
    use super::{SplitOrientation, choose_split, map_corresponding_point};

    #[test]
    fn maps_unequal_dimensions_by_normalized_coordinates() {
        assert_eq!(
            map_corresponding_point((50.0, 25.0), (100, 50), (400, 200)),
            Some((200.0, 100.0))
        );
    }

    #[test]
    fn landscapes_use_vertical_split() {
        assert_eq!(
            choose_split((1600, 900), (1200, 800)),
            SplitOrientation::Vertical
        );
    }
}
