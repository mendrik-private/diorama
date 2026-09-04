#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Tool {
    #[default]
    None,
    Pencil,
    Highlight,
    Arrow,
    Measure,
    Text,
    PickColor,
    Select,
    Scale,
}

impl Tool {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pencil => "pencil",
            Self::Highlight => "highlight",
            Self::Arrow => "arrow",
            Self::Measure => "measure",
            Self::Text => "text",
            Self::PickColor => "pick-color",
            Self::Select => "select",
            Self::Scale => "scale",
        }
    }

    pub(super) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "none" => Self::None,
            "pencil" => Self::Pencil,
            "highlight" => Self::Highlight,
            "arrow" => Self::Arrow,
            "measure" => Self::Measure,
            "text" => Self::Text,
            "pick-color" => Self::PickColor,
            "select" => Self::Select,
            "scale" => Self::Scale,
            _ => return None,
        })
    }

    pub(super) const fn is_annotation(self) -> bool {
        matches!(
            self,
            Self::Pencil | Self::Highlight | Self::Arrow | Self::Measure | Self::Text
        )
    }

    pub(super) const fn is_vector_annotation(self) -> bool {
        matches!(
            self,
            Self::Highlight | Self::Arrow | Self::Measure | Self::Text
        )
    }
}

pub(super) const fn palette_visible(tool: Tool, return_tool: Option<Tool>) -> bool {
    tool.is_annotation() || matches!(tool, Tool::PickColor) && return_tool.is_some()
}

pub(super) const fn pencil_drag_available(annotation_hit: bool) -> bool {
    !annotation_hit
}

pub(super) const fn resting_tool(requested: Tool, editable: bool) -> Tool {
    if editable && matches!(requested, Tool::None) {
        Tool::Select
    } else {
        requested
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for tool in [
            Tool::None,
            Tool::Pencil,
            Tool::Highlight,
            Tool::Arrow,
            Tool::Measure,
            Tool::Text,
            Tool::PickColor,
            Tool::Select,
            Tool::Scale,
        ] {
            assert_eq!(Tool::from_name(tool.name()), Some(tool));
        }
    }

    #[test]
    fn palette_visibility_covers_submode_and_modal_tools() {
        assert!(palette_visible(Tool::Pencil, None));
        assert!(palette_visible(Tool::Highlight, None));
        assert!(palette_visible(Tool::PickColor, Some(Tool::Arrow)));
        assert!(!palette_visible(Tool::PickColor, None));
        assert!(!palette_visible(Tool::Select, None));
        assert!(!palette_visible(Tool::Scale, None));
        assert!(!palette_visible(Tool::None, None));
    }

    #[test]
    fn pencil_defers_to_existing_vector_annotations() {
        assert!(pencil_drag_available(false));
        assert!(!pencil_drag_available(true));
    }

    #[test]
    fn select_is_the_resting_tool_for_editable_images() {
        assert_eq!(resting_tool(Tool::None, true), Tool::Select);
        assert_eq!(resting_tool(Tool::None, false), Tool::None);
        assert_eq!(resting_tool(Tool::Pencil, true), Tool::Pencil);
    }
}
