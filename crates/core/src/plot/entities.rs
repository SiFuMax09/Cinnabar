//! Entities that move around in a plot: primed TNT, falling blocks, thrown ender pearls and
//! dropped items.
//!
//! Entities are ticked 20 times per second, independent of the tick rate of the redstone in the
//! plot.

use super::minecart::{self, RAIL_HEIGHT};
use super::physics::{move_box, raycast, Aabb, Vec3};
use super::Plot;
use crate::config::CONFIG;
use crate::interaction;
use crate::player::{self, PacketSender, PlayerPos};
use cinnabar_blocks::block_entities::InventoryEntry;
use cinnabar_blocks::blocks::Block;
use cinnabar_blocks::{generic, BlockFace, BlockPos};
use cinnabar_network::packets::clientbound::*;
use cinnabar_network::packets::{PacketEncoder, PacketEncoderExt};
use cinnabar_proc_macros::protocol_id;
use cinnabar_redstone::rail;
use cinnabar_world::game_event::{block_center, EventContext, GameEvent};
use cinnabar_world::transport::{SpawnRequest, WorldItem};
use cinnabar_world::World;
use std::collections::HashSet;
use std::time::{Duration, Instant};

const TICK_INTERVAL: Duration = Duration::from_millis(50);
/// How many ticks are caught up on at most after the plot was lagging
const MAX_CATCH_UP_TICKS: u32 = 5;

const GRAVITY: f64 = 0.04;
/// The sound category of blocks, which is what TNT uses
const SOUND_CATEGORY_BLOCKS: i32 = 4;
pub const TNT_FUSE: i32 = 80;
const TNT_RADIUS: f32 = 4.0;
const EXPLOSION_RAYS: i32 = 16;

#[derive(Clone, Debug)]
pub enum EntityKind {
    PrimedTnt {
        fuse: i32,
    },
    FallingBlock {
        block_id: u32,
    },
    EnderPearl {
        owner: u128,
    },
    Item {
        entry: InventoryEntry,
    },
    /// A minecart that drives along rails. `on_detector` is the detector rail it is pressing.
    Minecart {
        on_detector: Option<BlockPos>,
    },
}

#[derive(Clone, Debug)]
pub struct Entity {
    pub id: i32,
    pub uuid: u128,
    pub kind: EntityKind,
    pub pos: Vec3,
    pub vel: Vec3,
    pub on_ground: bool,
    pub age: u32,
    /// The rotation in degrees, which only minecarts change
    pub yaw: f32,
    pub pitch: f32,
}

impl Entity {
    pub fn new(kind: EntityKind, pos: Vec3, vel: Vec3) -> Entity {
        Entity {
            id: player::next_entity_id() as i32,
            uuid: rand::random(),
            kind,
            pos,
            vel,
            on_ground: false,
            age: 0,
            yaw: 0.0,
            pitch: 0.0,
        }
    }

    /// The width and height of the hitbox
    fn size(&self) -> (f64, f64) {
        match self.kind {
            EntityKind::PrimedTnt { .. } | EntityKind::FallingBlock { .. } => (0.98, 0.98),
            EntityKind::EnderPearl { .. } | EntityKind::Item { .. } => (0.25, 0.25),
            EntityKind::Minecart { .. } => (0.98, 0.7),
        }
    }

    /// Dropped items stay for 5 minutes
    const ITEM_LIFETIME: u32 = 6000;

