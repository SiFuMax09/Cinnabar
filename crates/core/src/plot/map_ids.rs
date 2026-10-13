//! World ids for client side map mods (JourneyMap, VoxelMap, Xaero's Minimap and World Map).
//!
//! These mods keep one map per "world" and have no way to tell two worlds apart on a server that
//! never changes dimension. Every plot is its own world here, so each plot gets its own id and
//! the maps of different plots no longer mix. Normally this needs a proxy or backend plugin
//! (WorldNamePacket, MapModCompanion); the server does it natively instead.
//!
//! JourneyMap and VoxelMap ask for the id themselves on `worldinfo:world_id`, Xaero's mods have to
//! be told on their own channels.

use crate::player::Player;
use cinnabar_network::packets::clientbound::{CPlayPluginMessage, ClientBoundPacket};

const WORLD_ID_CHANNEL: &str = "worldinfo:world_id";
const XAERO_MINIMAP_CHANNEL: &str = "xaerominimap:main";
const XAERO_WORLDMAP_CHANNEL: &str = "xaeroworldmap:main";

/// First byte of a request or response that is not zero padding.
const MAGIC_MARKER: u8 = 42;

/// The id of a plot: both coordinates packed into a non-negative `i32`. Plots further than
/// 16384 away from the origin would share an id with another plot.
pub fn plot_map_id(plot_x: i32, plot_z: i32) -> i32 {
    ((plot_x & 0x7fff) << 16) | (plot_z & 0xffff)
}

/// How a client framed its request. The response has to use the same framing.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct WorldIdRequest {
    /// Number of zero bytes the response starts with.
    response_padding: usize,
}

impl WorldIdRequest {
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

/// Tells Xaero's Minimap and World Map which plot the player is in.
pub fn send_xaero_ids(player: &Player, plot_x: i32, plot_z: i32) {
    let payload = xaero_payload(plot_map_id(plot_x, plot_z));
    for channel in [XAERO_MINIMAP_CHANNEL, XAERO_WORLDMAP_CHANNEL] {
        let packet = CPlayPluginMessage {
            channel: channel.to_string(),
            data: payload.clone(),
        }
        .encode();
        player.client.send_packet(&packet);
    }
}

/// Answers a plugin message if it is a world id request. Returns whether it was one.
pub fn handle_request(
    player: &Player,
    channel: &str,
    data: &[u8],
    plot_x: i32,
    plot_z: i32,
) -> bool {
    if channel != WORLD_ID_CHANNEL {
        return false;
    }
    if let Some(request) = WorldIdRequest::parse(data) {
        let packet = CPlayPluginMessage {
            channel: WORLD_ID_CHANNEL.to_string(),
            data: request.response(plot_map_id(plot_x, plot_z)),
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
    fn plot_ids_are_distinct_and_not_negative() {
        let mut seen = std::collections::HashSet::new();
        for x in -20..20 {
            for z in -20..20 {
                let id = plot_map_id(x, z);
                assert!(id >= 0);
                assert!(seen.insert(id), "duplicate id for plot ({x}, {z})");
            }
        }
        assert_eq!(plot_map_id(0, 0), 0);
        assert_eq!(plot_map_id(1, 2), (1 << 16) | 2);
    }

    #[test]
    fn journeymap_request_gets_a_prefixed_response() {
        let request = WorldIdRequest::parse(&[0, 42, 0]).unwrap();
        assert_eq!(request.response(1337), [&[0, 42, 4][..], b"1337"].concat());
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
