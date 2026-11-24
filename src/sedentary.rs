use crate::app::App;

pub(crate) const MIN_MINUTES: u32 = 15;
pub(crate) const MAX_MINUTES: u32 = 180;
pub(crate) const TITLE: &str = "久坐提醒";

#[derive(Default)]
pub(crate) struct Timer {
    since: Option<f64>,
    interval: u32,
}

impl Timer {
    pub(crate) fn started_at(&self) -> Option<f64> {
        self.since
    }

    fn tick(&mut self, now: f64, idle: Option<f64>, minutes: u32, gap: f32) -> bool {
        const BREAK_SECONDS: f64 = 300.0;

        let Some(idle) = idle.filter(|idle| (0.0..BREAK_SECONDS).contains(idle)) else {
            self.since = None;
            return false;
        };
        if self.interval != minutes || gap >= 10.0 {
            self.since = None;
            self.interval = minutes;
        }
        let since = *self.since.get_or_insert(now);
        if now - since < f64::from(minutes) * 60.0 || idle > 30.0 {
            return false;
        }
        self.since = Some(now);
        true
    }
}

impl App {
    pub(crate) fn tick_sedentary(&mut self, idle: Option<f64>, gap: f32) {
        let idle = idle.filter(|_| self.settings.sedentary_enabled && self.settings.notifications);
        if self
            .sedentary
            .tick(self.t, idle, self.settings.sedentary_minutes, gap)
        {
            self.alert(TITLE, "起来走动一下，活动肩颈和双腿", usize::MAX, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminds_during_activity_and_restarts_after_a_break() {
        let mut timer = Timer::default();
        assert!(!timer.tick(0.0, Some(0.0), 15, 0.0));
        assert!(!timer.tick(899.0, Some(0.0), 15, 1.0));
        assert!(!timer.tick(900.0, Some(31.0), 15, 1.0));
        assert!(timer.tick(901.0, Some(0.0), 15, 1.0));
        assert!(!timer.tick(902.0, Some(0.0), 15, 1.0));
        assert!(!timer.tick(1300.0, Some(301.0), 15, 1.0));
        assert!(!timer.tick(1500.0, Some(0.0), 15, 1.0));
        assert!(!timer.tick(2399.0, Some(0.0), 15, 1.0));
        assert!(timer.tick(2400.0, Some(0.0), 15, 1.0));
        assert!(!timer.tick(3400.0, Some(0.0), 15, 1000.0));
        assert!(!timer.tick(4300.0, Some(0.0), 30, 1.0));
        assert!(!timer.tick(6100.0, None, 30, 1.0));
        assert!(!timer.tick(6101.0, Some(0.0), 30, 1.0));
    }
}
