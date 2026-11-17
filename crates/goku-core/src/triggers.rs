use crate::types::TriggerKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TriggerDecision {
    pub fire: Option<TriggerKind>,
    pub dropped: bool,
    pub pending: Option<TriggerKind>,
}

pub fn consider(
    pending: Option<TriggerKind>,
    incoming: TriggerKind,
    elapsed_ms: u64,
    min_interval_ms: u64,
) -> TriggerDecision {
    let chosen = match pending {
        Some(held) if priority(held) >= priority(incoming) => held,
        _ => incoming,
    };
    let dropped = pending.is_some();
    if elapsed_ms < min_interval_ms {
        TriggerDecision {
            fire: None,
            dropped,
            pending: Some(chosen),
        }
    } else {
        TriggerDecision {
            fire: Some(chosen),
            dropped,
            pending: None,
        }
    }
}

fn priority(kind: TriggerKind) -> u8 {
    match kind {
        TriggerKind::Manual => 7,
        TriggerKind::AppSwitch => 6,
        TriggerKind::WindowFocus => 5,
        TriggerKind::Click => 4,
        TriggerKind::TypingPause => 3,
        TriggerKind::ScrollStop => 2,
        TriggerKind::Idle => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_interval_does_not_fire_and_keeps_the_higher_event() {
        let held = consider(None, TriggerKind::Idle, 0, 750);
        assert_eq!(held.fire, None);
        assert!(!held.dropped);
        assert_eq!(held.pending, Some(TriggerKind::Idle));

        let upgraded = consider(held.pending, TriggerKind::Click, 100, 750);
        assert_eq!(upgraded.fire, None);
        assert!(upgraded.dropped);
        assert_eq!(upgraded.pending, Some(TriggerKind::Click));

        let kept = consider(Some(TriggerKind::Manual), TriggerKind::Idle, 0, 750);
        assert_eq!(kept.fire, None);
        assert_eq!(kept.pending, Some(TriggerKind::Manual));
        assert!(kept.dropped);
    }

    #[test]
    fn elapsed_interval_fires_the_higher_event_and_drops_the_other() {
        let decision = consider(Some(TriggerKind::AppSwitch), TriggerKind::Manual, 750, 750);
        assert_eq!(decision.fire, Some(TriggerKind::Manual));
        assert!(decision.dropped);
        assert_eq!(decision.pending, None);

        let idle = consider(Some(TriggerKind::Manual), TriggerKind::Idle, 800, 750);
        assert_eq!(idle.fire, Some(TriggerKind::Manual));
        assert!(idle.dropped);
    }

    #[test]
    fn priority_is_manual_then_app_switch_window_focus_click_typing_scroll_idle() {
        let order = [
            TriggerKind::Manual,
            TriggerKind::AppSwitch,
            TriggerKind::WindowFocus,
            TriggerKind::Click,
            TriggerKind::TypingPause,
            TriggerKind::ScrollStop,
            TriggerKind::Idle,
        ];
        for pair in order.windows(2) {
            let higher = pair[0];
            let lower = pair[1];
            let deferred = consider(Some(lower), higher, 0, 100);
            assert_eq!(deferred.fire, None);
            assert_eq!(deferred.pending, Some(higher));
            let fired = consider(Some(lower), higher, 100, 100);
            assert_eq!(fired.fire, Some(higher));
            assert!(fired.dropped);
        }
    }

    #[test]
    fn a_duplicate_inside_the_interval_stays_pending_once() {
        let decision = consider(Some(TriggerKind::Click), TriggerKind::Click, 10, 750);
        assert_eq!(decision.fire, None);
        assert!(decision.dropped);
        assert_eq!(decision.pending, Some(TriggerKind::Click));
    }

    #[test]
    fn the_first_event_fires_when_the_interval_has_already_elapsed() {
        let decision = consider(None, TriggerKind::WindowFocus, 750, 750);
        assert_eq!(decision.fire, Some(TriggerKind::WindowFocus));
        assert!(!decision.dropped);
        assert_eq!(decision.pending, None);
    }
}
