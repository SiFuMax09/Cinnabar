//! Live mirroring of the chunks next to a plot border.
//!
//! Every plot is its own thread and world, so a player standing near a border needs the chunks of
//! the neighbouring plot from that other thread. A plot publishes its edge chunks (the ones within
//! `view_distance` of one of its borders) as encoded chunk packets. The server broadcasts them, and
//! the neighbours forward them to their players that can see the chunks.

use super::{Plot, PLOT_WIDTH};
use crate::config::CONFIG;
use crate::server::Message;
use cinnabar_network::packets::PacketEncoder;
use cinnabar_world::World;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The minimum time between two publishes of changed edge chunks from one plot
const PUBLISH_INTERVAL: Duration = Duration::from_secs(1);

/// The edge chunks of plots that are shared with the neighbours, as received from the server
#[derive(Default)]
pub struct EdgeMirror {
    /// The latest packet of every neighbouring chunk that is within view of this plot
    received: FxHashMap<(i32, i32), Arc<PacketEncoder>>,
    /// Neighbours that were already asked for their edge chunks
    requested: FxHashSet<(i32, i32)>,
    /// When edge chunks were last published, used for debouncing
    last_publish: Option<Instant>,
}

impl EdgeMirror {
    pub fn received_chunk(&self, chunk_x: i32, chunk_z: i32) -> Option<&Arc<PacketEncoder>> {
        self.received.get(&(chunk_x, chunk_z))
    }
}

/// The view distance in chunks, clamped so that the ring around a plot stays within its neighbours
fn view_distance() -> i32 {
    (CONFIG.view_distance as i32).clamp(0, PLOT_WIDTH)
}

/// Whether an owned chunk is within `view_distance` chunks of a border of its plot.
/// Chunks that are not in the plot are never edge chunks.
pub fn is_edge_chunk(plot: (i32, i32), chunk: (i32, i32), view_distance: i32) -> bool {
    if !Plot::chunk_in_plot_bounds(plot.0, plot.1, chunk.0, chunk.1) {
        return false;
    }
    let local_x = chunk.0 - plot.0 * PLOT_WIDTH;
    let local_z = chunk.1 - plot.1 * PLOT_WIDTH;
    local_x < view_distance
        || local_x >= PLOT_WIDTH - view_distance
        || local_z < view_distance
        || local_z >= PLOT_WIDTH - view_distance
}

/// All edge chunks of a plot, as absolute chunk coordinates
pub fn edge_chunks_of_plot(plot: (i32, i32), view_distance: i32) -> Vec<(i32, i32)> {
    let mut chunks = Vec::new();
    for local_x in 0..PLOT_WIDTH {
        for local_z in 0..PLOT_WIDTH {
            let chunk = (plot.0 * PLOT_WIDTH + local_x, plot.1 * PLOT_WIDTH + local_z);
            if is_edge_chunk(plot, chunk, view_distance) {
                chunks.push(chunk);
            }
        }
    }
    chunks
}

/// Whether `other` is one of the 8 plots surrounding `own`
pub fn is_neighbour(own: (i32, i32), other: (i32, i32)) -> bool {
    let (dx, dz) = (other.0 - own.0, other.1 - own.1);
    (dx, dz) != (0, 0) && dx.abs() <= 1 && dz.abs() <= 1
}

/// Whether a chunk of a neighbouring plot is within `view_distance` chunks of `own`, which means a
/// player of `own` can see it.
pub fn in_neighbour_ring(own: (i32, i32), chunk: (i32, i32), view_distance: i32) -> bool {
    if Plot::chunk_in_plot_bounds(own.0, own.1, chunk.0, chunk.1) {
        return false;
    }
    let min_x = own.0 * PLOT_WIDTH - view_distance;
    let min_z = own.1 * PLOT_WIDTH - view_distance;
    let max_x = (own.0 + 1) * PLOT_WIDTH - 1 + view_distance;
    let max_z = (own.1 + 1) * PLOT_WIDTH - 1 + view_distance;
    (min_x..=max_x).contains(&chunk.0) && (min_z..=max_z).contains(&chunk.1)
}

