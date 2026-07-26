use std::collections::{HashMap, VecDeque};

pub const TOAST_WIDTH: i32 = 360;
pub const MIN_HEIGHT: i32 = 84;
pub const DEFAULT_HEIGHT: i32 = 112;
pub const MAX_HEIGHT: i32 = 160;
pub const VISIBLE_LIMIT: usize = 4;
const STACK_GAP: i32 = 10;
const WORK_AREA_MARGIN: i32 = 18;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppNotifyPayload {
    pub title: String,
    pub body: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ToastWindowId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkArea {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}
pub trait ToastWindowFactory {
    fn create(&mut self, payload: &AppNotifyPayload) -> Result<ToastWindowId, String>;
    fn is_destroyed(&self, id: ToastWindowId) -> bool;
}
#[derive(Debug)]
struct ActiveToast {
    id: ToastWindowId,
    height: i32,
}
#[derive(Debug, Default)]
pub struct ToastState {
    payloads: HashMap<ToastWindowId, AppNotifyPayload>,
    active: Vec<ActiveToast>,
    queue: VecDeque<AppNotifyPayload>,
}
pub fn clamp_height(height: f64) -> i32 {
    if !height.is_finite() {
        return DEFAULT_HEIGHT;
    }
    (height.ceil() as i32).clamp(MIN_HEIGHT, MAX_HEIGHT)
}
pub fn toast_bounds(heights: &[i32], work_area: WorkArea) -> Vec<Bounds> {
    let x = work_area.x + work_area.width - TOAST_WIDTH - WORK_AREA_MARGIN;
    let mut y = work_area.y + work_area.height - WORK_AREA_MARGIN;
    heights
        .iter()
        .map(|height| {
            y -= height;
            let bounds = Bounds {
                x,
                y,
                width: TOAST_WIDTH,
                height: *height,
            };
            y -= STACK_GAP;
            bounds
        })
        .collect()
}
impl ToastState {
    pub fn show(
        &mut self,
        payload: AppNotifyPayload,
        app_ready: bool,
        app_quitting: bool,
        windows: &mut impl ToastWindowFactory,
    ) {
        self.prune_destroyed(windows);
        if app_quitting {
            return;
        }
        if !app_ready || self.active.len() >= VISIBLE_LIMIT {
            self.queue.push_back(payload);
            return;
        }
        self.show_now(payload, windows);
    }

    pub fn drain(&mut self, app_quitting: bool, windows: &mut impl ToastWindowFactory) {
        self.prune_destroyed(windows);
        if app_quitting {
            self.queue.clear();
            return;
        }
        while self.active.len() < VISIBLE_LIMIT {
            let Some(payload) = self.queue.pop_front() else {
                break;
            };
            self.show_now(payload, windows);
        }
    }

    pub fn remove(&mut self, id: u64, app_quitting: bool, windows: &mut impl ToastWindowFactory) {
        let id = ToastWindowId(id);
        self.payloads.remove(&id);
        self.active.retain(|toast| toast.id != id);
        self.drain(app_quitting, windows);
    }

    pub fn resize(&mut self, id: ToastWindowId, height: f64) {
        if let Some(toast) = self.active.iter_mut().find(|toast| toast.id == id) {
            toast.height = clamp_height(height);
        }
    }

    pub fn bounds(&self, work_area: WorkArea) -> Vec<(ToastWindowId, Bounds)> {
        let heights: Vec<i32> = self.active.iter().map(|toast| toast.height).collect();
        self.active
            .iter()
            .zip(toast_bounds(&heights, work_area))
            .map(|(toast, bounds)| (toast.id, bounds))
            .collect()
    }

    pub fn payload(&self, id: u64) -> Option<&AppNotifyPayload> {
        self.payloads.get(&ToastWindowId(id))
    }

    pub fn active_len(&self) -> usize {
        self.active.len()
    }

    pub fn queued_len(&self) -> usize {
        self.queue.len()
    }

    fn show_now(&mut self, payload: AppNotifyPayload, windows: &mut impl ToastWindowFactory) {
        let Ok(id) = windows.create(&payload) else {
            return;
        };
        self.payloads.insert(id, payload);
        self.active.push(ActiveToast {
            id,
            height: DEFAULT_HEIGHT,
        });
    }

    fn prune_destroyed(&mut self, windows: &impl ToastWindowFactory) {
        self.active.retain(|toast| {
            let keep = !windows.is_destroyed(toast.id);
            if !keep {
                self.payloads.remove(&toast.id);
            }
            keep
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_invalid_and_out_of_range_heights() {
        assert_eq!(clamp_height(f64::NAN), DEFAULT_HEIGHT);
        assert_eq!(clamp_height(12.0), MIN_HEIGHT);
        assert_eq!(clamp_height(120.1), 121);
        assert_eq!(clamp_height(999.0), MAX_HEIGHT);
    }

    #[test]
    fn stacks_newest_toasts_upward_from_bottom_right() {
        let work_area = WorkArea {
            x: 100,
            y: 50,
            width: 1000,
            height: 800,
        };
        let bounds = toast_bounds(&[112, 84], work_area);
        assert_eq!(
            bounds,
            vec![
                Bounds {
                    x: 722,
                    y: 720,
                    width: 360,
                    height: 112
                },
                Bounds {
                    x: 722,
                    y: 626,
                    width: 360,
                    height: 84
                },
            ]
        );
    }

    #[test]
    fn queues_above_visible_limit_and_drains_after_remove() {
        let mut state = ToastState::default();
        let mut windows = FakeWindows::default();
        for index in 0..=VISIBLE_LIMIT {
            state.show(payload(index), true, false, &mut windows);
        }
        state.remove(1, false, &mut windows);
        assert_eq!(state.active_len(), VISIBLE_LIMIT);
        assert_eq!(state.queued_len(), 0);
        assert_eq!(state.payload(5), Some(&payload(VISIBLE_LIMIT)));
    }

    #[test]
    fn destroyed_toast_is_pruned_before_queue_is_drained() {
        let mut state = ToastState::default();
        let mut windows = FakeWindows::default();
        for index in 0..=VISIBLE_LIMIT {
            state.show(payload(index), true, false, &mut windows);
        }
        windows.destroyed.push(ToastWindowId(1));
        state.drain(false, &mut windows);
        assert_eq!(state.active_len(), VISIBLE_LIMIT);
        assert_eq!(state.queued_len(), 0);
        assert_eq!(state.payload(1), None);
        assert_eq!(state.payload(5), Some(&payload(VISIBLE_LIMIT)));
    }

    #[derive(Default)]
    struct FakeWindows {
        next_id: u64,
        destroyed: Vec<ToastWindowId>,
    }

    impl ToastWindowFactory for FakeWindows {
        fn create(&mut self, _payload: &AppNotifyPayload) -> Result<ToastWindowId, String> {
            self.next_id += 1;
            Ok(ToastWindowId(self.next_id))
        }

        fn is_destroyed(&self, id: ToastWindowId) -> bool {
            self.destroyed.contains(&id)
        }
    }

    fn payload(index: usize) -> AppNotifyPayload {
        AppNotifyPayload {
            title: format!("title {index}"),
            body: format!("body {index}"),
        }
    }
}
