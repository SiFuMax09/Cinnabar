//! Server side visualization of the WorldEdit selection for clients without WorldEditCUI. The
//! selection is outlined with dust particles that are only sent to the player owning it.

use super::Plot;
use crate::player::Player;
use cinnabar_blocks::BlockPos;
use cinnabar_network::packets::clientbound::CDustParticle;
use cinnabar_network::packets::clientbound::ClientBoundPacket;
use cinnabar_text::{ColorCode, TextComponentBuilder};
use std::time::{Duration, Instant};

/// Dust lives for roughly 0.5 to 2 seconds, so refreshing a few times a second keeps the
/// outline visually continuous.
const REDRAW_INTERVAL: Duration = Duration::from_millis(250);
/// Upper bound of particles per redraw for the cuboid outline. Every particle is its own packet.
const BOX_MAX_POINTS: usize = 200;
/// Points further away than this from the player are not sent.
const VIEW_DISTANCE: f64 = 64.0;

const COLOR_BOX: u32 = 0x55FFFF;
const COLOR_POS1: u32 = 0xFF5555;
const COLOR_POS2: u32 = 0x5555FF;

type Point = (f64, f64, f64);
type Corner = (i32, i32, i32);

#[derive(Debug)]
pub struct SelectionVisualizer {
    /// Toggled with `//visualize`.
    pub enabled: bool,
    /// The player has WorldEditCUI, which renders the selection itself.
    pub cui: bool,
    /// The selection as of the last update, used to detect changes.
    last_selection: (Option<BlockPos>, Option<BlockPos>),
    last_draw: Option<Instant>,
}

impl Default for SelectionVisualizer {
    fn default() -> Self {
        Self {
            enabled: true,
            cui: false,
            last_selection: (None, None),
            last_draw: None,
        }
    }
}

fn corner(pos: BlockPos) -> Corner {
    (pos.x, pos.y, pos.z)
}

/// Dimensions of the cuboid spanned by the two positions, inclusive of both.
pub fn selection_size(p1: BlockPos, p2: BlockPos) -> (u32, u32, u32) {
    (
        p1.x.abs_diff(p2.x) + 1,
        p1.y.abs_diff(p2.y) + 1,
        p1.z.abs_diff(p2.z) + 1,
    )
}

/// Points along the 12 edges of the box covering the blocks from `min` to `max` (both inclusive).
/// The 8 corners are always included. Along the edges, points are placed at least `min_spacing`
/// blocks apart and the spacing is increased as needed to stay within `max_points`.
pub fn box_edge_points(
    min: Corner,
    max: Corner,
    min_spacing: f64,
    max_points: usize,
) -> Vec<Point> {
    let lo = [min.0 as f64, min.1 as f64, min.2 as f64];
    let hi = [(max.0 + 1) as f64, (max.1 + 1) as f64, (max.2 + 1) as f64];
    let len = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];

    // Four parallel edges per axis
    let total_len = 4.0 * (len[0] + len[1] + len[2]);
    let budget = max_points.saturating_sub(8).max(1) as f64;
    let spacing = min_spacing.max(total_len / budget);

    let mut points = Vec::new();
    for x in [lo[0], hi[0]] {
        for y in [lo[1], hi[1]] {
            for z in [lo[2], hi[2]] {
                points.push((x, y, z));
            }
        }
    }
    for axis in 0..3 {
        let segments = (len[axis] / spacing).ceil().max(1.0) as usize;
        let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
        for ca in [lo[a], hi[a]] {
            for cb in [lo[b], hi[b]] {
                for i in 1..segments {
                    let mut p = [0.0; 3];
                    p[axis] = lo[axis] + len[axis] * i as f64 / segments as f64;
                    p[a] = ca;
                    p[b] = cb;
                    points.push((p[0], p[1], p[2]));
                }
            }
        }
    }
    points
}

/// Everything that should be drawn for the selection, with its colour.
pub fn selection_points(
    first: Option<BlockPos>,
    second: Option<BlockPos>,
    viewer: Point,
) -> Vec<(Point, u32)> {
    let mut out: Vec<(Point, u32)> = Vec::new();
    let mut marker = |pos: BlockPos, color: u32| {
        let c = corner(pos);
        out.extend(
            box_edge_points(c, c, 0.5, 40)
                .into_iter()
                .map(|p| (p, color)),
        );
    };
    match (first, second) {
        (None, None) => return out,
        (Some(p), None) => marker(p, COLOR_POS1),
        (None, Some(p)) => marker(p, COLOR_POS2),
        (Some(p1), Some(p2)) => {
            marker(p1, COLOR_POS1);
            marker(p2, COLOR_POS2);
            let min = (p1.x.min(p2.x), p1.y.min(p2.y), p1.z.min(p2.z));
            let max = (p1.x.max(p2.x), p1.y.max(p2.y), p1.z.max(p2.z));
            out.extend(
                box_edge_points(min, max, 1.0, BOX_MAX_POINTS)
                    .into_iter()
                    .map(|p| (p, COLOR_BOX)),
            );
        }
    }
    out.retain(|((x, y, z), _)| {
        let (dx, dy, dz) = (x - viewer.0, y - viewer.1, z - viewer.2);
        dx * dx + dy * dy + dz * dz <= VIEW_DISTANCE * VIEW_DISTANCE
    });
    out
}