/// The neighbouring plots whose border is within `view_distance` chunks of a player's chunk
pub fn neighbours_in_view(
    plot: (i32, i32),
    chunk: (i32, i32),
    view_distance: i32,
) -> Vec<(i32, i32)> {
    let mut neighbours = Vec::new();
    for dx in -1..=1 {
        for dz in -1..=1 {
            if (dx, dz) == (0, 0) {
                continue;
            }
            let near_x = match dx {
                1 => chunk.0 >= (plot.0 + 1) * PLOT_WIDTH - view_distance,
                -1 => chunk.0 < plot.0 * PLOT_WIDTH + view_distance,
                _ => true,
            };
            let near_z = match dz {
                1 => chunk.1 >= (plot.1 + 1) * PLOT_WIDTH - view_distance,
                -1 => chunk.1 < plot.1 * PLOT_WIDTH + view_distance,
                _ => true,
            };
            if near_x && near_z {
                neighbours.push((plot.0 + dx, plot.1 + dz));
            }
        }
    }
    neighbours
}

/// Whether enough time has passed since the last publish to publish again
pub fn publish_due(last_publish: Option<Instant>, now: Instant) -> bool {
    match last_publish {
        None => true,
        Some(last) => now.saturating_duration_since(last) >= PUBLISH_INTERVAL,
    }
}

impl Plot {
    /// Encodes the given owned chunks and sends them to the server for broadcasting
    fn send_edge_chunks(&mut self, chunks: &[(i32, i32)]) {
        let encoded: Vec<_> = chunks
            .iter()
            .filter_map(|&(chunk_x, chunk_z)| {
                let chunk = self.world.get_chunk(chunk_x, chunk_z)?;
                Some((chunk_x, chunk_z, Arc::new(chunk.encode_packet())))
            })
            .collect();
        if encoded.is_empty() {
            return;
        }
        let message = Message::EdgeChunks {
            plot_x: self.world.x,
            plot_z: self.world.z,
            chunks: encoded,
        };
        let _ = self.message_sender.send(message);
    }

    /// Publishes every edge chunk of this plot. Done when the plot starts, and when a neighbour asks.
    pub(super) fn publish_all_edge_chunks(&mut self) {
        let plot = (self.world.x, self.world.z);
        let chunks = edge_chunks_of_plot(plot, view_distance());
        self.world.edge_dirty_chunks.clear();
        self.send_edge_chunks(&chunks);
        self.edge_mirror.last_publish = Some(Instant::now());
    }

    /// Republishes the edge chunks that changed since the last publish, at most once per interval.
    pub(super) fn publish_dirty_edge_chunks(&mut self, now: Instant) {
        if self.world.edge_dirty_chunks.is_empty()
            || !publish_due(self.edge_mirror.last_publish, now)
        {
            return;
        }
        let plot = (self.world.x, self.world.z);
        let view_distance = view_distance();
        let chunks: Vec<_> = self
            .world
            .edge_dirty_chunks
            .drain()
            .filter(|&chunk| is_edge_chunk(plot, chunk, view_distance))
            .collect();
        if chunks.is_empty() {
            return;
        }
        self.edge_mirror.last_publish = Some(now);
        self.send_edge_chunks(&chunks);
    }

    /// Asks the neighbours that a player can see for their edge chunks, once per neighbour
    pub(super) fn request_neighbour_edge_chunks(&mut self, player_idx: usize) {
        let plot = (self.world.x, self.world.z);
        let player = &self.players[player_idx];
        let chunk = (player.last_chunk_x, player.last_chunk_z);
        for neighbour in neighbours_in_view(plot, chunk, view_distance()) {
            if self.edge_mirror.requested.insert(neighbour) {
                let _ = self.message_sender.send(Message::RequestEdgeChunks {
                    from_plot: plot,
                    to_plot: neighbour,
                });
            }
        }
    }