    fn spawn_packets(&self) -> Vec<PacketEncoder> {
        let (entity_type, data) = match self.kind {
            EntityKind::PrimedTnt { .. } => {
                (protocol_id!("minecraft:entity_type", "minecraft:tnt"), 0)
            }
            EntityKind::FallingBlock { block_id } => (
                protocol_id!("minecraft:entity_type", "minecraft:falling_block"),
                block_id as i32,
            ),
            EntityKind::EnderPearl { .. } => (
                protocol_id!("minecraft:entity_type", "minecraft:ender_pearl"),
                0,
            ),
            EntityKind::Item { .. } => (protocol_id!("minecraft:entity_type", "minecraft:item"), 0),
            EntityKind::Minecart { .. } => (
                protocol_id!("minecraft:entity_type", "minecraft:minecart"),
                0,
            ),
        };
        let mut packets = vec![CSpawnEntity {
            entity_id: self.id,
            entity_uuid: self.uuid,
            entity_type,
            x: self.pos.x,
            y: self.pos.y,
            z: self.pos.z,
            pitch: self.pitch,
            yaw: self.yaw,
            head_yaw: self.yaw,
            data,
            velocity_x: (self.vel.x * 8000.0) as i16,
            velocity_y: (self.vel.y * 8000.0) as i16,
            velocity_z: (self.vel.z * 8000.0) as i16,
        }
        .encode()];
        match &self.kind {
            EntityKind::PrimedTnt { fuse } => packets.push(self.tnt_metadata(*fuse)),
            EntityKind::Item { entry } => packets.push(self.item_metadata(entry)),
            _ => {}
        }
        packets
    }

    fn tnt_metadata(&self, fuse: i32) -> PacketEncoder {
        let mut fuse_value = Vec::new();
        fuse_value.write_varint(fuse);
        let mut block_value = Vec::new();
        block_value.write_varint(generic::default_state("minecraft:tnt").unwrap() as i32);
        CSetEntityMetadata {
            entity_id: self.id,
            metadata: vec![
                CSetEntityMetadataEntry {
                    index: 8,
                    // Int
                    metadata_type: 1,
                    value: fuse_value,
                },
                CSetEntityMetadataEntry {
                    index: 9,
                    // Block state
                    metadata_type: 14,
                    value: block_value,
                },
            ],
        }
        .encode()
    }

    /// The item stack the client renders, which is metadata field 8 of an item entity
    pub fn item_metadata(&self, entry: &InventoryEntry) -> PacketEncoder {
        let stack = crate::utils::inventory_entry_to_stack(entry);
        let mut value = Vec::new();
        value.write_slot_data(&Some(crate::utils::encode_slot_data(&stack)));
        CSetEntityMetadata {
            entity_id: self.id,
            metadata: vec![CSetEntityMetadataEntry {
                index: 8,
                // Item stack
                metadata_type: 7,
                value,
            }],
        }
        .encode()
    }

    fn teleport_packet(&self) -> PacketEncoder {
        CTeleportEntity {
            entity_id: self.id,
            x: self.pos.x,
            y: self.pos.y,
            z: self.pos.z,
            velocity: (self.vel.x, self.vel.y, self.vel.z),
            yaw: self.yaw,
            pitch: self.pitch,
            on_ground: self.on_ground,
        }
        .encode()
    }

    /// Moves the entity by its velocity, stopping at blocks. Returns the axes it collided on.
    fn move_with_collision(&mut self, world: &impl World) -> super::physics::Collisions {
        let (width, height) = self.size();
        let bb = Aabb::entity(self.pos, width, height);
        let (moved, collisions) = move_box(world, bb, self.vel);
        self.pos += moved;
        self.on_ground = collisions.y && self.vel.y < 0.0;
        if collisions.x {
            self.vel.x = 0.0;
        }
        if collisions.y {
            self.vel.y = 0.0;
        }
        if collisions.z {
            self.vel.z = 0.0;
        }
        collisions
    }
}

/// Something that happens as a result of an entity being ticked
enum Action {
    Explode(Vec3, f32),
    Land(Vec3, u32),
    Teleport(u128, Vec3),
    /// A minecart left and/or entered a detector rail
    Detector {
        leave: Option<BlockPos>,
        enter: Option<BlockPos>,
    },
}

#[derive(Default)]
pub struct Entities {
    list: Vec<Entity>,
    last_tick: Option<Instant>,
    /// Totals for the performance display
    pub(super) ticks_run: u64,
    pub(super) busy: Duration,
}

impl Entities {
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn spawn_packets(&self) -> Vec<PacketEncoder> {
        self.list.iter().flat_map(Entity::spawn_packets).collect()
    }

