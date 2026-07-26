#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayArea {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WidgetBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub fn clamp_widget_bounds(bounds: WidgetBounds, display: DisplayArea) -> WidgetBounds {
    let width = bounds.width.min(display.width);
    let height = bounds.height.min(display.height);
    WidgetBounds {
        x: bounds.x.clamp(display.x, display.x + display.width - width),
        y: bounds
            .y
            .clamp(display.y, display.y + display.height - height),
        width,
        height,
    }
}

pub fn compact_bottom_right_bounds(
    height: f64,
    width: f64,
    work_area: DisplayArea,
    display: DisplayArea,
) -> WidgetBounds {
    clamp_widget_bounds(
        WidgetBounds {
            x: work_area.x + work_area.width - width,
            y: work_area.y + work_area.height - height,
            width,
            height,
        },
        display,
    )
}

pub fn compact_bottom_left_bounds(
    height: f64,
    width: f64,
    work_area: DisplayArea,
    display: DisplayArea,
) -> WidgetBounds {
    clamp_widget_bounds(
        WidgetBounds {
            x: work_area.x,
            y: work_area.y + work_area.height - height,
            width,
            height,
        },
        display,
    )
}

pub fn compact_taskbar_bounds(
    height: f64,
    width: f64,
    display: DisplayArea,
    work_area: DisplayArea,
    tray: WidgetBounds,
) -> Option<WidgetBounds> {
    let is_bottom_taskbar = work_area.x == display.x
        && work_area.y == display.y
        && work_area.width == display.width
        && work_area.height < display.height;
    if !is_bottom_taskbar {
        return None;
    }
    let strip_top = work_area.y + work_area.height;
    let strip_height = display.y + display.height - strip_top;
    if tray.x < display.x || tray.x > display.x + display.width || strip_height <= 0.0 {
        return None;
    }
    let y = if height <= strip_height {
        strip_top + ((strip_height - height) / 2.0).round()
    } else {
        display.y + display.height - height
    };
    Some(clamp_widget_bounds(
        WidgetBounds {
            x: tray.x - 4.0 - width,
            y,
            width,
            height,
        },
        display,
    ))
}

pub fn taskbar_theme_is_light(
    system_uses_light_theme: i64,
    color_prevalence: i64,
    accent_color: u32,
) -> Option<bool> {
    if !matches!(system_uses_light_theme, 0 | 1) || !matches!(color_prevalence, 0 | 1) {
        return None;
    }
    let (red, green, blue) = if color_prevalence == 1 {
        let darken = |channel: u32| f64::from(channel).mul_add(0.82, 0.0).round();
        (
            darken(accent_color & 0xff),
            darken((accent_color >> 8) & 0xff),
            darken((accent_color >> 16) & 0xff),
        )
    } else if system_uses_light_theme == 1 {
        (243.0, 243.0, 243.0)
    } else {
        (32.0, 32.0, 32.0)
    };
    let linear = |channel: f64| {
        let normalized = channel / 255.0;
        if normalized <= 0.03928 {
            normalized / 12.92
        } else {
            ((normalized + 0.055) / 1.055).powf(2.4)
        }
    };
    Some(0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue) > 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY: DisplayArea = DisplayArea {
        x: 0.0,
        y: 0.0,
        width: 1_920.0,
        height: 1_080.0,
    };
    const WORK_AREA: DisplayArea = DisplayArea {
        x: 0.0,
        y: 0.0,
        width: 1_920.0,
        height: 1_040.0,
    };

    #[test]
    fn compact_bottom_right_is_flush_with_work_area() {
        let bounds = compact_bottom_right_bounds(70.0, 280.0, WORK_AREA, DISPLAY);

        assert_eq!(
            bounds,
            WidgetBounds {
                x: 1_640.0,
                y: 970.0,
                width: 280.0,
                height: 70.0,
            }
        );
    }

    #[test]
    fn compact_taskbar_centers_before_notification_area() {
        let bounds = compact_taskbar_bounds(
            70.0,
            280.0,
            DISPLAY,
            WORK_AREA,
            WidgetBounds {
                x: 1_700.0,
                y: 1_040.0,
                width: 220.0,
                height: 40.0,
            },
        );

        assert_eq!(
            bounds,
            Some(WidgetBounds {
                x: 1_416.0,
                y: 1_010.0,
                width: 280.0,
                height: 70.0,
            })
        );
    }

    #[test]
    fn compact_taskbar_rejects_side_taskbar() {
        let side_work_area = DisplayArea {
            x: 40.0,
            y: 0.0,
            width: 1_880.0,
            height: 1_080.0,
        };

        assert_eq!(
            compact_taskbar_bounds(
                38.0,
                280.0,
                DISPLAY,
                side_work_area,
                WidgetBounds {
                    x: 1_700.0,
                    y: 1_040.0,
                    width: 220.0,
                    height: 40.0,
                },
            ),
            None
        );
    }

    #[test]
    fn accent_theme_uses_abgr_channels_and_darken_factor() {
        assert_eq!(taskbar_theme_is_light(0, 1, 0xfff0f0f0), Some(true));
        assert_eq!(taskbar_theme_is_light(0, 1, 0xff202020), Some(false));
    }

    #[test]
    fn taskbar_theme_rejects_invalid_registry_values() {
        assert_eq!(taskbar_theme_is_light(2, 0, 0), None);
        assert_eq!(taskbar_theme_is_light(0, 2, 0), None);
    }
}
