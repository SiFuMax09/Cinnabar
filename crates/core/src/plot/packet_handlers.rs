use super::entities::{Entity, EntityKind};
use super::physics::Vec3;
use super::Plot;
use crate::config::CONFIG;
use crate::menu::ClickMode;
use crate::player::Gamemode;
use crate::player::{PacketSender, PlayerPos, SkinParts};
use crate::server::Message;
use crate::utils::{self, HyphenatedUUID};
use cinnabar_blocks::block_entities::BlockEntity;
use cinnabar_blocks::blocks::Block;
use cinnabar_blocks::items::{Item, ItemStack};
use cinnabar_blocks::BlockPos;
use cinnabar_network::packets::clientbound::*;
use cinnabar_network::packets::serverbound::*;
use cinnabar_redstone::transport::{read_inventory, write_inventory};
use cinnabar_world::World;
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;
use tracing::error;

impl Plot {
    /// Lets the other players see the item in the hand of the player when it changed
    fn after_player_inventory_changed(&mut self, player_idx: usize, changed_slots: &[usize]) {
        let selected = self.players[player_idx].selected_slot as usize + 36;
        if !changed_slots.contains(&selected) {
            return;
        }
        let Some(equipment) = self.players[player_idx].equippment_packet() else {
            return;
        };
        let packet = equipment.encode();
        for (idx, other) in self.players.iter().enumerate() {
            if idx != player_idx {
                other.client.send_packet(&packet);
            }
        }
    }

    /// Shows the players what hoppers and other players did to the container they have open
    pub(super) fn sync_open_menus(&mut self) {
        for player in &mut self.players {
            let Some(menu) = &mut player.menu else {
                continue;
            };
            match read_inventory(&self.world, menu.pos).filter(|(ty, _)| *ty == menu.ty) {
                None => player.close_menu_on_client(),
                Some((_, container)) if container != menu.container_inventory() => {
                    menu.load(&container, &player.inventory);
                    player.send_menu_contents();
                }
                Some(_) => {}
            }
        }
    }

    /// Throws the stack out of the player the way the drop key does
    fn drop_item(&mut self, player_idx: usize, stack: ItemStack) {
        let player = &self.players[player_idx];
        let direction = Vec3::from_rotation(player.yaw, player.pitch);
        let pos = Vec3::new(player.pos.x, player.pos.y + 1.32, player.pos.z);
        self.spawn_entity(Entity::new(
            EntityKind::Item {
                entry: crate::menu::stack_to_entry(&stack, 0),
            },
            pos,
            direction * 0.3,
        ));
    }

    pub(super) fn handle_packets_for_player(&mut self, player: usize) {
        let packets = self.players[player].client.receive_packets();
        for packet in packets {
            packet.handle(self, player);
        }
    }
}