    pub fn is_minecart(&self, id: i32) -> bool {
        self.list
            .iter()
            .any(|entity| entity.id == id && matches!(entity.kind, EntityKind::Minecart { .. }))
    }
}

/// Ticks a single entity
fn tick_entity(entity: &mut Entity, world: &impl World) -> (bool, Option<Action>) {
    entity.age += 1;
    let mut action = None;
    let mut remove = false;

    match &mut entity.kind {
        EntityKind::PrimedTnt { fuse } => {
            *fuse -= 1;
            entity.vel.y -= GRAVITY;
            entity.move_with_collision(world);
            entity.vel = entity.vel * 0.98;
            if entity.on_ground {
                entity.vel = Vec3::new(entity.vel.x * 0.7, entity.vel.y * -0.5, entity.vel.z * 0.7);
            }
            if let EntityKind::PrimedTnt { fuse } = entity.kind
                && fuse <= 0
            {
                remove = true;
                action = Some(Action::Explode(
                    entity.pos + Vec3::new(0.0, 0.98 * 0.0625, 0.0),
                    TNT_RADIUS,
                ));
            }
        }
        EntityKind::FallingBlock { block_id } => {
            let block_id = *block_id;
            entity.vel.y -= GRAVITY;
            entity.move_with_collision(world);
            entity.vel = entity.vel * 0.98;
            if entity.on_ground {
                remove = true;
                action = Some(Action::Land(entity.pos, block_id));
            } else if entity.pos.y < -64.0 || entity.age > 600 {
                remove = true;
            }
        }
        EntityKind::Item { .. } => {
            entity.vel.y -= GRAVITY;
            entity.move_with_collision(world);
            // 0.98 of air drag, and the default block friction of 0.6 on top of it on the ground
            let horizontal = if entity.on_ground { 0.98 * 0.6 } else { 0.98 };
            entity.vel = Vec3::new(
                entity.vel.x * horizontal,
                entity.vel.y * 0.98,
                entity.vel.z * horizontal,
            );
            if entity.on_ground && entity.vel.y < 0.0 {
                entity.vel.y *= -0.5;
            }
            if entity.age >= Entity::ITEM_LIFETIME || entity.pos.y < -64.0 {
                remove = true;
            }
        }
        EntityKind::Minecart { on_detector } => {
            let previous = *on_detector;
            let detector = match minecart::drive(world, &mut entity.pos, &mut entity.vel) {
                Some(movement) => {
                    entity.yaw = movement.yaw;
                    entity.pitch = movement.pitch;
                    entity.on_ground = true;
                    movement
                        .rail
                        .filter(|pos| matches!(world.get_block(*pos), Block::Generic(id) if rail::is_detector(id)))
                }
                None => {
                    // Off the track the cart falls like the other entities
                    entity.vel.y -= minecart::GRAVITY;
                    entity.move_with_collision(world);
                    entity.vel = entity.vel * 0.95;
                    None
                }
            };
            if entity.pos.y < -64.0 {
                remove = true;
            }
            // Removing the cart releases the detector it was on
            let enter = if remove { None } else { detector };
            if previous != enter {
                action = Some(Action::Detector {
                    leave: previous,
                    enter,
                });
            }
            if let EntityKind::Minecart { on_detector } = &mut entity.kind {
                *on_detector = detector;
            }
        }
        EntityKind::EnderPearl { owner } => {
            let owner = *owner;
            entity.vel.y -= 0.03;
            let distance = entity.vel.length();
            if let Some(hit) = raycast(world, entity.pos, entity.vel, distance, false) {
                // Teleport next to the block that was hit
                let normal = match hit.face {
                    BlockFace::Top => Vec3::new(0.0, 0.0, 0.0),
                    BlockFace::Bottom => Vec3::new(0.0, -1.8, 0.0),
                    BlockFace::North => Vec3::new(0.0, 0.0, -0.3),
                    BlockFace::South => Vec3::new(0.0, 0.0, 0.3),
                    BlockFace::West => Vec3::new(-0.3, 0.0, 0.0),
                    BlockFace::East => Vec3::new(0.3, 0.0, 0.0),
                };
                remove = true;
                action = Some(Action::Teleport(owner, hit.point + normal));
            } else {
                entity.pos += entity.vel;
                entity.vel = entity.vel * 0.99;
                if entity.age > 1200 || entity.pos.y < -64.0 {
                    remove = true;
                }
            }
        }
    }

    (remove, action)
}