    /// Stores the edge chunks of a neighbour and forwards them to the players that can see them.
    /// Only the chunks that changed are sent, so unchanged chunks are never sent again.
    pub(super) fn handle_edge_chunks(
        &mut self,
        plot_x: i32,
        plot_z: i32,
        chunks: Vec<(i32, i32, Arc<PacketEncoder>)>,
    ) {
        let own = (self.world.x, self.world.z);
        if !is_neighbour(own, (plot_x, plot_z)) {
            return;
        }
        let view_distance = view_distance();
        for (chunk_x, chunk_z, packet) in chunks {
            if !in_neighbour_ring(own, (chunk_x, chunk_z), view_distance) {
                continue;
            }
            for player in &mut self.players {
                let distance = Plot::get_chunk_distance(
                    chunk_x,
                    chunk_z,
                    player.last_chunk_x,
                    player.last_chunk_z,
                );
                if distance <= view_distance as u32 {
                    player.client.send_packet(&packet);
                }
            }
            self.edge_mirror.received.insert((chunk_x, chunk_z), packet);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VD: i32 = 8;

    #[test]
    fn edge_chunks_are_the_border_ring() {
        let plot = (0, 0);
        let edges = edge_chunks_of_plot(plot, VD);
        // 32x32 chunks minus the inner 16x16 area
        assert_eq!(edges.len(), 32 * 32 - 16 * 16);
        assert!(edges.contains(&(0, 0)));
        assert!(edges.contains(&(31, 31)));
        assert!(edges.contains(&(7, 16)));
        assert!(!edges.contains(&(8, 8)));
        assert!(!edges.contains(&(23, 23)));
        assert!(edges.contains(&(24, 24)));
    }

    #[test]
    fn edge_chunks_follow_plot_position() {
        // Plot (-1, 2) owns chunks x in [-32, -1] and z in [64, 95]
        assert!(is_edge_chunk((-1, 2), (-32, 64), VD));
        assert!(is_edge_chunk((-1, 2), (-1, 95), VD));
        assert!(!is_edge_chunk((-1, 2), (-16, 80), VD));
        assert!(!is_edge_chunk((-1, 2), (0, 80), VD), "not owned");
        assert!(!is_edge_chunk((-1, 2), (-33, 80), VD), "not owned");
    }

    #[test]
    fn no_edges_without_view_distance() {
        assert!(edge_chunks_of_plot((0, 0), 0).is_empty());
        assert_eq!(edge_chunks_of_plot((0, 0), 32).len(), 32 * 32);
    }

    #[test]
    fn neighbours_are_the_eight_surrounding_plots() {
        let own = (3, -2);
        let mut found = 0;
        for dx in -2i32..=2 {
            for dz in -2i32..=2 {
                let other = (own.0 + dx, own.1 + dz);
                let expected = (dx, dz) != (0, 0) && dx.abs() <= 1 && dz.abs() <= 1;
                assert_eq!(is_neighbour(own, other), expected, "{other:?}");
                found += expected as u32;
            }
        }
        assert_eq!(found, 8);
        assert!(!is_neighbour(own, own));
    }

    #[test]
    fn neighbour_ring_is_outside_plot_and_within_view() {
        let own = (0, 0);
        assert!(!in_neighbour_ring(own, (5, 5), VD), "owned chunk");
        assert!(in_neighbour_ring(own, (32, 5), VD));
        assert!(in_neighbour_ring(own, (32 + VD - 1, 5), VD));
        assert!(!in_neighbour_ring(own, (32 + VD, 5), VD));
        assert!(in_neighbour_ring(own, (-VD, -VD), VD));
        assert!(!in_neighbour_ring(own, (-VD - 1, 5), VD));
    }

    #[test]
    fn ring_chunks_are_edge_chunks_of_the_neighbour() {
        // Everything a player of `own` can see in a neighbour must be published by that neighbour
        let own = (0, 0);
        for dx in -1..=1 {
            for dz in -1..=1 {
                if (dx, dz) == (0, 0) {
                    continue;
                }
                let neighbour = (dx, dz);
                for local_x in 0..PLOT_WIDTH {
                    for local_z in 0..PLOT_WIDTH {
                        let chunk = (
                            neighbour.0 * PLOT_WIDTH + local_x,
                            neighbour.1 * PLOT_WIDTH + local_z,
                        );
                        if in_neighbour_ring(own, chunk, VD) {
                            assert!(is_edge_chunk(neighbour, chunk, VD), "{chunk:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn neighbours_in_view_depends_on_the_border() {
        // Plot (0, 0): chunk (31, 16) is near the +x border only
        assert_eq!(neighbours_in_view((0, 0), (31, 16), VD), vec![(1, 0)]);
        // the corner is near three neighbours
        let mut corner = neighbours_in_view((0, 0), (31, 31), VD);
        corner.sort();
        assert_eq!(corner, vec![(0, 1), (1, 0), (1, 1)]);
        // the middle of the plot is near nothing
        assert!(neighbours_in_view((0, 0), (16, 16), VD).is_empty());
        // the west border of plot (0, 0) is chunk x 0..8
        assert_eq!(neighbours_in_view((0, 0), (0, 16), VD), vec![(-1, 0)]);
    }

    #[test]
    fn publish_is_debounced() {
        let now = Instant::now();
        assert!(publish_due(None, now));
        assert!(!publish_due(Some(now), now));
        assert!(!publish_due(Some(now), now + Duration::from_millis(999)));
        assert!(publish_due(Some(now), now + PUBLISH_INTERVAL));
    }
}