impl ServerBoundPacketHandler for Plot {
    fn handle_command_suggestions_request(
        &mut self,
        packet: SCommandSuggestionsRequest,
        player_idx: usize,
    ) {
        if !packet.text.starts_with("//load ") {
            return;
        }

        let mut path = PathBuf::from("./schems");
        if CONFIG.schemati {
            let uuid = self.players[player_idx].uuid;
            path.push(HyphenatedUUID(uuid).to_string());
        }

        let current = &packet.text[7..];
        let mut res = CCommandSuggestionsResponse {
            id: packet.transaction_id,
            start: 7,
            length: current.len() as i32,
            matches: Vec::new(),
        };

        let dir = match fs::read_dir(path) {
            Ok(dir) => dir,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    error!("There was an error completing //load");
                    error!("{}", err);
                }
                return;
            }
        };

        for entry in dir {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(current) {
                    let m = CCommandSuggestionsResponseMatch {
                        match_: name.to_string(),
                        tooltip: None,
                    };
                    res.matches.push(m);
                }
            }
        }

        self.players[player_idx].send_packet(&res.encode());
    }

    fn handle_keep_alive(&mut self, _keep_alive: SKeepAlive, player_idx: usize) {
        let player = &mut self.players[player_idx];
        player.last_keep_alive_received = Instant::now();
        player.ping_ms = player.last_keep_alive_sent_at().elapsed().as_millis() as u32;
    }

    fn handle_set_creative_mode_slot(
        &mut self,
        creative_inventory_action: SSetCreativeModeSlot,
        player: usize,
    ) {
        if let Some(slot_data) = creative_inventory_action.clicked_item {
            if creative_inventory_action.slot < 0 || creative_inventory_action.slot >= 46 {
                return;
            }
            let item = ItemStack {
                count: slot_data.item_count as u8,
                item_type: Item::from_id(slot_data.item_id as u32),
                nbt: slot_data.nbt.map(nbt::Blob::with_content),
            };
            self.players[player].inventory[creative_inventory_action.slot as usize] = Some(item);
            if creative_inventory_action.slot as u32 == self.players[player].selected_slot + 36 {
                let entity_equipment = CSetEquipment {
                    entity_id: self.players[player].entity_id as i32,
                    equipment: vec![CSetEquipmentEquipment {
                        slot: 0, // Main hand
                        item: self.players[player].inventory
                            [creative_inventory_action.slot as usize]
                            .as_ref()
                            .map(utils::encode_slot_data),
                    }],
                }
                .encode();
                for other_player in 0..self.players.len() {
                    if player == other_player {
                        continue;
                    };
                    self.players[other_player]
                        .client
                        .send_packet(&entity_equipment);
                }
            }
        } else {
            self.players[player].inventory[creative_inventory_action.slot as usize] = None;
        }

        // We wanna retrieve the item slot from the inventory
        // rather than re-using the value from the client,
        // to avoid getting out of sync if the server validates
        // or sanitizes data or something
        let item = self.players[player].inventory[creative_inventory_action.slot as usize].clone();
        self.players[player].set_inventory_slot(creative_inventory_action.slot as u32, item);
    }

    fn handle_container_click(&mut self, packet: SContainerClick, player_idx: usize) {
        let player = &mut self.players[player_idx];
        let Some(menu) = &mut player.menu else {
            return;
        };
        if menu.window_id != packet.window_id {
            return;
        }
        let pos = menu.pos;
        let Some((ty, container)) =
            read_inventory(&self.world, pos).filter(|(ty, _)| *ty == menu.ty)
        else {
            // The container was destroyed while the screen was open
            player.close_menu_on_client();
            return;
        };
        if matches!(player.gamemode, Gamemode::Spectator) {
            player.send_menu_contents();
            return;
        }

        // Hoppers and other players may have changed the container since the last click
        menu.load(&container, &player.inventory);
        let mut dropped = Vec::new();
        menu.click(
            packet.slot as i32,
            packet.button,
            ClickMode::from_id(packet.mode),
            &mut dropped,
        );

        let new_container = menu.container_inventory();
        let changed_slots = menu.store_player_inventory(&mut player.inventory);
        let container_changed = new_container != container;
        player.send_menu_contents();
        self.after_player_inventory_changed(player_idx, &changed_slots);
        if container_changed {
            self.reset_redpiler();
            write_inventory(&mut self.world, pos, ty, new_container);
            self.world.flush_block_changes();
        }
        for stack in dropped {
            self.drop_item(player_idx, stack);
        }
    }

    fn handle_container_close(&mut self, packet: SContainerClose, player_idx: usize) {
        let player = &mut self.players[player_idx];
        let Some(menu) = &mut player.menu else {
            return;
        };
        if menu.window_id != packet.window_id {
            return;
        }
        let rest = menu.close();
        let changed_slots = menu.store_player_inventory(&mut player.inventory);
        player.menu = None;
        for slot in &changed_slots {
            let item = player.inventory[*slot].clone();
            player.send_inventory_slot(*slot as u32, item.as_ref());
        }
        self.after_player_inventory_changed(player_idx, &changed_slots);
        if let Some(rest) = rest {
            self.drop_item(player_idx, rest);
        }
    }

    fn handle_player_abilities(&mut self, player_abilities: SPlayerAbilities, player: usize) {
        self.players[player].flying = player_abilities.is_flying;
    }

    fn handle_attack(&mut self, attack: SAttack, player: usize) {
        // Hitting a minecart breaks it, which only creative players do here
        let is_minecart = self.entities.is_minecart(attack.entity_id);
        if !is_minecart
            || !matches!(self.players[player].gamemode, Gamemode::Creative)
            || !super::may_edit_plot(
                self.owner,
                self.players[player].uuid,
                self.edit_permissions(player),
            )
        {
            return;
        }
        self.remove_entity(attack.entity_id);
    }

    fn handle_swing_arm(&mut self, animation: SSwingArm, player: usize) {
        let animation_id = match animation.hand {
            0 => 0,
            1 => 3,
            _ => 0,
        };
        let entity_animation = CEntityAnimation {
            entity_id: self.players[player].entity_id as i32,
            animation: animation_id,
        }
        .encode();
        for other_player in 0..self.players.len() {
            if player == other_player {
                continue;
            };
            self.players[other_player]
                .client
                .send_packet(&entity_animation);
        }
    }

    fn handle_use_item_on(&mut self, use_item_on: SUseItemOn, player: usize) {
        self.handle_use_item_impl(&use_item_on, player);

        let acknowledge_block_change = CAcknowledgeBlockChange {
            sequence_id: use_item_on.sequence,
        }
        .encode();
        self.players[player].send_packet(&acknowledge_block_change);
    }

    fn handle_chat_command(&mut self, chat_command: SChatCommand, player: usize) {
        self.players[player]
            .command_queue
            .push(chat_command.command);
    }

    fn handle_chat_message(&mut self, chat_message: SChatMessage, player: usize) {
        let message = chat_message.message;
        let player = &self.players[player];
        let broadcast_message = Message::ChatInfo(player.uuid, player.username.clone(), message);
        self.message_sender.send(broadcast_message).unwrap();
    }

    fn handle_client_information(&mut self, client_settings: SClientInformation, player: usize) {
        let player = &mut self.players[player];
        player.skin_parts =
            SkinParts::from_bits_truncate(client_settings.displayed_skin_parts as u32);
        let metadata_entry = CSetEntityMetadataEntry {
            index: 16,
            metadata_type: 0,
            value: vec![player.skin_parts.bits() as u8],
        };
        let entity_metadata = CSetEntityMetadata {
            entity_id: player.entity_id as i32,
            metadata: vec![metadata_entry],
        }
        .encode();
        for player in &mut self.players {
            player.client.send_packet(&entity_metadata);
        }
    }

    fn handle_plugin_message(&mut self, plugin_message: SPluginMessage, player: usize) {
        if plugin_message.channel == "worldedit:cui" {
            // The mod renders the selection itself, so the particle outline would be a duplicate
            self.players[player].selection_viz.cui = true;
            self.players[player].worldedit_send_cui("s|cuboid");
        }
    }

    fn handle_set_player_position(&mut self, player_position: SSetPlayerPosition, player: usize) {
        let old = self.players[player].pos;
        let new = PlayerPos::new(player_position.x, player_position.y, player_position.z);
        self.players[player].pos = new;
        self.players[player].on_ground = player_position.on_ground;
        let packet = if (new.x - old.x).abs() > 8.0
            || (new.y - old.y).abs() > 8.0
            || (new.z - old.z).abs() > 8.0
        {
            CTeleportEntity {
                velocity: (0.0, 0.0, 0.0),
                entity_id: self.players[player].entity_id as i32,
                x: new.x,
                y: new.y,
                z: new.z,
                yaw: self.players[player].yaw,
                pitch: self.players[player].pitch,
                on_ground: player_position.on_ground,
            }
            .encode()
        } else {
            let delta_x = ((player_position.x * 32.0 - old.x * 32.0) * 128.0) as i16;
            let delta_y = ((player_position.y * 32.0 - old.y * 32.0) * 128.0) as i16;
            let delta_z = ((player_position.z * 32.0 - old.z * 32.0) * 128.0) as i16;
            CUpdateEntityPosition {
                delta_x,
                delta_y,
                delta_z,
                entity_id: self.players[player].entity_id as i32,
                on_ground: player_position.on_ground,
            }
            .encode()
        };
        for other_player in 0..self.players.len() {
            if player == other_player {
                continue;
            };
            self.players[other_player].client.send_packet(&packet);
        }
        self.on_player_move(player, old, new);
    }

    fn handle_set_player_position_and_rotation(
        &mut self,
        player_position_and_rotation: SSetPlayerPositionAndRotation,
        player: usize,
    ) {
        let old = self.players[player].pos;
        let new = PlayerPos::new(
            player_position_and_rotation.x,
            player_position_and_rotation.y,
            player_position_and_rotation.z,
        );
        self.players[player].pos = new;
        self.players[player].yaw = player_position_and_rotation.yaw;
        self.players[player].pitch = player_position_and_rotation.pitch;
        self.players[player].on_ground = player_position_and_rotation.on_ground;
        let packet = if (new.x - old.x).abs() > 8.0
            || (new.y - old.y).abs() > 8.0
            || (new.z - old.z).abs() > 8.0
        {
            CTeleportEntity {
                velocity: (0.0, 0.0, 0.0),
                entity_id: self.players[player].entity_id as i32,
                x: new.x,
                y: new.y,
                z: new.z,
                yaw: self.players[player].yaw,
                pitch: self.players[player].pitch,
                on_ground: player_position_and_rotation.on_ground,
            }
            .encode()
        } else {
            let delta_x = ((player_position_and_rotation.x * 32.0 - old.x * 32.0) * 128.0) as i16;
            let delta_y = ((player_position_and_rotation.y * 32.0 - old.y * 32.0) * 128.0) as i16;
            let delta_z = ((player_position_and_rotation.z * 32.0 - old.z * 32.0) * 128.0) as i16;
            CUpdateEntityPositionAndRotation {
                delta_x,
                delta_y,
                delta_z,
                pitch: player_position_and_rotation.pitch,
                yaw: player_position_and_rotation.yaw,
                entity_id: self.players[player].entity_id as i32,
                on_ground: player_position_and_rotation.on_ground,
            }
            .encode()
        };
        let entity_head_look = CSetHeadRotation {
            entity_id: self.players[player].entity_id as i32,
            head_yaw: player_position_and_rotation.yaw,
        }
        .encode();
        for other_player in 0..self.players.len() {
            if player == other_player {
                continue;
            };
            self.players[other_player].client.send_packet(&packet);
            self.players[other_player]
                .client
                .send_packet(&entity_head_look);
        }
        self.on_player_move(player, old, new);
    }

    fn handle_player_rotation(&mut self, player_rotation: SPlayerRotation, player: usize) {
        self.players[player].yaw = player_rotation.yaw;
        self.players[player].pitch = player_rotation.pitch;
        self.players[player].on_ground = player_rotation.on_ground;
        let rotation_packet = CEntityRotation {
            entity_id: self.players[player].entity_id as i32,
            yaw: player_rotation.yaw,
            pitch: player_rotation.pitch,
            on_ground: player_rotation.on_ground,
        }
        .encode();
        let entity_head_look = CSetHeadRotation {
            entity_id: self.players[player].entity_id as i32,
            head_yaw: player_rotation.yaw,
        }
        .encode();
        for other_player in 0..self.players.len() {
            if player == other_player {
                continue;
            };
            self.players[other_player]
                .client
                .send_packet(&rotation_packet);
            self.players[other_player]
                .client
                .send_packet(&entity_head_look);
        }
    }

    fn handle_set_player_on_ground(&mut self, player_movement: SSetPlayerOnGround, player: usize) {
        self.players[player].on_ground = player_movement.on_ground;
    }

    fn handle_player_action(&mut self, player_action: SPlayerAction, player: usize) {
        if player_action.status == 0 {
            let block_pos = BlockPos::new(player_action.x, player_action.y, player_action.z);
            self.handle_player_digging(block_pos, player);

            let acknowledge_block_change = CAcknowledgeBlockChange {
                sequence_id: player_action.sequence,
            }
            .encode();
            self.players[player].send_packet(&acknowledge_block_change);
        } else {
            let selected_slot = self.players[player].selected_slot as usize + 36;
            if player_action.status == 3 {
                self.players[player].inventory[selected_slot] = None;
            } else if player_action.status == 4 {
                let mut stack_empty = false;
                if let Some(item_stack) = &mut self.players[player].inventory[selected_slot] {
                    item_stack.count -= 1;
                    stack_empty = item_stack.count == 0;
                }
                if stack_empty {
                    self.players[player].inventory[selected_slot] = None;
                }
            }
        }
    }

    fn handle_player_command(&mut self, entity_action: SPlayerCommand, player: usize) {
        match entity_action.action_id {
            // Start sprinting
            1 => self.players[player].sprinting = true,
            // Stop sprinting
            2 => self.players[player].sprinting = false,
            _ => return,
        }
        self.broadcast_player_state(player);
    }

    fn handle_use_item(&mut self, use_item: SUseItem, player: usize) {
        self.players[player].send_packet(
            &CAcknowledgeBlockChange {
                sequence_id: use_item.sequence,
            }
            .encode(),
        );

        let item = if use_item.hand == 0 {
            self.players[player].inventory[self.players[player].selected_slot as usize + 36].clone()
        } else {
            self.players[player].inventory[45].clone()
        };
        let Some(item) = item else {
            return;
        };

        if use_item.hand == 0 && self.try_use_brush(player, item.item_type.get_id(), None) {
            return;
        }

        if let Some(owner) = self.owner {
            let player = &mut self.players[player];
            if owner != player.uuid && !player.has_permission("plots.admin.interact.other") {
                player.send_no_permission_message();
                return;
            }
        } else if !self.players[player].has_permission("plots.admin.interact.unowned") {
            self.players[player].send_no_permission_message();
            return;
        }

        match item.item_type.get_name() {
            "minecraft:ender_pearl" => {
                let owner = &self.players[player];
                let eye_height = if owner.crouching { 1.27 } else { 1.62 };
                let eye = Vec3::new(owner.pos.x, owner.pos.y + eye_height, owner.pos.z);
                let dir = Vec3::from_rotation(use_item.yaw, use_item.pitch);
                let uuid = owner.uuid;
                let context = cinnabar_world::game_event::EventContext {
                    sneaking: owner.crouching,
                    spectator: matches!(owner.gamemode, crate::player::Gamemode::Spectator),
                    dampened: false,
                };
                let shoot_pos = (owner.pos.x, owner.pos.y, owner.pos.z);
                self.world.game_event(
                    cinnabar_world::game_event::GameEvent::ProjectileShoot,
                    shoot_pos,
                    context,
                );
                self.spawn_entity(Entity::new(
                    EntityKind::EnderPearl { owner: uuid },
                    eye - Vec3::new(0.0, 0.1, 0.0),
                    dir * 1.5,
                ));
            }
            name => self.use_bucket(player, name, &use_item),
        }
    }

    fn handle_pick_item_from_block(&mut self, pick: SPickItemFromBlock, player: usize) {
        let block = self.world.get_block(BlockPos::new(pick.x, pick.y, pick.z));
        let name = match block {
            Block::RedstoneWire { .. } => "minecraft:redstone",
            _ => block.get_name(),
        };
        let Some(item_type) = Item::from_name(name) else {
            return;
        };

        // Hotbar slots are 36..=44 in the player inventory window
        let hotbar = |slot: usize| slot + 36;
        let players = &mut self.players[player];
        let existing = (0..9).find(|&slot| {
            players.inventory[hotbar(slot)]
                .as_ref()
                .is_some_and(|stack| stack.item_type.get_id() == item_type.get_id())
        });
        let slot = match existing {
            Some(slot) => slot,
            None => {
                // Prefer an empty hotbar slot, otherwise replace the selected one
                let slot = (0..9)
                    .find(|&slot| players.inventory[hotbar(slot)].is_none())
                    .unwrap_or(players.selected_slot as usize);
                let stack = ItemStack {
                    item_type,
                    count: 1,
                    nbt: None,
                };
                players.set_inventory_slot(hotbar(slot) as u32, Some(stack));
                slot
            }
        };

        players.send_packet(&CSetHeldItem { slot: slot as i8 }.encode());
        self.handle_set_held_item(SSetHeldItem { slot: slot as i16 }, player);
    }

    fn handle_player_input(&mut self, input: SPlayerInput, player: usize) {
        if self.players[player].crouching == input.shift {
            return;
        }
        self.players[player].crouching = input.shift;
        self.broadcast_player_state(player);
    }

    fn handle_set_held_item(&mut self, held_item_change: SSetHeldItem, player: usize) {
        let entity_equipment = CSetEquipment {
            entity_id: self.players[player].entity_id as i32,
            equipment: vec![CSetEquipmentEquipment {
                slot: 0, // Main hand
                item: self.players[player].inventory[held_item_change.slot as usize + 36]
                    .as_ref()
                    .map(utils::encode_slot_data),
            }],
        }
        .encode();
        for other_player in 0..self.players.len() {
            if player == other_player {
                continue;
            };
            self.players[other_player]
                .client
                .send_packet(&entity_equipment);
        }
        self.players[player].selected_slot = held_item_change.slot as u32;
    }

    fn handle_update_sign(&mut self, packet: SUpdateSign, _player: usize) {
        let pos = BlockPos::new(packet.x, packet.y, packet.z);
        let mut rows = packet
            .lines
            .iter()
            .map(|line| json!({ "text": line }).to_string());
        let rows = [
            rows.next().unwrap(),
            rows.next().unwrap(),
            rows.next().unwrap(),
            rows.next().unwrap(),
        ];
        let mut block_entity = match self.world.get_block_entity(pos) {
            Some(BlockEntity::Sign(sign)) => sign.as_ref().clone(),
            _ => Default::default(),
        };
        if packet.is_front_text {
            block_entity.front_rows = rows;
        } else {
            block_entity.back_rows = rows;
        }
        self.world
            .set_block_entity(pos, BlockEntity::Sign(Box::new(block_entity)));
        self.publish_world();
    }
}

impl Plot {
    /// Sends the crouching and sprinting state of a player to all other players.
    fn broadcast_player_state(&mut self, player: usize) {
        let mut bitfield = 0;
        if self.players[player].crouching {
            bitfield |= 0x02;
        };
        if self.players[player].sprinting {
            bitfield |= 0x08;
        };
        let metadata_entries = vec![
            CSetEntityMetadataEntry {
                index: 0,
                metadata_type: 0,
                value: vec![bitfield],
            },
            CSetEntityMetadataEntry {
                index: 6,
                metadata_type: 20,
                value: vec![if self.players[player].crouching { 5 } else { 0 }],
            },
        ];
        let entity_metadata = CSetEntityMetadata {
            entity_id: self.players[player].entity_id as i32,
            metadata: metadata_entries,
        }
        .encode();
        for other_player in 0..self.players.len() {
            if player == other_player {
                continue;
            };
            self.players[other_player]
                .client
                .send_packet(&entity_metadata);
        }
    }
}
