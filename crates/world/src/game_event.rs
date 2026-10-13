//! Game events are the things that happen in the world that can be heard by listeners like sculk
//! sensors. Only the events that produce vibrations are modeled.

/// A position in the world, which is not necessarily the center of a block
pub type EventPos = (f64, f64, f64);

pub fn block_center(pos: cinnabar_blocks::BlockPos) -> EventPos {
    (pos.x as f64 + 0.5, pos.y as f64 + 0.5, pos.z as f64 + 0.5)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GameEvent {
    BlockActivate,
    BlockAttach,
    BlockChange,
    BlockClose,
    BlockDeactivate,
    BlockDestroy,
    BlockDetach,
    BlockOpen,
    BlockPlace,
    Bounce,
    ContainerClose,
    ContainerOpen,
    Drink,
    Eat,
    ElytraGlide,
    EntityAction,
    EntityDamage,
    EntityDie,
    EntityDismount,
    EntityInteract,
    EntityMount,
    EntityPlace,
    Equip,
    Explode,
    Flap,
    FluidPickup,
    FluidPlace,
    HitGround,
    InstrumentPlay,
    ItemInteractFinish,
    ItemInteractStart,
    LightningStrike,
    NoteBlockPlay,
    PrimeFuse,
    ProjectileLand,
    ProjectileShoot,
    SculkSensorTendrilsClicking,
    Shear,
    Splash,
    Step,
    Swim,
    Teleport,
    Unequip,
    /// The event amethyst blocks next to an activated sculk sensor send out. The frequency is the
    /// one of the vibration the sensor received (1 to 15).
    Resonate(u8),
}

impl GameEvent {
    /// The frequency of the vibration the event causes, from 1 to 15, or 0 if sculk sensors don't
    /// hear it
    pub fn frequency(self) -> u8 {
        use GameEvent::*;
        match self {
            Step | Swim | Flap => 1,
            ProjectileLand | HitGround | Splash | Bounce => 2,
            ItemInteractFinish | ProjectileShoot | InstrumentPlay => 3,
            EntityAction | ElytraGlide | Unequip => 4,
            EntityDismount | Equip => 5,
            EntityInteract | Shear | EntityMount => 6,
            EntityDamage => 7,
            Drink | Eat => 8,
            ContainerClose | BlockClose | BlockDeactivate | BlockDetach => 9,
            ContainerOpen | BlockOpen | BlockActivate | BlockAttach | PrimeFuse | NoteBlockPlay => {
                10
            }
            BlockChange => 11,
            BlockDestroy | FluidPickup => 12,
            BlockPlace | FluidPlace => 13,
            EntityPlace | LightningStrike | Teleport => 14,
            EntityDie | Explode => 15,
            Resonate(frequency) => frequency.clamp(1, 15),
            ItemInteractStart | SculkSensorTendrilsClicking => 0,
        }
    }

    /// Whether players that are sneaking don't cause the event to be heard
    pub fn ignored_when_sneaking(self) -> bool {
        use GameEvent::*;
        matches!(
            self,
            HitGround | ProjectileShoot | Step | Swim | ItemInteractStart | ItemInteractFinish
        )
    }
}

/// Who or what caused a game event, as far as it matters for listeners
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventContext {
    /// A sneaking player is not heard for some events, see `GameEvent::ignored_when_sneaking`
    pub sneaking: bool,
    pub spectator: bool,
    /// The block the event is about absorbs vibrations
    pub dampened: bool,
}

impl EventContext {
    pub const NONE: EventContext = EventContext {
        sneaking: false,
        spectator: false,
        dampened: false,
    };

    /// The context of an event that affects the block with the name
    pub fn affecting(block_name: &str) -> EventContext {
        EventContext {
            dampened: dampens_vibrations(block_name),
            ..EventContext::NONE
        }
    }
}

/// Blocks that don't cause vibrations when something happens to them
pub fn dampens_vibrations(block_name: &str) -> bool {
    occludes_vibrations(block_name)
        || (block_name.ends_with("_carpet") && !block_name.contains("moss"))
}

/// Blocks that vibrations can't travel through
pub fn occludes_vibrations(block_name: &str) -> bool {
    block_name.ends_with("_wool")
}

/// The redstone signal strength a sculk sensor outputs for a vibration that traveled `distance`
/// blocks, for a sensor that listens up to `radius` blocks away
pub fn redstone_strength_for_distance(distance: f32, radius: u32) -> u8 {
    let power_scale = 15.0 / radius as f64;
    (15 - (power_scale * distance as f64).floor() as i32).clamp(1, 15) as u8
}

/// How many ticks a vibration needs to travel `distance` blocks
pub fn travel_time(distance: f32) -> u32 {
    distance.floor() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequencies() {
        assert_eq!(GameEvent::Step.frequency(), 1);
        assert_eq!(GameEvent::HitGround.frequency(), 2);
        assert_eq!(GameEvent::ProjectileLand.frequency(), 2);
        assert_eq!(GameEvent::ContainerOpen.frequency(), 10);
        assert_eq!(GameEvent::BlockClose.frequency(), 9);
        assert_eq!(GameEvent::BlockDestroy.frequency(), 12);
        assert_eq!(GameEvent::BlockPlace.frequency(), 13);
        assert_eq!(GameEvent::Teleport.frequency(), 14);
        assert_eq!(GameEvent::Explode.frequency(), 15);
        assert_eq!(GameEvent::PrimeFuse.frequency(), 10);
        assert_eq!(GameEvent::NoteBlockPlay.frequency(), 10);
        assert_eq!(GameEvent::SculkSensorTendrilsClicking.frequency(), 0);
        for frequency in 1..=15 {
            assert_eq!(GameEvent::Resonate(frequency).frequency(), frequency);
        }
    }

    #[test]
    fn strength_for_distance() {
        // A normal sensor listens up to 8 blocks away
        let expected = [15, 14, 12, 10, 8, 6, 4, 2, 1];
        for (distance, power) in expected.into_iter().enumerate() {
            assert_eq!(redstone_strength_for_distance(distance as f32, 8), power);
        }
        // A calibrated sensor listens up to 16 blocks away
        assert_eq!(redstone_strength_for_distance(0.0, 16), 15);
        assert_eq!(redstone_strength_for_distance(1.0, 16), 15);
        assert_eq!(redstone_strength_for_distance(2.0, 16), 14);
        assert_eq!(redstone_strength_for_distance(8.0, 16), 8);
        assert_eq!(redstone_strength_for_distance(16.0, 16), 1);
        // The power is never 0 for a vibration that was heard
        assert_eq!(redstone_strength_for_distance(8.5, 8), 1);
    }

    #[test]
    fn travel_times() {
        assert_eq!(travel_time(0.0), 0);
        assert_eq!(travel_time(1.99), 1);
        assert_eq!(travel_time(7.0), 7);
    }

    #[test]
    fn dampening() {
        assert!(dampens_vibrations("minecraft:white_wool"));
        assert!(dampens_vibrations("minecraft:red_carpet"));
        assert!(!dampens_vibrations("minecraft:moss_carpet"));
        assert!(!dampens_vibrations("minecraft:stone"));
        assert!(occludes_vibrations("minecraft:blue_wool"));
        assert!(!occludes_vibrations("minecraft:blue_carpet"));
    }

    #[test]
    fn sneaking_events() {
        assert!(GameEvent::Step.ignored_when_sneaking());
        assert!(GameEvent::HitGround.ignored_when_sneaking());
        assert!(!GameEvent::BlockPlace.ignored_when_sneaking());
        assert!(!GameEvent::ContainerOpen.ignored_when_sneaking());
    }
}