impl Plot {
    pub(super) fn broadcast(&self, packet: &PacketEncoder) {
        for player in &self.players {
            player.send_packet(packet);
        }
    }

    /// Spawns the entity unless the plot has reached the limit of entities. Returns whether it was
    /// spawned.
    pub fn spawn_entity(&mut self, entity: Entity) -> bool {
        if self.entities.list.len() >= CONFIG.max_entities as usize {
            return false;
        }
        for packet in entity.spawn_packets() {
            self.broadcast(&packet);
        }
        self.entities.list.push(entity);
        true
    }

    /// Places a minecart on the rail, which is what the minecart item does. Returns whether it
    /// was spawned.
    pub fn spawn_minecart(&mut self, rail_pos: BlockPos) -> bool {
        let pos = Vec3::new(
            rail_pos.x as f64 + 0.5,
            rail_pos.y as f64 + RAIL_HEIGHT,
            rail_pos.z as f64 + 0.5,
        );
        self.spawn_entity(Entity::new(
            EntityKind::Minecart { on_detector: None },
            pos,
            Vec3::ZERO,
        ))
    }

    /// Removes the entity with the id, for example a minecart that was broken
    pub fn remove_entity(&mut self, id: i32) {
        let Some(entity) = self.entities.list.iter().find(|entity| entity.id == id) else {
            return;
        };
        if let EntityKind::Minecart {
            on_detector: Some(pos),
        } = entity.kind
        {
            self.reset_redpiler();
            rail::set_detector_powered(&mut self.world, pos, false);
        }
        self.entities.list.retain(|entity| entity.id != id);
        self.broadcast(
            &CRemoveEntities {
                entity_ids: vec![id],
            }
            .encode(),
        );
    }

    /// Spawns primed TNT that hops up a little, like it does in Minecraft
    pub fn spawn_tnt(&mut self, pos: Vec3, fuse: i32) -> bool {
        let angle = rand::random::<f64>() * std::f64::consts::TAU;
        let vel = Vec3::new(-angle.sin() * 0.02, 0.2, -angle.cos() * 0.02);
        if !self.spawn_entity(Entity::new(EntityKind::PrimedTnt { fuse }, pos, vel)) {
            return false;
        }
        self.world.game_event(
            GameEvent::PrimeFuse,
            (pos.x, pos.y, pos.z),
            EventContext::NONE,
        );
        self.world.play_sound(
            pos.block_pos(),
            protocol_id!("minecraft:sound_event", "minecraft:entity.tnt.primed"),
            SOUND_CATEGORY_BLOCKS,
            1.0,
            1.0,
        );
        true
    }

    /// Turns the TNT block at the position into a primed TNT entity
    fn ignite_tnt_block(&mut self, pos: BlockPos, fuse: i32) {
        if !matches!(self.world.get_block(pos), Block::Generic(id) if generic::state_name(id) == "minecraft:tnt")
        {
            return;
        }
        let center = Vec3::new(pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5);
        // The block stays when there is no room for more entities
        if self.spawn_tnt(center, fuse) {
            self.world.set_block(pos, Block::Air);
        }
    }

    /// Handles what the world asked for since the last time, like TNT that received power and
    /// blocks that lost the block below them
    fn process_world_events(&mut self) {
        for pos in std::mem::take(&mut self.world.pending_ignitions) {
            self.reset_redpiler();
            self.ignite_tnt_block(pos, TNT_FUSE);
        }

        for pos in std::mem::take(&mut self.world.pending_falling) {
            let block = self.world.get_block(pos);
            let below = pos.offset(BlockFace::Bottom);
            if !crate::generic_placement::is_gravity_block(block)
                || !self.world.get_block(below).can_place_block_in()
                || pos.y <= 0
            {
                continue;
            }
            self.reset_redpiler();
            let block_id = block.get_id();
            let center = Vec3::new(pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5);
            if self.spawn_entity(Entity::new(
                EntityKind::FallingBlock { block_id },
                center,
                Vec3::ZERO,
            )) {
                self.world.set_block(pos, Block::Air);
                interaction::change_neighbors(&mut self.world, pos);
            }
        }
    }

