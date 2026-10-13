//! World id for client side map mods (JourneyMap, VoxelMap, Xaero's Minimap and World Map).
//!
//! These mods keep one map per "world" and tell worlds apart by an id the server has to provide.
//! The whole server is one big world, so every player gets the same id and all plots share one
//! map. Normally this needs a proxy or backend plugin (WorldNamePacket, MapModCompanion); the
//! server does it natively instead.
//!
//! JourneyMap and VoxelMap ask for the id themselves on `worldinfo:world_id`, Xaero's mods have to
//! be told on their own channels.

use crate::config::CONFIG;
use crate::player::Player;
use cinnabar_network::packets::clientbound::{CPlayPluginMessage, ClientBoundPacket};

const WORLD_ID_CHANNEL: &str = "worldinfo:world_id";
const XAERO_MINIMAP_CHANNEL: &str = "xaerominimap:main";
const XAERO_WORLDMAP_CHANNEL: &str = "xaeroworldmap:main";

/// First byte of a request or response that is not zero padding.
const MAGIC_MARKER: u8 = 42;

/// The id of the one world every player is in. Configurable so that it can be told apart from the
/// ids other servers behind the same proxy send; ids outside the `i32` range are ignored.
fn world_map_id() -> i32 {
    i32::try_from(CONFIG.map_world_id).unwrap_or(DEFAULT_WORLD_MAP_ID)
}

/// "CINN"; unlikely to collide with the ids other servers or proxies hand out.
const DEFAULT_WORLD_MAP_ID: i32 = 0x43_49_4e_4e;

/// How a client framed its request. The response has to use the same framing.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct WorldIdRequest {
    /// Number of zero bytes the response starts with.
    response_padding: usize,
}

impl WorldIdRequest {
    /// The framing current JourneyMap and VoxelMap versions expect: one zero byte, then the marker.
    const UNPROMPTED: WorldIdRequest = WorldIdRequest {
        response_padding: 1,
    };

    /// Parses a request the way JourneyMap and VoxelMap send it:
    /// `00 2A 00` (JourneyMap, VoxelMap Fabric 1.19.3+) or `00 00 00 2A` (older VoxelMap Fabric).
    pub fn parse(data: &[u8]) -> Option<WorldIdRequest> {
        let padding = data.iter().take_while(|&&b| b == 0).count();
        if data.get(padding) != Some(&MAGIC_MARKER) {
            return None;
        }
        let response_padding = match padding {
            1 => 1,
            3 => 0,
            _ => return None,
        };
        Some(WorldIdRequest { response_padding })
    }

    /// `[padding zeros] 2A <length> <id as decimal text>`
    pub fn response(self, id: i32) -> Vec<u8> {
        let text = id.to_string();
        let mut out = vec![0; self.response_padding];
        out.push(MAGIC_MARKER);
        out.push(text.len() as u8);
        out.extend_from_slice(text.as_bytes());
        out
    }
}

/// Xaero's `LevelMapProperties`: a zero marker followed by the id as big endian `i32`.
fn xaero_payload(id: i32) -> Vec<u8> {
    let mut out = vec![0];
    out.extend_from_slice(&id.to_be_bytes());
    out
}

/// Tells the map mods which world the player is in without waiting for a request, the way the
/// JourneyMap server plugin does on join: Xaero's Minimap and World Map get their own packet,
/// JourneyMap and VoxelMap get the answer to a request they might not have sent yet.
pub fn send_ids(player: &Player) {
    let id = world_map_id();
    let xaero = xaero_payload(id);
    let channels = [
        (XAERO_MINIMAP_CHANNEL, xaero.clone()),
        (XAERO_WORLDMAP_CHANNEL, xaero),
        (WORLD_ID_CHANNEL, WorldIdRequest::UNPROMPTED.response(id)),
    ];
    for (channel, data) in channels {
        let packet = CPlayPluginMessage {
            channel: channel.to_string(),
            data,
        }
        .encode();
        player.client.send_packet(&packet);
    }
}

/// Answers a plugin message if it is a world id request. Returns whether it was one.
pub fn handle_request(player: &Player, channel: &str, data: &[u8]) -> bool {
    if channel != WORLD_ID_CHANNEL {
        return false;
    }
    if let Some(request) = WorldIdRequest::parse(data) {
        let packet = CPlayPluginMessage {
            channel: WORLD_ID_CHANNEL.to_string(),
            data: request.response(world_map_id()),
        }
        .encode();
        player.client.send_packet(&packet);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journeymap_request_gets_a_prefixed_response() {
        let request = WorldIdRequest::parse(&[0, 42, 0]).unwrap();
        assert_eq!(request.response(1337), [&[0, 42, 4][..], b"1337"].concat());
    }

    #[test]
    fn unprompted_response_uses_the_current_framing() {
        assert_eq!(
            WorldIdRequest::UNPROMPTED.response(1),
            [&[0, 42, 1][..], b"1"].concat()
        );
    }

    #[test]
    fn old_voxelmap_fabric_request_gets_an_unpadded_response() {
        let request = WorldIdRequest::parse(&[0, 0, 0, 42]).unwrap();
        assert_eq!(request.response(7), [&[42, 1][..], b"7"].concat());
    }

    #[test]
    fn other_requests_are_ignored() {
        assert_eq!(WorldIdRequest::parse(&[]), None);
        assert_eq!(WorldIdRequest::parse(&[0, 0]), None);
        assert_eq!(WorldIdRequest::parse(&[0, 0, 0]), None);
        assert_eq!(WorldIdRequest::parse(&[0, 7, 0]), None);
        assert_eq!(WorldIdRequest::parse(&[0, 0, 42]), None);
        assert_eq!(WorldIdRequest::parse(&[42]), None);
    }

    #[test]
    fn xaero_payload_is_a_zero_marker_and_a_big_endian_id() {
        assert_eq!(xaero_payload(0x0102_0304), [0, 1, 2, 3, 4]);
    }
}
