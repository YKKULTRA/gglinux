//! ChatMix: one balance control across the Game and Chat channels.

/// Gains for the (Game, Chat) outlets from a balance of -1.0 (all Game) through
/// 0.0 (both full) to 1.0 (all Chat). As on the hardware dial, the favoured side
/// stays at full volume and only the other side is turned down.
pub fn chatmix_gains(balance: f32) -> (f32, f32) {
    let b = if balance.is_finite() { balance.clamp(-1.0, 1.0) } else { 0.0 };
    (1.0 - b.max(0.0), 1.0 + b.min(0.0))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centre_leaves_both_full() {
        assert_eq!(chatmix_gains(0.0), (1.0, 1.0));
    }

    #[test]
    fn the_favoured_side_stays_full() {
        assert_eq!(chatmix_gains(-1.0), (1.0, 0.0));
        assert_eq!(chatmix_gains(1.0), (0.0, 1.0));
        assert_eq!(chatmix_gains(0.5), (0.5, 1.0));
        assert_eq!(chatmix_gains(-0.25), (1.0, 0.75));
    }

    #[test]
    fn out_of_range_and_nan_are_tamed() {
        assert_eq!(chatmix_gains(3.0), (0.0, 1.0));
        assert_eq!(chatmix_gains(f32::NAN), (1.0, 1.0));
    }
}