    /// Runs the hoppers once and spawns what droppers and dispensers produced. This is part of
    /// the redstone tick, unlike the entities, which are ticked in real time.
    pub(super) fn tick_item_transport(&mut self) {
        if !self.world.transport.is_empty() {
            let transport = &mut self.world.transport;
            transport.changed_items.clear();
            transport.items = self
                .entities
                .list
                .iter()
                .filter_map(|entity| match &entity.kind {
                    EntityKind::Item { entry } => Some(WorldItem {
                        entity_id: entity.id,
                        pos: (entity.pos.x, entity.pos.y, entity.pos.z),
                        entry: entry.clone(),
                    }),
                    _ => None,
                })
                .collect();
            cinnabar_redstone::transport::tick_hoppers(&mut self.world);
            self.apply_picked_up_items();
        }
        self.spawn_transport_entities();
    }

    /// Takes the items that hoppers picked up out of the entities
    fn apply_picked_up_items(&mut self) {
        let transport = &mut self.world.transport;
        if transport.changed_items.is_empty() {
            return;
        }
        let items = std::mem::take(&mut transport.items);
        let changed = std::mem::take(&mut transport.changed_items);
        let mut removed = Vec::new();
        let mut updated = Vec::new();
        for item in items
            .into_iter()
            .filter(|item| changed.contains(&item.entity_id))
        {
            let Some(entity) = self
                .entities
                .list
                .iter_mut()
                .find(|entity| entity.id == item.entity_id)
            else {
                continue;
            };
            if item.entry.count <= 0 {
                removed.push(entity.id);
            } else {
                entity.kind = EntityKind::Item { entry: item.entry };
                updated.push(entity.id);
            }
        }
        for id in updated {
            let entity = self
                .entities
                .list
                .iter()
                .find(|entity| entity.id == id)
                .unwrap();
            if let EntityKind::Item { entry } = &entity.kind {
                self.broadcast(&entity.item_metadata(entry));
            }
        }
        if !removed.is_empty() {
            self.entities
                .list
                .retain(|entity| !removed.contains(&entity.id));
            self.broadcast(
                &CRemoveEntities {
                    entity_ids: removed,
                }
                .encode(),
            );
        }
    }

    fn spawn_transport_entities(&mut self) {
        let transport = &mut self.world.transport;
        let spawned = std::mem::take(&mut transport.spawned);
        let requests = std::mem::take(&mut transport.spawn_requests);
        for item in spawned {
            self.spawn_entity(Entity::new(
                EntityKind::Item { entry: item.entry },
                Vec3::new(item.pos.0, item.pos.1, item.pos.2),
                Vec3::new(item.vel.0, item.vel.1, item.vel.2),
            ));
        }
        for request in requests {
            match request {
                SpawnRequest::PrimedTnt { pos } => {
                    self.spawn_tnt(Vec3::new(pos.0, pos.1, pos.2), TNT_FUSE);
                }
                SpawnRequest::EnderPearl { pos, vel } => {
                    // Without a thrower nobody is teleported
                    self.spawn_entity(Entity::new(
                        EntityKind::EnderPearl { owner: 0 },
                        Vec3::new(pos.0, pos.1, pos.2),
                        Vec3::new(vel.0, vel.1, vel.2),
                    ));
                }
            }
        }
    }