impl Player {
    fn send_action_bar(&self, message: &str) {
        let packet = cinnabar_network::packets::clientbound::CSystemChatMessage {
            content: TextComponentBuilder::new(message.to_string())
                .color_code(ColorCode::LightPurple)
                .finish(),
            overlay: true,
        }
        .encode();
        self.client.send_packet(&packet);
    }
}

impl Plot {
    /// Shows the selection size in the action bar when the selection changes and keeps the
    /// particle outline of the selection alive.
    pub(super) fn update_selection_visuals(&mut self, player_idx: usize) {
        let player = &mut self.players[player_idx];
        let selection = (player.first_position, player.second_position);
        let viz = &mut player.selection_viz;
        if viz.last_selection != selection {
            viz.last_selection = selection;
            viz.last_draw = None;
            if let (Some(p1), Some(p2)) = selection {
                let (x, y, z) = selection_size(p1, p2);
                let blocks = u64::from(x) * u64::from(y) * u64::from(z);
                player.send_action_bar(&format!("Selection: {x} x {y} x {z} ({blocks} blocks)"));
            }
        }

        let viz = &mut player.selection_viz;
        if !viz.enabled || viz.cui || (selection.0.is_none() && selection.1.is_none()) {
            return;
        }
        if viz.last_draw.is_some_and(|t| t.elapsed() < REDRAW_INTERVAL) {
            return;
        }
        viz.last_draw = Some(Instant::now());

        let pos = player.pos;
        for ((x, y, z), color) in selection_points(selection.0, selection.1, (pos.x, pos.y, pos.z))
        {
            let packet = CDustParticle {
                x,
                y,
                z,
                color,
                scale: 0.8,
            }
            .encode();
            player.client.send_packet(&packet);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on_edge(p: Point, min: Corner, max: Corner) -> bool {
        let lo = [min.0 as f64, min.1 as f64, min.2 as f64];
        let hi = [(max.0 + 1) as f64, (max.1 + 1) as f64, (max.2 + 1) as f64];
        let p = [p.0, p.1, p.2];
        let at_bound = (0..3).filter(|&i| p[i] == lo[i] || p[i] == hi[i]).count();
        let inside = (0..3).all(|i| p[i] >= lo[i] && p[i] <= hi[i]);
        inside && at_bound >= 2
    }

    #[test]
    fn single_block_has_corners_and_midpoints() {
        let points = box_edge_points((3, 4, 5), (3, 4, 5), 0.5, 40);
        // 8 corners and the midpoints of 12 edges
        assert_eq!(points.len(), 20);
        assert!(points.contains(&(3.0, 4.0, 5.0)));
        assert!(points.contains(&(4.0, 5.0, 6.0)));
        assert!(points.contains(&(3.5, 4.0, 5.0)));
    }

    #[test]
    fn points_lie_on_edges() {
        let (min, max) = ((0, 0, 0), (4, 2, 7));
        let points = box_edge_points(min, max, 1.0, 1000);
        assert!(points.iter().all(|&p| on_edge(p, min, max)));
        // 8 corners + 4 * (4 + 2 + 7) interior points at unit spacing
        assert_eq!(points.len(), 8 + 4 * 13);
    }

    #[test]
    fn negative_coordinates() {
        let (min, max) = ((-3, -1, -2), (-1, 0, 0));
        let points = box_edge_points(min, max, 1.0, 1000);
        assert!(points.iter().all(|&p| on_edge(p, min, max)));
        assert!(points.contains(&(-3.0, -1.0, -2.0)));
        assert!(points.contains(&(0.0, 1.0, 1.0)));
    }

    #[test]
    fn huge_selection_is_capped() {
        let points = box_edge_points((0, 0, 0), (9999, 255, 9999), 1.0, 200);
        assert!(points.len() <= 200, "{} points", points.len());
        assert!(points.len() >= 8);
        assert!(points
            .iter()
            .all(|&p| on_edge(p, (0, 0, 0), (9999, 255, 9999))));
    }

    #[test]
    fn selection_size_is_inclusive() {
        let a = BlockPos::new(1, 5, -2);
        let b = BlockPos::new(-1, 5, 2);
        assert_eq!(selection_size(a, b), (3, 1, 5));
        assert_eq!(selection_size(a, a), (1, 1, 1));
    }

    #[test]
    fn selection_points_cases() {
        let origin = (0.0, 0.0, 0.0);
        assert!(selection_points(None, None, origin).is_empty());

        let one = selection_points(Some(BlockPos::new(1, 1, 1)), None, origin);
        assert!(!one.is_empty());
        assert!(one.iter().all(|&(_, c)| c == COLOR_POS1));

        let both = selection_points(
            Some(BlockPos::new(0, 0, 0)),
            Some(BlockPos::new(10, 10, 10)),
            origin,
        );
        assert!(both.iter().any(|&(_, c)| c == COLOR_BOX));
        assert!(both.iter().any(|&(_, c)| c == COLOR_POS1));
        assert!(both.iter().any(|&(_, c)| c == COLOR_POS2));
        assert!(both.len() <= BOX_MAX_POINTS + 80);
    }

    #[test]
    fn far_points_are_culled() {
        let far = selection_points(
            Some(BlockPos::new(1000, 0, 0)),
            Some(BlockPos::new(1010, 10, 10)),
            (0.0, 0.0, 0.0),
        );
        assert!(far.is_empty());
    }
}
