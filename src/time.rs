use serde_with::DeserializeFromStr;
use std::ops::{Add, Mul};
use std::str::FromStr;

pub type InstantNanos = u64;
pub type InstantTicks = u64;

/// Represents a frequency, in Hz.
/// Hz = nom / denom
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, DeserializeFromStr)]
pub struct Rate {
    nom: u64,
    denom: u64,
}

impl FromStr for Rate {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err_msg = |input: &str| {
            format!("Invalid rate '{input}', use the supported format 'numerator/denominator' with valid components.")
        };
        let components: Vec<&str> = s.split('/').map(|c| c.trim()).collect();
        if components.len() != 2 || components.iter().any(|c| c.is_empty()) {
            return Err(err_msg(s));
        }
        let nom = components[0].parse::<u64>().ok();
        let denom = components[1].parse::<u64>().ok();
        match (nom, denom) {
            (Some(n), Some(d)) => Rate::new(n, d).ok_or_else(|| err_msg(s)),
            _ => Err(err_msg(s)),
        }
    }
}

impl Rate {
    pub fn new(nom: u64, denom: u64) -> Option<Self> {
        if nom == 0 || denom == 0 || (nom > denom) {
            None
        } else {
            Some(Self { nom, denom })
        }
    }

    pub fn numerator(&self) -> u64 {
        self.nom
    }

    pub fn denominator(&self) -> u64 {
        self.denom
    }
}

const NS_PER_SEC: u64 = 1_000_000_000;

// TODO - switch to checked arithmetic
impl Mul<InstantTicks> for Rate {
    type Output = InstantNanos;

    fn mul(self, rhs: InstantTicks) -> Self::Output {
        (self.nom * rhs * NS_PER_SEC) / self.denom
    }
}

/// Instant, in ticks, that tracks rollovers.
/// Supports u8, u16, and u32 ticks types.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct TrackingInstant<T: TicksExt> {
    lower: T,
    upper: u32,
}

impl<T> TrackingInstant<T>
where
    T: TicksExt,
{
    pub const fn zero() -> Self {
        Self {
            lower: T::ZERO,
            upper: 0,
        }
    }

    pub fn elapsed(&mut self, now: T) -> InstantTicks {
        // Check for rollover on the lower
        if now < self.lower {
            self.upper += 1;
        }

        self.lower = now;

        self.as_ticks()
    }

    pub fn as_ticks(&self) -> InstantTicks {
        u64::from(self.upper) << T::WIDTH | self.lower.to_ticks()
    }
}

pub trait TicksExt: Ord + Copy + Clone + Add<Self, Output = Self> {
    const ZERO: Self;
    const MAX: Self;
    const WIDTH: usize;

    fn to_ticks(self) -> InstantTicks;
}

impl TicksExt for u8 {
    const ZERO: u8 = 0;
    const MAX: u8 = u8::MAX;
    const WIDTH: usize = u8::BITS as _;

    fn to_ticks(self) -> InstantTicks {
        self.into()
    }
}

impl TicksExt for u16 {
    const ZERO: u16 = 0;
    const MAX: u16 = u16::MAX;
    const WIDTH: usize = u16::BITS as _;

    fn to_ticks(self) -> InstantTicks {
        self.into()
    }
}

impl TicksExt for u32 {
    const ZERO: u32 = 0;
    const MAX: u32 = u32::MAX;
    const WIDTH: usize = u32::BITS as _;

    fn to_ticks(self) -> InstantTicks {
        self.into()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn rate_from_str() {
        assert!(Rate::from_str("0/0").is_err());
        assert!(Rate::from_str("1/0").is_err());
        assert!(Rate::from_str("10/0").is_err());
        assert!(Rate::from_str("10").is_err());

        assert_eq!(Rate::from_str("1/10"), Ok(Rate { nom: 1, denom: 10 }));
        assert_eq!(Rate::from_str("2 / 10"), Ok(Rate { nom: 2, denom: 10 }));
    }

    #[test]
    fn rates() {
        assert_eq!(Rate::new(0, 0), None);
        assert_eq!(Rate::new(1, 0), None);
        assert_eq!(Rate::new(10, 1), None);

        let r = Rate::new(1, 1_000_000).unwrap(); // 1 MHz, 1 tick == 1us
        let ticks = 1_000; // 1ms
        let ns = r * ticks;
        assert_eq!(ns, 1_000_000);

        let r = Rate::new(1, 80_000_000).unwrap(); // 80 MHz, 1 tick == 12.5ns
        let ticks = 1;
        let ns = r * ticks;
        assert_eq!(ns, 12);
        let ticks = 2;
        let ns = r * ticks;
        assert_eq!(ns, 25);
    }

    #[test]
    fn rollover_tracking_u8() {
        // 5 ticks before rollover
        let t0 = u8::MAX - 5;

        // 10 ticks after rollover
        let t1 = 10;

        let mut instant = TrackingInstant::<u8>::zero();
        assert_eq!(instant.elapsed(t0), u64::from(t0));

        let t2 = instant.elapsed(t1);
        assert_eq!(u64::from(t0) + 16, t2);
    }

    #[test]
    fn rollover_tracking_u16() {
        // 5 ticks before rollover
        let t0 = u16::MAX - 5;

        // 10 ticks after rollover
        let t1 = 10;

        let mut instant = TrackingInstant::<u16>::zero();
        assert_eq!(instant.elapsed(t0), u64::from(t0));

        let t2 = instant.elapsed(t1);
        assert_eq!(u64::from(t0) + 16, t2);
    }

    #[test]
    fn rollover_tracking_u32() {
        // 5 ticks before rollover
        let t0 = u32::MAX - 5;

        // 10 ticks after rollover
        let t1 = 10;

        let mut instant = TrackingInstant::<u32>::zero();
        assert_eq!(instant.elapsed(t0), u64::from(t0));

        let t2 = instant.elapsed(t1);
        assert_eq!(u64::from(t0) + 16, t2);
    }
}