    /// Ticks the entities of the plot if enough time has passed since the last tick
    pub(super) fn tick_entities(&mut self) {
        self.process_world_events();
        if self.entities.list.is_empty() {
            self.entities.last_tick = None;
            return;
        }

        let now = Instant::now();
        let mut last_tick = *self.entities.last_tick.get_or_insert(now);
        let mut ticks = 0;
        while last_tick + TICK_INTERVAL <= now && ticks < MAX_CATCH_UP_TICKS {
            last_tick += TICK_INTERVAL;
            ticks += 1;
            let start = Instant::now();
            self.tick_entities_once();
            self.entities.ticks_run += 1;
            self.entities.busy += start.elapsed();
        }
        self.entities.last_tick = Some(if ticks == MAX_CATCH_UP_TICKS {
            // The plot is too far behind to catch up
            now
        } else {
            last_tick
        });
    }

    fn tick_entities_once(&mut self) {
        let mut actions = Vec::new();
        let mut removed = Vec::new();
        let mut moved = Vec::new();

        for entity in &mut self.entities.list {
            let before = entity.pos;
            let (remove, action) = tick_entity(entity, &self.world);
            if remove {
                removed.push(entity.id);
            } else if entity.pos != before {
                moved.push(entity.teleport_packet());
            }
            if let EntityKind::PrimedTnt { fuse } = entity.kind
                && !remove
                && fuse % 5 == 0
            {
                // Keep the fuse of the client in sync
                moved.push(entity.tnt_metadata(fuse));
            }
            actions.extend(action);
        }

        let removed_set: HashSet<i32> = removed.iter().copied().collect();
        self.entities
            .list
            .retain(|entity| !removed_set.contains(&entity.id));
        for packet in &moved {
            self.broadcast(packet);
        }
        if !removed.is_empty() {
            self.broadcast(
                &CRemoveEntities {
                    entity_ids: removed,
                }
                .encode(),
            );
        }

        for action in actions {
            match action {
                Action::Explode(center, radius) => self.explode(center, radius),
                Action::Land(pos, block_id) => self.land_falling_block(pos, block_id),
                Action::Detector { leave, enter } => {
                    self.reset_redpiler();
                    if let Some(pos) = leave {
                        rail::set_detector_powered(&mut self.world, pos, false);
                    }
                    if let Some(pos) = enter {
                        rail::set_detector_powered(&mut self.world, pos, true);
                    }
                }
                Action::Teleport(owner, pos) => {
                    if let Some(player) = self.players.iter_mut().find(|p| p.uuid == owner) {
                        player.teleport(PlayerPos::new(pos.x, pos.y, pos.z));
                    }
                    let at = (pos.x, pos.y, pos.z);
                    self.world
                        .game_event(GameEvent::ProjectileLand, at, EventContext::NONE);
                    self.world
                        .game_event(GameEvent::Teleport, at, EventContext::NONE);
                }
            }
        }
    }

    fn land_falling_block(&mut self, pos: Vec3, block_id: u32) {
        let block_pos = pos.block_pos();
        if !self.world.get_block(block_pos).can_place_block_in()
            || !(0..super::PLOT_BLOCK_HEIGHT).contains(&block_pos.y)
        {
            return;
        }
        self.reset_redpiler();
        self.world.set_block(block_pos, Block::from_id(block_id));
        interaction::change_neighbors(&mut self.world, block_pos);
        self.world.game_event(
            GameEvent::BlockPlace,
            block_center(block_pos),
            EventContext::affecting(Block::from_id(block_id).get_name()),
        );
    }

    /// Destroys the blocks around the center of the explosion the way vanilla Minecraft does it
    /// and pushes the entities in range away
    pub fn explode(&mut self, center: Vec3, radius: f32) {
        let mut destroyed = HashSet::new();
        let steps = EXPLOSION_RAYS - 1;
        for x in 0..EXPLOSION_RAYS {
            for y in 0..EXPLOSION_RAYS {
                for z in 0..EXPLOSION_RAYS {
                    // Only the rays towards the surface of the cube
                    if x != 0 && x != steps && y != 0 && y != steps && z != 0 && z != steps {
                        continue;
                    }
                    let dir = Vec3::new(
                        x as f64 / steps as f64 * 2.0 - 1.0,
                        y as f64 / steps as f64 * 2.0 - 1.0,
                        z as f64 / steps as f64 * 2.0 - 1.0,
                    )
                    .normalized();

                    let mut power = radius * (0.7 + rand::random::<f32>() * 0.6);
                    let mut pos = center;
                    while power > 0.0 {
                        let block_pos = pos.block_pos();
                        let resistance =
                            generic::explosion_resistance(self.world.get_block_raw(block_pos));
                        power -= (resistance + 0.3) * 0.3;
                        if power > 0.0 && (0..super::PLOT_BLOCK_HEIGHT).contains(&block_pos.y) {
                            destroyed.insert(block_pos);
                        }
                        pos += dir * 0.3;
                        power -= 0.225;
                    }
                }
            }
        }

        let in_plot: Vec<BlockPos> = destroyed
            .into_iter()
            .filter(|pos| super::Plot::in_plot_bounds(self.world.x, self.world.z, pos.x, pos.z))
            .collect();

        // Entities in range get pushed away
        let reach = radius as f64 * 2.0;
        let mut pushed = Vec::new();
        for entity in &mut self.entities.list {
            let (_, height) = entity.size();
            let offset = entity.pos + Vec3::new(0.0, height / 2.0, 0.0) - center;
            // Cheap check first, as there can be a lot of entities
            if offset.x.abs() > reach || offset.y.abs() > reach || offset.z.abs() > reach {
                continue;
            }
            let distance = offset.length();
            if distance > reach || distance < 1e-6 {
                continue;
            }
            entity.vel += offset.normalized() * (1.0 - distance / reach);
            pushed.push(CSetEntityMotion {
                entity_id: entity.id,
                x: entity.vel.x,
                y: entity.vel.y,
                z: entity.vel.z,
            });
        }
        // The entities that moved are sent their position in the next tick anyway, so the motion
        // is only needed for the ones that are in the air
        for motion in pushed {
            self.broadcast(&motion.encode());
        }

        self.reset_redpiler();
        let mut ignite = Vec::new();
        for pos in &in_plot {
            let block = self.world.get_block(*pos);
            if block == Block::Air {
                continue;
            }
            if matches!(block, Block::Generic(id) if generic::state_name(id) == "minecraft:tnt") {
                ignite.push(*pos);
            } else if let Block::Generic(_) = block {
                // Blocks without behavior don't need the expensive updates of their neighbors
                self.world.set_block(*pos, Block::Air);
                self.world
                    .schedule_falling_block(pos.offset(BlockFace::Top));
            } else {
                interaction::destroy(block, &mut self.world, *pos);
            }
        }
        for pos in ignite {
            // Blocks of TNT that are hit start a shorter fuse
            let fuse = 10 + (rand::random::<f32>() * 20.0) as i32;
            self.ignite_tnt_block(pos, fuse);
        }

        self.world.game_event(
            GameEvent::Explode,
            (center.x, center.y, center.z),
            EventContext::NONE,
        );
        self.broadcast(
            &CExplode {
                x: center.x,
                y: center.y,
                z: center.z,
                radius,
                block_count: in_plot.len() as i32,
            }
            .encode(),
        );
        self.world.flush_block_changes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cinnabar_world::testing::TestWorld;

    /// Prints the packets of a TNT entity as hex, so they can be checked against the game's codecs
    #[test]
    #[ignore]
    fn dump_tnt_packets() {
        let mut entity = Entity::new(
            EntityKind::PrimedTnt { fuse: 80 },
            Vec3::new(1.5, 70.0, 2.5),
            Vec3::ZERO,
        );
        entity.id = 5;
        let mut packets = entity.spawn_packets();
        packets.push(entity.teleport_packet());
        packets.push(
            CExplode {
                x: 1.5,
                y: 70.0,
                z: 2.5,
                radius: 4.0,
                block_count: 10,
            }
            .encode(),
        );
        packets.push(
            CSetEntityMotion {
                entity_id: 5,
                x: 0.1,
                y: 0.2,
                z: 0.0,
            }
            .encode(),
        );
        for packet in packets {
            let mut bytes = Vec::new();
            packet.write_uncompressed(&mut bytes).unwrap();
            println!(
                "PACKET {}",
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
        }
    }

    /// Prints the packets of an item entity as hex, so they can be checked against the game's codecs
    #[test]
    #[ignore]
    fn dump_item_packets() {
        let mut entity = Entity::new(
            EntityKind::Item {
                entry: InventoryEntry {
                    id: 1,
                    slot: 0,
                    count: 3,
                    nbt: None,
                },
            },
            Vec3::new(1.5, 70.0, 2.5),
            Vec3::ZERO,
        );
        entity.id = 6;
        for packet in entity.spawn_packets() {
            let mut bytes = Vec::new();
            packet.write_uncompressed(&mut bytes).unwrap();
            println!(
                "PACKET {}",
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
        }
    }

    /// Prints the packets of a minecart as hex, so they can be checked against the game's codecs
    #[test]
    #[ignore]
    fn dump_minecart_packets() {
        let mut entity = Entity::new(
            EntityKind::Minecart { on_detector: None },
            Vec3::new(1.5, 70.0625, 2.5),
            Vec3::new(0.2, 0.0, 0.0),
        );
        entity.id = 7;
        entity.yaw = -90.0;
        entity.pitch = 0.0;
        let mut packets = entity.spawn_packets();
        packets.push(entity.teleport_packet());
        for packet in packets {
            let mut bytes = Vec::new();
            packet.write_uncompressed(&mut bytes).unwrap();
            println!(
                "PACKET {}",
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
        }
    }

    fn minecart_world() -> TestWorld {
        let mut world = TestWorld::new(2, 2, 2);
        let stone = generic::default_state("minecraft:stone").unwrap();
        for x in 0..32 {
            for z in 0..32 {
                world.set_block_raw(BlockPos::new(x, 0, z), stone);
            }
        }
        for x in 5..=12 {
            let id = generic::default_state("minecraft:rail").unwrap();
            world.set_block_raw(BlockPos::new(x, 1, 5), id);
            cinnabar_redstone::rail::on_placed(&mut world, BlockPos::new(x, 1, 5));
        }
        world
    }

    #[test]
    fn minecart_drives_along_the_track_and_is_teleported() {
        let world = minecart_world();
        let mut cart = Entity::new(
            EntityKind::Minecart { on_detector: None },
            Vec3::new(5.5, 1.0 + RAIL_HEIGHT, 5.5),
            Vec3::new(0.2, 0.0, 0.0),
        );
        let (remove, action) = tick_entity(&mut cart, &world);
        assert!(!remove);
        assert!(action.is_none());
        assert!(cart.pos.x > 5.5);
        assert!(cart.on_ground);
        assert!((cart.yaw + 90.0).abs() < 1e-3);
    }

    #[test]
    fn minecart_off_the_track_falls() {
        let world = minecart_world();
        let mut cart = Entity::new(
            EntityKind::Minecart { on_detector: None },
            Vec3::new(20.5, 5.0, 5.5),
            Vec3::ZERO,
        );
        tick_entity(&mut cart, &world);
        assert!(cart.pos.y < 5.0);
    }

    #[test]
    fn minecart_on_a_detector_rail_reports_it() {
        let mut world = minecart_world();
        let detector = BlockPos::new(7, 1, 5);
        let id = generic::default_state("minecraft:detector_rail").unwrap();
        let id = generic::with_property(id, "shape", "east_west");
        world.set_block_raw(detector, id);
        let mut cart = Entity::new(
            EntityKind::Minecart { on_detector: None },
            Vec3::new(6.9, 1.0 + RAIL_HEIGHT, 5.5),
            Vec3::new(0.3, 0.0, 0.0),
        );
        let (_, action) = tick_entity(&mut cart, &world);
        match action {
            Some(Action::Detector { leave, enter }) => {
                assert_eq!(leave, None);
                assert_eq!(enter, Some(detector));
            }
            _ => panic!("the detector should be entered"),
        }
        assert!(
            matches!(cart.kind, EntityKind::Minecart { on_detector: Some(pos) } if pos == detector)
        );
    }
}
