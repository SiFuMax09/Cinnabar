//! Block patterns and masks for the WorldEdit commands.
//!
//! A pattern decides which block is placed, a mask decides which positions are affected.
//! Everything works on raw block state ids; masks that only look at the block they are tested
//! on (`stone,dirt`, `!air`, `oak_*`, ...) are compiled into a bit set, so testing a block is a
//! single bit lookup.
//!
//! Pattern syntax (`,` separates alternatives):
//! - `stone`, `minecraft:stone`, `oak_stairs[facing=north,half=top]`
//! - `50%stone,30%dirt,gravel`: random choice, weights default to 1
//! - `^oak_stairs`: place the block but keep the properties the old block had (when it has them)
//! - `#clipboard`: the blocks of the clipboard, repeated over the area
//! - `=123`: a raw block state id
//!
//! Mask syntax (`,` is "any of", `&` is "all of", `&` binds tighter):
//! - blocks like in patterns; without properties every state of the block matches and with
//!   properties only the states that have them. `*` is a wildcard: `*_stairs`, `oak_*`
//! - `!mask`: not, `>mask`: the block above matches, `<mask`: the block below matches
//! - `#existing` (not air), `#air`, `#solid`, `##wool` (block tags), `%50` (50% chance)

use cinnabar_blocks::blocks::Block;
use cinnabar_blocks::{generic, BlockPos};
use cinnabar_schematic::WorldEditClipboard;
use cinnabar_world::World;
use rand::RngExt;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::RefCell;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, LazyLock, OnceLock};

#[derive(Debug)]
pub enum PatternParseError {
    UnknownBlock(String),
    UnknownTag(String),
    InvalidProperty(String),
    InvalidPattern(String),
}

impl fmt::Display for PatternParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatternParseError::UnknownBlock(block) => write!(f, "unknown block: {}", block),
            PatternParseError::UnknownTag(tag) => write!(f, "unknown block tag: {}", tag),
            PatternParseError::InvalidProperty(prop) => write!(f, "invalid property: {}", prop),
            PatternParseError::InvalidPattern(pattern) => write!(f, "invalid pattern: {}", pattern),
        }
    }
}

impl std::error::Error for PatternParseError {}

pub type PatternParseResult<T> = Result<T, PatternParseError>;

/// A small and fast random number generator (xorshift64*). Patterns pick a block for every
/// single block of a big operation, the thread rng is too slow for that.
#[derive(Clone, Debug)]
pub struct FastRng(u64);

impl FastRng {
    pub fn new() -> FastRng {
        FastRng::seeded(rand::rng().random::<u64>())
    }

    pub fn seeded(seed: u64) -> FastRng {
        FastRng(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A float in `0.0..1.0`
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// A number in `0..bound`
    pub fn below(&mut self, bound: u32) -> u32 {
        ((self.next_u64() >> 32) * bound as u64 >> 32) as u32
    }
}

impl Default for FastRng {
    fn default() -> Self {
        FastRng::new()
    }
}

/// Everything a pattern may need besides the position and the old block
pub struct PatternEnv<'a> {
    pub rng: FastRng,
    pub clipboard: Option<&'a WorldEditClipboard>,
    /// The position of the corner of the clipboard, used by `#clipboard`
    pub origin: BlockPos,
    keep_cache: FxHashMap<(u32, u32), u32>,
    noise_seed: u64,
    linear_counter: u64,
    buffer: FxHashSet<(i32, i32, i32)>,
    /// Read access to the world for the patterns that look at neighbouring blocks
    pub view: Option<&'a dyn BlockView>,
}

impl<'a> PatternEnv<'a> {
    pub fn new(clipboard: Option<&'a WorldEditClipboard>, origin: BlockPos) -> PatternEnv<'a> {
        let mut rng = FastRng::new();
        let noise_seed = rng.next_u64();
        PatternEnv {
            rng,
            clipboard,
            origin,
            keep_cache: FxHashMap::default(),
            noise_seed,
            linear_counter: 0,
            buffer: FxHashSet::default(),
            view: None,
        }
    }

    /// Gives the patterns that need it (`#spread`, `#surfacespread`, `#anglecolor`, ...) access to
    /// the world
    pub fn with_view(mut self, view: Option<&'a dyn BlockView>) -> PatternEnv<'a> {
        self.view = view;
        self
    }
}

/// Read access to the blocks of a world, used by masks that look at neighbouring blocks
pub trait BlockView {
    fn block_id(&self, pos: BlockPos) -> u32;

    fn sky_light(&self, _pos: BlockPos) -> u8 {
        15
    }

    fn block_light(&self, _pos: BlockPos) -> u8 {
        0
    }
}

impl<W: World> BlockView for W {
    fn block_id(&self, pos: BlockPos) -> u32 {
        self.get_block_raw(pos)
    }

    fn sky_light(&self, pos: BlockPos) -> u8 {
        match self.get_chunk(pos.x >> 4, pos.z >> 4) {
            Some(chunk) if pos.y >= 0 && ((pos.y >> 4) as usize) < chunk.light.num_sections() => {
                chunk
                    .light
                    .sky_light((pos.x & 15) as u32, pos.y as u32, (pos.z & 15) as u32)
            }
            _ => 15,
        }
    }

    fn block_light(&self, pos: BlockPos) -> u8 {
        match self.get_chunk(pos.x >> 4, pos.z >> 4) {
            Some(chunk) if pos.y >= 0 && ((pos.y >> 4) as usize) < chunk.light.num_sections() => {
                chunk
                    .light
                    .block_light((pos.x & 15) as u32, pos.y as u32, (pos.z & 15) as u32)
            }
            _ => 0,
        }
    }
}

/// A copy of a box of the world, for the patterns that look at the neighbours of the blocks they
/// replace. Everything outside of the box is air.
pub struct Snapshot {
    min: BlockPos,
    size: (usize, usize, usize),
    data: Vec<u16>,
}

impl Snapshot {
    /// `data` is in the order of `read_box`: x, then z, then y
    pub fn new(min: BlockPos, max: BlockPos, data: Vec<u16>) -> Snapshot {
        Snapshot {
            min,
            size: (
                (max.x - min.x + 1) as usize,
                (max.y - min.y + 1) as usize,
                (max.z - min.z + 1) as usize,
            ),
            data,
        }
    }
}

impl BlockView for Snapshot {
    fn block_id(&self, pos: BlockPos) -> u32 {
        let (x, y, z) = (pos.x - self.min.x, pos.y - self.min.y, pos.z - self.min.z);
        if x < 0
            || y < 0
            || z < 0
            || x as usize >= self.size.0
            || y as usize >= self.size.1
            || z as usize >= self.size.2
        {
            return 0;
        }
        self.data[((y as usize * self.size.2) + z as usize) * self.size.0 + x as usize] as u32
    }
}

/// What the selection and the position of the player are when a mask is parsed, for `#region`,
/// `#id`, `#xaxis`, `{` and friends
#[derive(Clone, Copy, Debug, Default)]
pub struct ParseContext {
    pub origin: Option<BlockPos>,
    pub region: Option<(BlockPos, BlockPos)>,
}

thread_local! {
    static PARSE_CONTEXT: RefCell<ParseContext> = RefCell::new(ParseContext::default());
}

/// Sets the context the following [`Mask::parse`] calls on this thread use
pub fn set_parse_context(context: ParseContext) {
    PARSE_CONTEXT.with(|c| *c.borrow_mut() = context);
}

fn parse_context() -> ParseContext {
    PARSE_CONTEXT.with(|c| *c.borrow())
}

/// The y of the highest non air block of the column at `(x, z)` near `y`
fn surface_y<V: BlockView + ?Sized>(view: &V, x: i32, y: i32, z: i32, range: i32) -> Option<i32> {
    if view.block_id(BlockPos::new(x, y, z)) != 0 {
        for dy in 1..=range {
            if view.block_id(BlockPos::new(x, y + dy, z)) == 0 {
                return Some(y + dy - 1);
            }
        }
        None
    } else {
        for dy in 1..=range {
            if view.block_id(BlockPos::new(x, y - dy, z)) != 0 {
                return Some(y - dy);
            }
        }
        None
    }
}

const ANGLE_RANGE: i32 = 16;

/// The biggest height difference to one of the four neighbour columns, `distance` blocks away
fn max_height_step<V: BlockView + ?Sized>(view: &V, pos: BlockPos, distance: i32) -> f32 {
    let here = surface_y(view, pos.x, pos.y, pos.z, ANGLE_RANGE).unwrap_or(pos.y);
    let mut best = 0.0f32;
    for d in HORIZONTAL {
        let (nx, nz) = (pos.x + d[0] * distance, pos.z + d[2] * distance);
        let step = match surface_y(view, nx, here, nz, ANGLE_RANGE) {
            Some(y) => (y - here).abs() as f32,
            None => ANGLE_RANGE as f32,
        };
        best = best.max(step / distance as f32);
    }
    best
}

fn terrain_angle<V: BlockView + ?Sized>(view: &V, pos: BlockPos, distance: i32) -> f32 {
    max_height_step(view, pos, distance).atan().to_degrees()
}

/// The names of all block states, indexed by state id
fn state_names() -> &'static [&'static str] {
    static NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        (0..generic::state_count())
            .map(|id| Block::from_id(id).get_name())
            .collect()
    });
    &NAMES
}

/// A set of block state ids
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateSet {
    words: Vec<u64>,
}

impl StateSet {
    fn new() -> StateSet {
        StateSet {
            words: vec![0; (generic::state_count() as usize).div_ceil(64)],
        }
    }

    pub fn single(id: u32) -> StateSet {
        let mut set = StateSet::new();
        set.insert(id);
        set
    }

    fn insert(&mut self, id: u32) {
        if let Some(word) = self.words.get_mut(id as usize >> 6) {
            *word |= 1 << (id & 63);
        }
    }

    pub fn contains(&self, id: u32) -> bool {
        self.words
            .get(id as usize >> 6)
            .is_some_and(|word| word >> (id & 63) & 1 == 1)
    }

    fn from_predicate(mut f: impl FnMut(u32) -> bool) -> StateSet {
        let mut set = StateSet::new();
        for id in 0..generic::state_count() {
            if f(id) {
                set.insert(id);
            }
        }
        set
    }

    fn union_with(&mut self, other: &StateSet) {
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a |= b;
        }
    }

    fn intersect_with(&mut self, other: &StateSet) {
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a &= b;
        }
    }

    fn inverted(&self) -> StateSet {
        StateSet::from_predicate(|id| !self.contains(id))
    }

    pub fn len(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|w| *w == 0)
    }
}

/// Splits at `sep` but not inside of `[...]`
fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Splits `name[a][b][c]` into the name and the contents of the bracket groups. The groups may
/// contain nested brackets. Without any `[` the whole text is the name.
fn bracket_args(s: &str) -> Option<(&str, Vec<&str>)> {
    let Some(first) = s.find('[') else {
        return Some((s, Vec::new()));
    };
    let name = &s[..first];
    let mut args = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices().skip_while(|(i, _)| *i < first) {
        match c {
            '[' => {
                if depth == 0 {
                    start = i + 1;
                }
                depth += 1;
            }
            ']' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
                if depth == 0 {
                    args.push(&s[start..i]);
                    // Only another group or the end may follow
                    let rest = &s[i + 1..];
                    if !rest.is_empty() && !rest.starts_with('[') {
                        return None;
                    }
                }
            }
            _ => {}
        }
    }
    (depth == 0).then_some((name, args))
}

const DYES: [(&str, [u8; 3], [u8; 3]); 16] = [
    ("white", [207, 213, 214], [209, 177, 161]),
    ("orange", [224, 97, 1], [161, 83, 37]),
    ("magenta", [169, 48, 159], [149, 88, 108]),
    ("light_blue", [36, 137, 199], [113, 108, 137]),
    ("yellow", [240, 175, 21], [186, 133, 35]),
    ("lime", [94, 168, 24], [103, 117, 52]),
    ("pink", [213, 101, 142], [161, 78, 78]),
    ("gray", [55, 58, 62], [57, 42, 35]),
    ("light_gray", [125, 125, 115], [135, 106, 97]),
    ("cyan", [21, 119, 136], [87, 91, 91]),
    ("purple", [100, 32, 156], [118, 70, 86]),
    ("blue", [45, 47, 143], [74, 59, 91]),
    ("brown", [96, 60, 32], [77, 51, 36]),
    ("green", [73, 91, 36], [76, 83, 42]),
    ("red", [142, 32, 32], [143, 61, 46]),
    ("black", [8, 10, 15], [37, 22, 16]),
];

const PLAIN_COLORS: &[(&str, [u8; 3])] = &[
    ("stone", [125, 125, 125]),
    ("cobblestone", [127, 127, 127]),
    ("smooth_stone", [158, 158, 158]),
    ("stone_bricks", [122, 121, 122]),
    ("dirt", [134, 96, 67]),
    ("coarse_dirt", [119, 85, 59]),
    ("podzol", [91, 63, 24]),
    ("grass_block", [95, 159, 53]),
    ("sand", [219, 207, 163]),
    ("red_sand", [190, 102, 33]),
    ("gravel", [131, 127, 126]),
    ("oak_planks", [162, 130, 78]),
    ("spruce_planks", [114, 84, 48]),
    ("birch_planks", [192, 175, 121]),
    ("jungle_planks", [160, 115, 81]),
    ("acacia_planks", [168, 90, 50]),
    ("dark_oak_planks", [66, 43, 20]),
    ("bricks", [150, 97, 83]),
    ("snow_block", [249, 254, 254]),
    ("clay", [160, 166, 179]),
    ("terracotta", [152, 94, 67]),
    ("netherrack", [97, 38, 38]),
    ("end_stone", [219, 222, 158]),
    ("obsidian", [15, 11, 25]),
    ("coal_block", [16, 16, 16]),
    ("iron_block", [220, 220, 220]),
    ("gold_block", [249, 236, 79]),
    ("diamond_block", [98, 219, 214]),
    ("emerald_block", [42, 203, 87]),
    ("lapis_block", [31, 67, 140]),
    ("redstone_block", [170, 24, 9]),
    ("quartz_block", [236, 230, 223]),
    ("sandstone", [216, 203, 155]),
    ("red_sandstone", [186, 99, 29]),
    ("glowstone", [171, 131, 84]),
    ("prismarine", [99, 171, 158]),
    ("purpur_block", [169, 125, 169]),
    ("bone_block", [229, 226, 208]),
    ("deepslate", [80, 80, 82]),
    ("blackstone", [42, 36, 41]),
    ("andesite", [136, 136, 137]),
    ("diorite", [188, 188, 188]),
    ("granite", [149, 103, 86]),
    ("calcite", [223, 224, 220]),
    ("tuff", [108, 109, 102]),
    ("mud_bricks", [137, 103, 79]),
    ("moss_block", [89, 109, 45]),
    ("oak_log", [109, 85, 50]),
    ("spruce_log", [58, 37, 16]),
    ("birch_log", [216, 215, 210]),
    ("ice", [145, 183, 253]),
    ("packed_ice", [141, 180, 250]),
    ("magma_block", [142, 63, 31]),
    ("sea_lantern", [172, 199, 190]),
];

/// The blocks `#color` and friends choose from with their color
fn color_table() -> &'static [(u32, [u8; 3])] {
    static TABLE: LazyLock<Vec<(u32, [u8; 3])>> = LazyLock::new(|| {
        let mut table = Vec::new();
        let mut add = |name: String, rgb: [u8; 3]| {
            if let Some(id) = generic::default_state(&format!("minecraft:{name}")) {
                table.push((id, rgb));
            }
        };
        for (dye, concrete, terracotta) in DYES {
            add(format!("{dye}_concrete"), concrete);
            add(
                format!("{dye}_wool"),
                concrete.map(|c| c.saturating_sub(c / 12)),
            );
            add(format!("{dye}_terracotta"), terracotta);
        }
        for (name, rgb) in PLAIN_COLORS {
            add((*name).to_owned(), *rgb);
        }
        table
    });
    &TABLE
}

fn color_of(id: u32) -> Option<[u8; 3]> {
    // Only the default state is in the table, so go by the name
    let name = state_names().get(id as usize)?;
    let default = generic::default_state(name)?;
    color_table()
        .iter()
        .find(|(block, _)| *block == default)
        .map(|(_, rgb)| *rgb)
}

fn luma(c: [u8; 3]) -> f32 {
    0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32
}

fn color_distance(a: [u8; 3], b: [u8; 3]) -> f32 {
    let d = |i: usize| (a[i] as f32 - b[i] as f32).powi(2);
    d(0) * 0.30 + d(1) * 0.59 + d(2) * 0.11
}

/// The block of the table that is closest to the color, optionally only from the ones that pass
/// `filter`
fn closest_block(color: [u8; 3], filter: impl Fn(u32, [u8; 3]) -> bool) -> Option<u32> {
    color_table()
        .iter()
        .filter(|(id, rgb)| filter(*id, *rgb))
        .min_by(|a, b| {
            color_distance(a.1, color)
                .partial_cmp(&color_distance(b.1, color))
                .unwrap()
        })
        .map(|(id, _)| *id)
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    [0, 1, 2].map(|i| {
        (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    })
}

/// `#rrggbb`, `r,g,b` or the name of a dye
fn parse_color(text: &str, original: &str) -> PatternParseResult<[u8; 3]> {
    let invalid = || PatternParseError::InvalidPattern(original.to_owned());
    let text = text.trim();
    if let Some(hex) = text.strip_prefix('#') {
        if hex.len() == 6 {
            let v = u32::from_str_radix(hex, 16).map_err(|_| invalid())?;
            return Ok([(v >> 16) as u8, (v >> 8) as u8, v as u8]);
        }
        return Err(invalid());
    }
    if let Some((_, concrete, _)) = DYES.iter().find(|(name, _, _)| *name == text) {
        return Ok(*concrete);
    }
    let parts: Vec<&str> = text.split(',').collect();
    if parts.len() == 3 {
        let mut rgb = [0u8; 3];
        for (i, part) in parts.iter().enumerate() {
            rgb[i] = part.trim().parse::<u8>().map_err(|_| invalid())?;
        }
        return Ok(rgb);
    }
    Err(invalid())
}

fn tag_states(tag: &str) -> PatternParseResult<StateSet> {
    let predicate = tag_predicate(tag).ok_or_else(|| {
        PatternParseError::UnknownTag(format!(
            "{tag} (known tags: {TAG_NAMES}; biomes are not supported)"
        ))
    })?;
    Ok(StateSet::from_predicate(|id| {
        predicate(state_names()[id as usize])
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoiseKind {
    Simplex,
    Perlin,
    /// Ridged multi-fractal
    Rmf,
    Voronoi,
}

fn hash3(x: i32, y: i32, z: i32, seed: u64) -> u64 {
    let mut h = seed
        ^ (x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (z as i64 as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^ (h >> 31)
}

fn unit(h: u64) -> f32 {
    (h >> 40) as f32 / (1u64 << 24) as f32
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn value_noise(x: f32, y: f32, z: f32, seed: u64) -> f32 {
    let (fx, fy, fz) = (x.floor(), y.floor(), z.floor());
    let (ix, iy, iz) = (fx as i32, fy as i32, fz as i32);
    let (tx, ty, tz) = (smooth(x - fx), smooth(y - fy), smooth(z - fz));
    let corner = |dx, dy, dz| unit(hash3(ix + dx, iy + dy, iz + dz, seed));
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), tx);
    let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), tx);
    let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), tx);
    let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), tx);
    lerp(lerp(x00, x10, ty), lerp(x01, x11, ty), tz)
}

/// Noise in `0.0..1.0`, roughly evenly distributed
pub fn noise_value(kind: NoiseKind, pos: BlockPos, scale: f32, seed: u64) -> f32 {
    let (x, y, z) = (
        pos.x as f32 / scale,
        pos.y as f32 / scale,
        pos.z as f32 / scale,
    );
    // Value noise clusters around the middle, stretch it
    let stretch = |n: f32| ((n - 0.5) * 1.7 + 0.5).clamp(0.0, 0.999_999);
    match kind {
        NoiseKind::Simplex => stretch(value_noise(x, y, z, seed)),
        NoiseKind::Perlin => stretch(value_noise(x, y, z, seed ^ 0x5851_F42D_4C95_7F2D)),
        NoiseKind::Rmf => {
            let mut sum = 0.0;
            let mut amp = 0.5;
            let mut freq = 1.0;
            for octave in 0..3u64 {
                let n = value_noise(x * freq, y * freq, z * freq, seed ^ octave.wrapping_mul(77));
                sum += (1.0 - (2.0 * n - 1.0).abs()) * amp;
                amp *= 0.5;
                freq *= 2.0;
            }
            stretch(sum / 0.875)
        }
        NoiseKind::Voronoi => {
            let (cx, cy, cz) = (x.floor() as i32, y.floor() as i32, z.floor() as i32);
            let mut best = (f32::MAX, 0u64);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let (gx, gy, gz) = (cx + dx, cy + dy, cz + dz);
                        let h = hash3(gx, gy, gz, seed);
                        let px = gx as f32 + unit(h);
                        let py = gy as f32 + unit(h.rotate_left(21));
                        let pz = gz as f32 + unit(h.rotate_left(42));
                        let d = (px - x).powi(2) + (py - y).powi(2) + (pz - z).powi(2);
                        if d < best.0 {
                            best = (d, h);
                        }
                    }
                }
            }
            unit(best.1.rotate_left(7).wrapping_mul(0x2545_F491_4F6C_DD1D))
        }
    }
}

fn parse_int(s: &str, original: &str) -> PatternParseResult<i32> {
    s.trim()
        .parse()
        .map_err(|_| PatternParseError::InvalidPattern(original.to_owned()))
}

fn parse_float(s: &str, original: &str) -> PatternParseResult<f32> {
    s.trim()
        .parse()
        .map_err(|_| PatternParseError::InvalidPattern(original.to_owned()))
}

struct BlockSpec<'a> {
    name: String,
    properties: Vec<(&'a str, &'a str)>,
}

fn parse_block_spec(spec: &str) -> PatternParseResult<BlockSpec<'_>> {
    let invalid = || PatternParseError::InvalidPattern(spec.to_owned());
    let (name, properties) = match spec.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']').ok_or_else(invalid)?),
        None => (spec, ""),
    };
    let valid_name = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | ':' | '*'));
    if !valid_name {
        return Err(invalid());
    }
    let mut name = name.to_owned();
    if !name.contains(':') {
        name.insert_str(0, "minecraft:");
    }
    let mut parsed = Vec::new();
    for prop in properties.split(',').filter(|p| !p.is_empty()) {
        let (key, value) = prop.split_once('=').ok_or_else(invalid)?;
        parsed.push((key.trim(), value.trim()));
    }
    Ok(BlockSpec {
        name,
        properties: parsed,
    })
}

/// Resolves a block with properties to a block state, the properties that are not given keep
/// their default values
fn block_state_of(spec: &BlockSpec<'_>, original: &str) -> PatternParseResult<u32> {
    let mut block = Block::from_name(&spec.name)
        .ok_or_else(|| PatternParseError::UnknownBlock(original.to_owned()))?;
    if !spec.properties.is_empty() {
        block.set_properties(spec.properties.iter().copied().collect());
        let actual = block.properties();
        for (key, value) in &spec.properties {
            if actual.get(key).map(String::as_str) != Some(*value) {
                return Err(PatternParseError::InvalidProperty(format!(
                    "{} for {}",
                    key, spec.name
                )));
            }
        }
    }
    Ok(block.get_id())
}

/// The block states of the block with this name (which may contain `*` wildcards) that have all
/// of the properties
fn states_matching(spec: &BlockSpec<'_>, original: &str) -> PatternParseResult<StateSet> {
    let wildcard = spec.name.contains('*');
    let set = StateSet::from_predicate(|id| {
        let name = state_names()[id as usize];
        let name_matches = if wildcard {
            glob_match(&spec.name, name)
        } else {
            name == spec.name
        };
        name_matches
            && (spec.properties.is_empty() || {
                let props = Block::from_id(id).properties();
                spec.properties
                    .iter()
                    .all(|(key, value)| props.get(key).map(String::as_str) == Some(*value))
            })
    });
    if set.is_empty() {
        // Tell apart blocks that don't exist and properties that never match
        if !wildcard && Block::from_name(&spec.name).is_none() {
            return Err(PatternParseError::UnknownBlock(original.to_owned()));
        }
        return Err(if spec.properties.is_empty() {
            PatternParseError::UnknownBlock(original.to_owned())
        } else {
            PatternParseError::InvalidProperty(original.to_owned())
        });
    }
    Ok(set)
}

/// Matches `text` against a pattern where `*` stands for any number of characters
fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || text.len() < first.len() + last.len() || !text.ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(idx) => rest = &rest[idx + part.len()..],
            None => return false,
        }
    }
    true
}

fn tag_predicate(tag: &str) -> Option<fn(&str) -> bool> {
    // The names all start with `minecraft:`
    fn short(name: &str) -> &str {
        name.strip_prefix("minecraft:").unwrap_or(name)
    }
    Some(match tag {
        "wool" => |n| short(n).ends_with("_wool"),
        "carpets" => |n| short(n).ends_with("_carpet"),
        "stairs" => |n| short(n).ends_with("_stairs"),
        "slabs" => |n| short(n).ends_with("_slab"),
        "fences" => |n| short(n).ends_with("_fence"),
        "fence_gates" => |n| short(n).ends_with("_fence_gate"),
        "walls" => |n| short(n).ends_with("_wall"),
        "doors" => |n| short(n).ends_with("_door"),
        "trapdoors" => |n| short(n).ends_with("_trapdoor"),
        "buttons" => |n| short(n).ends_with("_button"),
        "pressure_plates" => |n| short(n).ends_with("_pressure_plate"),
        "planks" => |n| short(n).ends_with("_planks"),
        "logs" => |n| {
            let n = short(n);
            n.ends_with("_log")
                || n.ends_with("_wood")
                || n.ends_with("_stem")
                || n.ends_with("_hyphae")
        },
        "leaves" => |n| short(n).ends_with("_leaves"),
        "saplings" => |n| short(n).ends_with("_sapling"),
        "glass" => |n| {
            let n = short(n);
            n == "glass" || n == "tinted_glass" || n.ends_with("_stained_glass")
        },
        "glass_panes" => |n| short(n).ends_with("glass_pane"),
        "concrete" => |n| short(n).ends_with("_concrete"),
        "concrete_powder" => |n| short(n).ends_with("_concrete_powder"),
        "terracotta" => |n| short(n) == "terracotta" || short(n).ends_with("_terracotta"),
        "beds" => |n| short(n).ends_with("_bed"),
        "banners" => |n| short(n).ends_with("_banner"),
        "signs" => |n| short(n).ends_with("_sign"),
        "shulker_boxes" => |n| short(n).ends_with("shulker_box"),
        "ores" => |n| short(n).ends_with("_ore"),
        _ => return None,
    })
}

const TAG_NAMES: &str = "wool, carpets, stairs, slabs, fences, fence_gates, walls, doors, \
    trapdoors, buttons, pressure_plates, planks, logs, leaves, saplings, glass, glass_panes, \
    concrete, concrete_powder, terracotta, beds, banners, signs, shulker_boxes, ores";

const NEIGHBOURS: [[i32; 3]; 6] = [
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
];
const HORIZONTAL: [[i32; 3]; 4] = [[1, 0, 0], [-1, 0, 0], [0, 0, 1], [0, 0, -1]];

/// Decides which positions an operation affects
#[derive(Clone, Debug, PartialEq)]
pub enum Mask {
    /// The block at the position is one of these states
    States(StateSet),
    Not(Box<Mask>),
    And(Vec<Mask>),
    Or(Vec<Mask>),
    /// The block above the position matches
    Above(Box<Mask>),
    /// The block below the position matches
    Below(Box<Mask>),
    /// Matches with this probability
    Random(f32),
    /// `#offset[dx][dy][dz][mask]`: the mask is tested at the offset position
    Offset([i32; 3], Box<Mask>),
    /// `#surface`: a solid block next to air
    Surface,
    /// `#wall`: air to the north, east, south or west
    Wall,
    /// `~[mask][min][max]`: between `min` and `max` of the six neighbours match
    Adjacent {
        mask: Box<Mask>,
        min: u32,
        max: u32,
    },
    /// `#id`, `#data`, `#iddata`: same block, same properties or same state as the block at the
    /// origin (kind 0, 1, 2)
    Initial {
        kind: u8,
        origin: BlockPos,
        cache: Arc<OnceLock<u32>>,
    },
    /// `#xaxis`, `#yaxis`, `#zaxis`: on the line through the origin
    Axis {
        axis: usize,
        origin: BlockPos,
    },
    /// `{min max`: the distance to the origin is in the range
    Radius {
        origin: BlockPos,
        min: f32,
        max: f32,
    },
    /// `#region`, `#dregion`
    Region(BlockPos, BlockPos),
    /// `/[min][max]`: the angle of the terrain (in degrees) or the height step to the neighbours
    Angle {
        min: f32,
        max: f32,
        degrees: bool,
    },
    /// `#light`, `#skylight`, `#blocklight`: kind 0, 1, 2
    Light {
        kind: u8,
        min: u8,
        max: u8,
    },
    /// `#opacity[min][max]` (kind 0) and `#brightness[min][max]` (kind 1) of the block itself
    BlockLight {
        kind: u8,
        min: u8,
        max: u8,
    },
    /// `#haslight` (true) and `#nolight` (false)
    HasLight(bool),
    /// `#simplex[scale][min][max]`: noise in percent between min and max
    Noise {
        scale: f32,
        min: f32,
        max: f32,
        seed: u64,
    },
}

impl Mask {
    pub fn parse(s: &str) -> PatternParseResult<Mask> {
        let s = s.trim();
        if s.is_empty() {
            return Err(PatternParseError::InvalidPattern(s.to_owned()));
        }
        let alternatives = split_top_level(s, ',')
            .into_iter()
            .map(Mask::parse_and)
            .collect::<PatternParseResult<Vec<_>>>()?;
        Ok(Mask::or(alternatives))
    }

    fn parse_and(s: &str) -> PatternParseResult<Mask> {
        let parts = split_top_level(s, '&')
            .into_iter()
            .map(Mask::parse_unary)
            .collect::<PatternParseResult<Vec<_>>>()?;
        Ok(Mask::and(parts))
    }

    fn parse_unary(s: &str) -> PatternParseResult<Mask> {
        let s = s.trim();
        let invalid = || PatternParseError::InvalidPattern(s.to_owned());
        if let Some(rest) = s.strip_prefix('!') {
            return Ok(Mask::not(Mask::parse_unary(rest)?));
        }
        if let Some(rest) = s.strip_prefix('>') {
            return Ok(Mask::Above(Box::new(Mask::parse_unary(rest)?)));
        }
        if let Some(rest) = s.strip_prefix('<') {
            return Ok(Mask::Below(Box::new(Mask::parse_unary(rest)?)));
        }
        if let Some(rest) = s.strip_prefix('%') {
            let percent: f32 = rest.parse().map_err(|_| invalid())?;
            return Ok(Mask::Random((percent / 100.0).clamp(0.0, 1.0)));
        }
        if let Some(tag) = s.strip_prefix("##").or_else(|| s.strip_prefix('$')) {
            return Ok(Mask::States(tag_states(tag)?));
        }
        if let Some(mask) = Mask::parse_special(s)? {
            return Ok(mask);
        }
        match s {
            "#existing" => return Ok(Mask::States(StateSet::single(0).inverted())),
            "#air" => return Ok(Mask::States(StateSet::single(0))),
            "#solid" => {
                return Ok(Mask::States(StateSet::from_predicate(|id| {
                    Block::from_id(id).is_solid()
                })))
            }
            _ => {}
        }
        if let Some(id) = s.strip_prefix('=') {
            let id: u32 = id.parse().map_err(|_| invalid())?;
            if id >= generic::state_count() {
                return Err(invalid());
            }
            return Ok(Mask::States(StateSet::single(id)));
        }
        let spec = parse_block_spec(s)?;
        Ok(Mask::States(states_matching(&spec, s)?))
    }

    /// `{min max` (or `{[min][max]`)
    fn parse_radius(text: &str, original: &str) -> PatternParseResult<Mask> {
        let invalid = || PatternParseError::InvalidPattern(original.to_owned());
        let origin = parse_context().origin.ok_or_else(invalid)?;
        let (min, max) = if text.starts_with('[') {
            let (_, args) = bracket_args(text).ok_or_else(invalid)?;
            if args.len() != 2 {
                return Err(invalid());
            }
            (
                parse_float(args[0], original)?,
                parse_float(args[1], original)?,
            )
        } else {
            let parts: Vec<&str> = text.split([' ', ':']).filter(|p| !p.is_empty()).collect();
            match parts.as_slice() {
                [max] => (0.0, parse_float(max, original)?),
                [min, max] => (parse_float(min, original)?, parse_float(max, original)?),
                _ => return Err(invalid()),
            }
        };
        Ok(Mask::Radius { origin, min, max })
    }

    /// The masks with arguments in brackets and the constants
    fn parse_special(s: &str) -> PatternParseResult<Option<Mask>> {
        if let Some(rest) = s.strip_prefix('{') {
            return Mask::parse_radius(rest.trim_end_matches('}'), s).map(Some);
        }
        if !(s.starts_with('#')
            || s.starts_with('~')
            || s.starts_with('/')
            || s.starts_with('\\')
            || s == "true"
            || s == "false")
        {
            return Ok(None);
        }
        let invalid = || PatternParseError::InvalidPattern(s.to_owned());
        let Some((name, args)) = bracket_args(s) else {
            return Err(invalid());
        };
        let none_if_no_args = |mask: Mask| {
            if args.is_empty() {
                Ok(Some(mask))
            } else {
                Err(invalid())
            }
        };
        match name {
            "true" => none_if_no_args(Mask::States(StateSet::from_predicate(|_| true))),
            "false" => none_if_no_args(Mask::States(StateSet::new())),
            "#liquid" => none_if_no_args(Mask::States(StateSet::from_predicate(|id| {
                matches!(
                    state_names()[id as usize],
                    "minecraft:water" | "minecraft:lava"
                )
            }))),
            "#surface" => none_if_no_args(Mask::Surface),
            "#id" | "#data" | "#iddata" => {
                let origin = parse_context().origin.ok_or_else(invalid)?;
                let kind = match name {
                    "#id" => 0,
                    "#data" => 1,
                    _ => 2,
                };
                none_if_no_args(Mask::Initial {
                    kind,
                    origin,
                    cache: Arc::new(OnceLock::new()),
                })
            }
            "#xaxis" | "#yaxis" | "#zaxis" => {
                let origin = parse_context().origin.ok_or_else(invalid)?;
                let axis = (name.as_bytes()[1] - b'x') as usize;
                none_if_no_args(Mask::Axis { axis, origin })
            }
            "#region" | "#dregion" => {
                let (a, b) = parse_context().region.ok_or_else(|| {
                    PatternParseError::InvalidPattern(format!(
                        "{s} (make a region selection first)"
                    ))
                })?;
                none_if_no_args(Mask::Region(a.min(b), a.max(b)))
            }
            "/" | "\\" => {
                if args.len() != 2 {
                    return Err(invalid());
                }
                let parse = |text: &str| -> PatternParseResult<(f32, bool)> {
                    match text.trim().strip_suffix('d') {
                        Some(v) => Ok((parse_float(v, s)?, true)),
                        None => Ok((parse_float(text, s)?, false)),
                    }
                };
                let (min, degrees) = parse(args[0])?;
                let (max, degrees2) = parse(args[1])?;
                if degrees != degrees2 {
                    return Err(invalid());
                }
                Ok(Some(Mask::Angle { min, max, degrees }))
            }
            "#light" | "#skylight" | "#blocklight" | "#opacity" | "#brightness" => {
                if args.len() > 2 {
                    return Err(invalid());
                }
                let get = |i: usize, default: i32| match args.get(i) {
                    Some(v) => parse_int(v, s).map(|v| v.clamp(0, 15) as u8),
                    None => Ok(default as u8),
                };
                let (min, max) = (get(0, 0)?, get(1, 15)?);
                Ok(Some(match name {
                    "#light" => Mask::Light { kind: 0, min, max },
                    "#skylight" => Mask::Light { kind: 1, min, max },
                    "#blocklight" => Mask::Light { kind: 2, min, max },
                    "#opacity" => Mask::BlockLight { kind: 0, min, max },
                    _ => Mask::BlockLight { kind: 1, min, max },
                }))
            }
            "#haslight" => none_if_no_args(Mask::HasLight(true)),
            "#nolight" => none_if_no_args(Mask::HasLight(false)),
            "#wall" => none_if_no_args(Mask::Wall),
            "#offset" => {
                if args.len() != 4 {
                    return Err(invalid());
                }
                let d = [
                    parse_int(args[0], s)?,
                    parse_int(args[1], s)?,
                    parse_int(args[2], s)?,
                ];
                Ok(Some(Mask::Offset(d, Box::new(Mask::parse(args[3])?))))
            }
            "~" => {
                if args.is_empty() || args.len() > 3 {
                    return Err(invalid());
                }
                let min = match args.get(1) {
                    Some(v) => parse_int(v, s)?.max(0) as u32,
                    None => 1,
                };
                let max = match args.get(2) {
                    Some(v) => parse_int(v, s)?.max(0) as u32,
                    None => 8,
                };
                Ok(Some(Mask::Adjacent {
                    mask: Box::new(Mask::parse(args[0])?),
                    min,
                    max,
                }))
            }
            "#simplex" => {
                if args.len() > 3 {
                    return Err(invalid());
                }
                let get = |i: usize, default: f32| match args.get(i) {
                    Some(v) => parse_float(v, s),
                    None => Ok(default),
                };
                let scale = get(0, 10.0)?;
                if scale <= 0.0 {
                    return Err(invalid());
                }
                Ok(Some(Mask::Noise {
                    scale,
                    min: get(1, 0.0)?,
                    max: get(2, 100.0)?,
                    seed: FastRng::new().next_u64(),
                }))
            }
            _ => Ok(None),
        }
    }

    pub fn not(mask: Mask) -> Mask {
        match mask {
            Mask::States(set) => Mask::States(set.inverted()),
            Mask::Not(inner) => *inner,
            other => Mask::Not(Box::new(other)),
        }
    }

    /// Any of the masks, merging masks that only look at the block state
    pub fn or(masks: Vec<Mask>) -> Mask {
        Mask::combine(masks, true)
    }

    /// All of the masks, merging masks that only look at the block state
    pub fn and(masks: Vec<Mask>) -> Mask {
        Mask::combine(masks, false)
    }

    fn combine(masks: Vec<Mask>, any: bool) -> Mask {
        let mut merged: Option<StateSet> = None;
        let mut others = Vec::new();
        for mask in masks {
            match mask {
                Mask::States(set) => match &mut merged {
                    Some(merged) if any => merged.union_with(&set),
                    Some(merged) => merged.intersect_with(&set),
                    None => merged = Some(set),
                },
                other => others.push(other),
            }
        }
        if let Some(set) = merged {
            others.insert(0, Mask::States(set));
        }
        if others.len() == 1 {
            others.pop().unwrap()
        } else if any {
            Mask::Or(others)
        } else {
            Mask::And(others)
        }
    }

    /// Whether the mask can be decided by looking at the block state alone. Only then
    /// [`Mask::test_state`] may be used.
    pub fn is_state_only(&self) -> bool {
        match self {
            Mask::States(_) => true,
            Mask::Not(mask) => mask.is_state_only(),
            Mask::And(masks) | Mask::Or(masks) => masks.iter().all(Mask::is_state_only),
            _ => false,
        }
    }

    /// Tests a block state. Masks that are not [state only](Mask::is_state_only) never match.
    pub fn test_state(&self, id: u32) -> bool {
        match self {
            Mask::States(set) => set.contains(id),
            Mask::Not(mask) => !mask.test_state(id),
            Mask::And(masks) => masks.iter().all(|mask| mask.test_state(id)),
            Mask::Or(masks) => masks.iter().any(|mask| mask.test_state(id)),
            _ => false,
        }
    }

    /// Tests the block `id` at `pos` in `view`
    pub fn test<V: BlockView + ?Sized>(
        &self,
        view: &V,
        pos: BlockPos,
        id: u32,
        rng: &mut FastRng,
    ) -> bool {
        match self {
            Mask::States(set) => set.contains(id),
            Mask::Not(mask) => !mask.test(view, pos, id, rng),
            Mask::And(masks) => masks.iter().all(|mask| mask.test(view, pos, id, rng)),
            Mask::Or(masks) => masks.iter().any(|mask| mask.test(view, pos, id, rng)),
            Mask::Above(mask) => {
                let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
                mask.test(view, above, view.block_id(above), rng)
            }
            Mask::Below(mask) => {
                let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
                mask.test(view, below, view.block_id(below), rng)
            }
            Mask::Random(chance) => rng.next_f32() < *chance,
            Mask::Offset(d, mask) => {
                let at = BlockPos::new(pos.x + d[0], pos.y + d[1], pos.z + d[2]);
                mask.test(view, at, view.block_id(at), rng)
            }
            Mask::Surface => {
                Block::from_id(id).is_solid()
                    && NEIGHBOURS.iter().any(|d| {
                        view.block_id(BlockPos::new(pos.x + d[0], pos.y + d[1], pos.z + d[2])) == 0
                    })
            }
            Mask::Wall => HORIZONTAL.iter().any(|d| {
                view.block_id(BlockPos::new(pos.x + d[0], pos.y + d[1], pos.z + d[2])) == 0
            }),
            Mask::Adjacent { mask, min, max } => {
                let mut count = 0u32;
                for d in NEIGHBOURS {
                    let at = BlockPos::new(pos.x + d[0], pos.y + d[1], pos.z + d[2]);
                    if mask.test(view, at, view.block_id(at), rng) {
                        count += 1;
                    }
                }
                (*min..=*max).contains(&count)
            }
            Mask::Initial {
                kind,
                origin,
                cache,
            } => {
                let initial = *cache.get_or_init(|| view.block_id(*origin));
                match kind {
                    0 => state_names()[id as usize] == state_names()[initial as usize],
                    1 => Block::from_id(id).properties() == Block::from_id(initial).properties(),
                    _ => id == initial,
                }
            }
            Mask::Axis { axis, origin } => {
                let (p, o) = ([pos.x, pos.y, pos.z], [origin.x, origin.y, origin.z]);
                (0..3).all(|i| i == *axis || p[i] == o[i])
            }
            Mask::Radius { origin, min, max } => {
                let d = pos - *origin;
                let dist = ((d.x * d.x + d.y * d.y + d.z * d.z) as f32).sqrt();
                dist >= *min && dist <= *max
            }
            Mask::Region(lo, hi) => {
                pos.x >= lo.x
                    && pos.x <= hi.x
                    && pos.y >= lo.y
                    && pos.y <= hi.y
                    && pos.z >= lo.z
                    && pos.z <= hi.z
            }
            Mask::Angle { min, max, degrees } => {
                if id == 0 {
                    return false;
                }
                let value = if *degrees {
                    terrain_angle(view, pos, 1)
                } else {
                    max_height_step(view, pos, 1)
                };
                value >= *min && value <= *max
            }
            Mask::Light { kind, min, max } => {
                let (sky, block) = (view.sky_light(pos), view.block_light(pos));
                let level = match kind {
                    0 => sky.max(block),
                    1 => sky,
                    _ => block,
                };
                level >= *min && level <= *max
            }
            Mask::BlockLight { kind, min, max } => {
                let level = if *kind == 0 {
                    generic::light_opacity(id)
                } else {
                    generic::light_emission(id)
                };
                level >= *min && level <= *max
            }
            Mask::HasLight(has) => (view.sky_light(pos).max(view.block_light(pos)) > 0) == *has,
            Mask::Noise {
                scale,
                min,
                max,
                seed,
            } => {
                let n = noise_value(NoiseKind::Simplex, pos, *scale, *seed) * 100.0;
                n >= *min && n <= *max
            }
        }
    }
}

impl FromStr for Mask {
    type Err = PatternParseError;

    fn from_str(s: &str) -> PatternParseResult<Mask> {
        Mask::parse(s)
    }
}

/// Decides which block is placed at a position
#[derive(Clone, Debug, PartialEq)]
pub enum Pattern {
    Single(u32),
    /// A weighted random choice
    Random {
        parts: Vec<(f32, Pattern)>,
        total_weight: f32,
    },
    /// The block with this state id, but with the properties of the old block where possible
    KeepStates(u32),
    /// The blocks of the clipboard, repeated over the area
    Clipboard,
    /// `#existing`: keeps the block that is there
    Existing,
    /// `#simplex[scale][pattern]` and friends
    Noise {
        kind: NoiseKind,
        scale: f32,
        inner: Box<Pattern>,
    },
    /// `#linear`, `#linear2d`, `#linear3d`. `dims` is 0 (sequential), 2 or 3.
    Linear {
        parts: Vec<Pattern>,
        dims: u8,
        scale: [i32; 3],
    },
    /// `#offset[dx][dy][dz][pattern]`: the pattern as seen from another position
    Offset([i32; 3], Box<Pattern>),
    /// `#relative[pattern]`: the position relative to where the operation started
    Relative(Box<Pattern>),
    /// `#mask[mask][pattern-true][pattern-false]`, the mask only looks at the old block
    Mask {
        mask: Mask,
        yes: Box<Pattern>,
        no: Box<Pattern>,
    },
    /// `#!x[pattern]`, `#!y[pattern]`, `#!z[pattern]`: the inner pattern doesn't see that axis
    DropAxis(usize, Box<Pattern>),
    /// `#spread[dx][dy][dz][pattern]` (`solid` false) and `#solidspread`: the pattern at a random
    /// position nearby
    Spread {
        solid: bool,
        d: [i32; 3],
        inner: Box<Pattern>,
    },
    /// `#surfacespread[distance][pattern]`: the pattern at the surface near a random position
    SurfaceSpread(i32, Box<Pattern>),
    /// `#buffer[pattern]` (`two_d` false) and `#buffer2d`: every position (column) is only set once
    Buffer {
        two_d: bool,
        inner: Box<Pattern>,
    },
    /// The patterns that work with the color of the block that is there
    Color(ColorOp),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ColorOp {
    /// `#color[color]`: the block closest to the color
    Fixed([u8; 3]),
    /// `#saturate[color]`
    Saturate([u8; 3]),
    /// `#averagecolor[color]`
    Average([u8; 3]),
    /// `#desaturate[percent]`
    Desaturate(f32),
    Darken,
    Lighten,
    /// `#anglecolor[distance]`: the steeper the terrain, the darker
    AngleColor(i32),
    /// `#angledata`: sets the `layers` of snow and the like from the angle of the terrain
    AngleData,
}

impl Pattern {
    pub fn parse(s: &str) -> PatternParseResult<Pattern> {
        let s = s.trim();
        if s.is_empty() {
            return Err(PatternParseError::InvalidPattern(s.to_owned()));
        }
        let items = split_top_level(s, ',');
        if items.len() == 1 {
            // A weight on its own doesn't change anything
            return Ok(Pattern::parse_item(items[0])?.1);
        }
        let mut parts = Vec::new();
        let mut total_weight = 0.0;
        for item in items {
            let (weight, pattern) = Pattern::parse_item(item)?;
            total_weight += weight;
            parts.push((weight, pattern));
        }
        if total_weight <= 0.0 {
            return Err(PatternParseError::InvalidPattern(s.to_owned()));
        }
        Ok(Pattern::Random {
            parts,
            total_weight,
        })
    }

    fn parse_item(item: &str) -> PatternParseResult<(f32, Pattern)> {
        let item = item.trim();
        let invalid = || PatternParseError::InvalidPattern(item.to_owned());
        let (weight, rest) = match item.split_once('%') {
            Some((weight, rest)) if !weight.contains('[') && !weight.starts_with('#') => {
                (weight.parse::<f32>().map_err(|_| invalid())?, rest)
            }
            _ => (1.0, item),
        };
        if weight < 0.0 {
            return Err(invalid());
        }
        let pattern = if rest == "#clipboard" {
            Pattern::Clipboard
        } else if let Some(tag) = rest.strip_prefix('$') {
            Pattern::from_tag(tag)?
        } else if rest.starts_with('#') {
            Pattern::parse_special(rest)?
        } else if let Some(id) = rest.strip_prefix('=') {
            let id: u32 = id.parse().map_err(|_| invalid())?;
            if id >= generic::state_count() {
                return Err(invalid());
            }
            Pattern::Single(id)
        } else if let Some(block) = rest.strip_prefix('^') {
            let spec = parse_block_spec(block)?;
            Pattern::KeepStates(block_state_of(&spec, rest)?)
        } else {
            let spec = parse_block_spec(rest)?;
            Pattern::Single(block_state_of(&spec, rest)?)
        };
        Ok((weight, pattern))
    }

    /// A random block of the tag (every block has the same chance)
    fn from_tag(tag: &str) -> PatternParseResult<Pattern> {
        let states = tag_states(tag)?;
        let mut seen = FxHashSet::default();
        let mut parts = Vec::new();
        for id in 0..generic::state_count() {
            if states.contains(id) {
                let name = state_names()[id as usize];
                if seen.insert(name) {
                    let default = generic::default_state(name).unwrap_or(id);
                    parts.push((1.0, Pattern::Single(default)));
                }
            }
        }
        let total_weight = parts.len() as f32;
        Ok(Pattern::Random {
            parts,
            total_weight,
        })
    }

    /// The patterns with arguments in brackets
    fn parse_special(s: &str) -> PatternParseResult<Pattern> {
        if let Some(tag) = s.strip_prefix("##") {
            return Pattern::from_tag(tag);
        }
        let invalid = || PatternParseError::InvalidPattern(s.to_owned());
        let (name, args) = bracket_args(s).ok_or_else(invalid)?;
        let noise = |kind: NoiseKind| -> PatternParseResult<Pattern> {
            let (scale, inner) = match args.as_slice() {
                [inner] => (10.0, *inner),
                [scale, inner] => (parse_float(scale, s)?, *inner),
                _ => return Err(invalid()),
            };
            if scale <= 0.0 {
                return Err(invalid());
            }
            Ok(Pattern::Noise {
                kind,
                scale,
                inner: Box::new(Pattern::parse(inner)?),
            })
        };
        let parts_of = |text: &str| -> PatternParseResult<Vec<Pattern>> {
            Ok(match Pattern::parse(text)? {
                Pattern::Random { parts, .. } => parts.into_iter().map(|(_, p)| p).collect(),
                other => vec![other],
            })
        };
        let scale_arg = |i: usize| -> PatternParseResult<i32> {
            match args.get(i) {
                Some(v) => Ok(parse_int(v, s)?.max(1)),
                None => Ok(1),
            }
        };
        match name {
            "#existing" if args.is_empty() => Ok(Pattern::Existing),
            // The clipboard is tiled over the area, which is what a paste at every block ends up as
            "#copy" | "#fullcopy"
                if args.len() <= 1 && args.iter().all(|a| *a == "#copy" || *a == "#clipboard") =>
            {
                Ok(Pattern::Clipboard)
            }
            "#spread" | "#solidspread" if args.len() == 4 => Ok(Pattern::Spread {
                solid: name == "#solidspread",
                d: [
                    parse_int(args[0], s)?.max(0),
                    parse_int(args[1], s)?.max(0),
                    parse_int(args[2], s)?.max(0),
                ],
                inner: Box::new(Pattern::parse(args[3])?),
            }),
            "#surfacespread" if args.len() == 2 => Ok(Pattern::SurfaceSpread(
                parse_int(args[0], s)?.clamp(0, 64),
                Box::new(Pattern::parse(args[1])?),
            )),
            "#buffer" | "#buffer2d" if args.len() == 1 => Ok(Pattern::Buffer {
                two_d: name == "#buffer2d",
                inner: Box::new(Pattern::parse(args[0])?),
            }),
            "#color" if args.len() == 1 => {
                Ok(Pattern::Color(ColorOp::Fixed(parse_color(args[0], s)?)))
            }
            "#saturate" | "#averagecolor" => {
                let color = match args.as_slice() {
                    [one] => parse_color(one, s)?,
                    [r, g, b] => [
                        r.trim().parse::<u8>().map_err(|_| invalid())?,
                        g.trim().parse::<u8>().map_err(|_| invalid())?,
                        b.trim().parse::<u8>().map_err(|_| invalid())?,
                    ],
                    _ => return Err(invalid()),
                };
                Ok(Pattern::Color(if name == "#saturate" {
                    ColorOp::Saturate(color)
                } else {
                    ColorOp::Average(color)
                }))
            }
            "#desaturate" if args.len() <= 1 => {
                let percent = match args.first() {
                    Some(v) => parse_float(v.trim_end_matches('%'), s)?,
                    None => 25.0,
                };
                Ok(Pattern::Color(ColorOp::Desaturate(
                    percent.clamp(0.0, 100.0) / 100.0,
                )))
            }
            // The optional arguments of #lighten (randomize, max-complexity) don't change anything here
            "#darken" => Ok(Pattern::Color(ColorOp::Darken)),
            "#lighten" => Ok(Pattern::Color(ColorOp::Lighten)),
            "#anglecolor" if args.len() <= 1 => {
                Ok(Pattern::Color(ColorOp::AngleColor(match args.first() {
                    Some(v) => parse_int(v, s)?.clamp(1, 16),
                    None => 1,
                })))
            }
            "#angledata" if args.is_empty() => Ok(Pattern::Color(ColorOp::AngleData)),
            "#simplex" => noise(NoiseKind::Simplex),
            "#perlin" => noise(NoiseKind::Perlin),
            "#rmf" | "#ridged" => noise(NoiseKind::Rmf),
            "#voronoi" => noise(NoiseKind::Voronoi),
            "#linear" if args.len() == 1 => Ok(Pattern::Linear {
                parts: parts_of(args[0])?,
                dims: 0,
                scale: [1; 3],
            }),
            "#linear2d" if (1..=3).contains(&args.len()) => Ok(Pattern::Linear {
                parts: parts_of(args[0])?,
                dims: 2,
                scale: [scale_arg(1)?, 1, scale_arg(2)?],
            }),
            "#linear3d" if (1..=4).contains(&args.len()) => Ok(Pattern::Linear {
                parts: parts_of(args[0])?,
                dims: 3,
                scale: [scale_arg(1)?, scale_arg(2)?, scale_arg(3)?],
            }),
            "#offset" if args.len() == 4 => Ok(Pattern::Offset(
                [
                    parse_int(args[0], s)?,
                    parse_int(args[1], s)?,
                    parse_int(args[2], s)?,
                ],
                Box::new(Pattern::parse(args[3])?),
            )),
            "#relative" if args.len() == 1 => {
                Ok(Pattern::Relative(Box::new(Pattern::parse(args[0])?)))
            }
            "#mask" if args.len() == 3 => {
                let mask = Mask::parse(args[0])?;
                Ok(Pattern::Mask {
                    mask,
                    yes: Box::new(Pattern::parse(args[1])?),
                    no: Box::new(Pattern::parse(args[2])?),
                })
            }
            "#!x" | "#!y" | "#!z" if args.len() == 1 => Ok(Pattern::DropAxis(
                (name.as_bytes()[2] - b'x') as usize,
                Box::new(Pattern::parse(args[0])?),
            )),
            _ => Err(PatternParseError::InvalidPattern(format!(
                "{s} (unknown or unsupported pattern)"
            ))),
        }
    }

    /// Whether the pattern looks at other blocks than the one it replaces, then the edit has to
    /// give the [`PatternEnv`] a view of the world
    pub fn needs_world(&self) -> bool {
        self.reach() > 0
    }

    /// How far from the position the pattern may look at the world
    pub fn reach(&self) -> i32 {
        match self {
            Pattern::Spread { d, inner, .. } => d[0].max(d[1]).max(d[2]) + inner.reach(),
            Pattern::SurfaceSpread(distance, inner) => distance + ANGLE_RANGE + inner.reach(),
            Pattern::Color(ColorOp::AngleColor(distance)) => distance + ANGLE_RANGE,
            Pattern::Color(ColorOp::AngleData) => 1 + ANGLE_RANGE,
            Pattern::Color(_) => 0,
            Pattern::Mask { mask, yes, no } => {
                let own = if mask.is_state_only() {
                    0
                } else {
                    ANGLE_RANGE + 1
                };
                own.max(yes.reach()).max(no.reach())
            }
            Pattern::Noise { inner, .. }
            | Pattern::Offset(_, inner)
            | Pattern::Relative(inner)
            | Pattern::DropAxis(_, inner)
            | Pattern::Buffer { inner, .. } => inner.reach(),
            Pattern::Linear { parts, .. } => parts.iter().map(Pattern::reach).max().unwrap_or(0),
            Pattern::Random { parts, .. } => {
                parts.iter().map(|(_, p)| p.reach()).max().unwrap_or(0)
            }
            _ => 0,
        }
    }

    /// The block state if the pattern always places the same block
    pub fn as_single(&self) -> Option<u32> {
        match self {
            Pattern::Single(id) => Some(*id),
            Pattern::Random { parts, .. } if parts.len() == 1 => parts[0].1.as_single(),
            _ => None,
        }
    }

    pub fn needs_clipboard(&self) -> bool {
        match self {
            Pattern::Clipboard => true,
            Pattern::Random { parts, .. } => parts.iter().any(|(_, p)| p.needs_clipboard()),
            Pattern::Noise { inner, .. }
            | Pattern::Offset(_, inner)
            | Pattern::Relative(inner)
            | Pattern::DropAxis(_, inner)
            | Pattern::Spread { inner, .. }
            | Pattern::SurfaceSpread(_, inner)
            | Pattern::Buffer { inner, .. } => inner.needs_clipboard(),
            Pattern::Linear { parts, .. } => parts.iter().any(Pattern::needs_clipboard),
            Pattern::Mask { yes, no, .. } => yes.needs_clipboard() || no.needs_clipboard(),
            _ => false,
        }
    }

    /// The block to place at `pos`, which currently contains `old`
    pub fn pick(&self, pos: BlockPos, old: u32, env: &mut PatternEnv<'_>) -> u32 {
        match self {
            Pattern::Single(id) => *id,
            Pattern::Random {
                parts,
                total_weight,
            } => {
                let mut roll = env.rng.next_f32() * total_weight;
                for (weight, pattern) in parts {
                    roll -= weight;
                    if roll < 0.0 {
                        return pattern.pick(pos, old, env);
                    }
                }
                // Floating point error can leave a tiny rest
                parts.last().unwrap().1.pick(pos, old, env)
            }
            Pattern::Existing => old,
            Pattern::Noise { kind, scale, inner } => {
                let v = noise_value(*kind, pos, *scale, env.noise_seed);
                match &**inner {
                    Pattern::Random {
                        parts,
                        total_weight,
                    } => {
                        let mut roll = v * total_weight;
                        for (weight, pattern) in parts {
                            roll -= weight;
                            if roll < 0.0 {
                                return pattern.pick(pos, old, env);
                            }
                        }
                        parts.last().unwrap().1.pick(pos, old, env)
                    }
                    other => other.pick(pos, old, env),
                }
            }
            Pattern::Linear { parts, dims, scale } => {
                let n = parts.len() as i64;
                let idx = match dims {
                    0 => {
                        let i = env.linear_counter;
                        env.linear_counter += 1;
                        (i % n as u64) as i64
                    }
                    2 => (pos.x.div_euclid(scale[0]) as i64 + pos.z.div_euclid(scale[2]) as i64)
                        .rem_euclid(n),
                    _ => (pos.x.div_euclid(scale[0]) as i64
                        + pos.y.div_euclid(scale[1]) as i64
                        + pos.z.div_euclid(scale[2]) as i64)
                        .rem_euclid(n),
                };
                parts[idx as usize].pick(pos, old, env)
            }
            Pattern::Offset(d, inner) => inner.pick(
                BlockPos::new(pos.x + d[0], pos.y + d[1], pos.z + d[2]),
                old,
                env,
            ),
            Pattern::Relative(inner) => {
                let rel = pos - env.origin;
                inner.pick(BlockPos::new(rel.x, rel.y, rel.z), old, env)
            }
            Pattern::Mask { mask, yes, no } => {
                let matches = match env.view {
                    Some(view) if !mask.is_state_only() => mask.test(view, pos, old, &mut env.rng),
                    _ => mask.test_state(old),
                };
                if matches {
                    yes.pick(pos, old, env)
                } else {
                    no.pick(pos, old, env)
                }
            }
            Pattern::DropAxis(axis, inner) => {
                let mut p = [pos.x, pos.y, pos.z];
                p[*axis] = 0;
                inner.pick(BlockPos::new(p[0], p[1], p[2]), old, env)
            }
            Pattern::Spread { solid, d, inner } => {
                let tries = if *solid { 16 } else { 1 };
                let mut target = pos;
                for _ in 0..tries {
                    let offset = |env: &mut PatternEnv<'_>, d: i32| {
                        env.rng.below(2 * d as u32 + 1) as i32 - d
                    };
                    let candidate = BlockPos::new(
                        pos.x + offset(env, d[0]),
                        pos.y + offset(env, d[1]),
                        pos.z + offset(env, d[2]),
                    );
                    let ok = !*solid
                        || env
                            .view
                            .is_some_and(|v| Block::from_id(v.block_id(candidate)).is_solid());
                    if ok {
                        target = candidate;
                        break;
                    }
                }
                let at = env.view.map_or(old, |v| v.block_id(target));
                inner.pick(target, at, env)
            }
            Pattern::SurfaceSpread(distance, inner) => {
                let Some(view) = env.view else {
                    return inner.pick(pos, old, env);
                };
                let mut offset = |d: i32| env.rng.below(2 * d as u32 + 1) as i32 - d;
                let (dx, dy, dz) = (offset(*distance), offset(*distance), offset(*distance));
                let (x, z) = (pos.x + dx, pos.z + dz);
                match surface_y(view, x, pos.y + dy, z, ANGLE_RANGE) {
                    Some(y) => {
                        let target = BlockPos::new(x, y, z);
                        inner.pick(target, view.block_id(target), env)
                    }
                    None => old,
                }
            }
            Pattern::Buffer { two_d, inner } => {
                let key = (pos.x, if *two_d { 0 } else { pos.y }, pos.z);
                if env.buffer.insert(key) {
                    inner.pick(pos, old, env)
                } else {
                    old
                }
            }
            Pattern::Color(op) => pick_color(op, pos, old, env),
            Pattern::KeepStates(new) => *env
                .keep_cache
                .entry((old, *new))
                .or_insert_with(|| keep_states(old, *new)),
            Pattern::Clipboard => match env.clipboard {
                Some(cb) if cb.volume() > 0 => {
                    let rel = pos - env.origin;
                    cb.get(
                        rel.x.rem_euclid(cb.size_x as i32) as u32,
                        rel.y.rem_euclid(cb.size_y as i32) as u32,
                        rel.z.rem_euclid(cb.size_z as i32) as u32,
                    )
                }
                _ => old,
            },
        }
    }
}

fn pick_color(op: &ColorOp, pos: BlockPos, old: u32, env: &PatternEnv<'_>) -> u32 {
    let any = |_: u32, _: [u8; 3]| true;
    match op {
        ColorOp::Fixed(color) => closest_block(*color, any).unwrap_or(old),
        ColorOp::AngleData => {
            let Some(view) = env.view else { return old };
            if !Block::from_id(old).properties().contains_key("layers") {
                return old;
            }
            let angle = terrain_angle(view, pos, 1);
            let layers = (1.0 + angle / 90.0 * 7.0).round().clamp(1.0, 8.0) as u32;
            let mut block = Block::from_id(old);
            block.set_properties(
                [("layers", layers.to_string().as_str())]
                    .into_iter()
                    .collect(),
            );
            block.get_id()
        }
        _ => {
            let Some(color) = color_of(old) else {
                return old;
            };
            let old_luma = luma(color);
            match op {
                ColorOp::Saturate(c) => closest_block(mix(color, *c, 0.35), any),
                ColorOp::Average(c) => closest_block(mix(color, *c, 0.5), any),
                ColorOp::Desaturate(t) => {
                    let gray = luma(color).round() as u8;
                    closest_block(mix(color, [gray; 3], *t), any)
                }
                ColorOp::Darken => {
                    closest_block(mix(color, [0; 3], 0.2), |_, c| luma(c) < old_luma)
                }
                ColorOp::Lighten => {
                    closest_block(mix(color, [255; 3], 0.2), |_, c| luma(c) > old_luma)
                }
                ColorOp::AngleColor(distance) => {
                    let angle = env.view.map_or(0.0, |v| terrain_angle(v, pos, *distance));
                    let t = (angle / 90.0 * 0.7).clamp(0.0, 0.7);
                    closest_block(mix(color, [0; 3], t), any)
                }
                _ => None,
            }
            .unwrap_or(old)
        }
    }
}

/// A transform of what an edit places (`//gtransform`, `/transform`): where and which block ends
/// up in the world
#[derive(Clone, Debug, PartialEq)]
pub enum Transform {
    /// Places the block as it is
    Identity,
    /// `#offset[dx][dy][dz][transform]`
    Offset([i32; 3], Box<Transform>),
    /// `#rotate[rx][ry][rz][transform]`: multiples of 90 degrees around the origin of the edit
    Rotate([i32; 3], Box<Transform>),
    /// `#scale[sx][sy][sz][transform]`: whole numbers, every block becomes a box
    Scale([i32; 3], Box<Transform>),
    /// `#pattern[pattern][transform]`: the block is replaced by the pattern
    Pattern(Pattern, Box<Transform>),
    /// `#linear[t1,t2,...]` (dims 0, one after the other) and `#linear3d[t1,t2,...]`
    Linear { parts: Vec<Transform>, dims: u8 },
    /// `#spread[dx][dy][dz][transform]`: a random position nearby
    Spread([i32; 3], Box<Transform>),
}

impl Transform {
    pub fn parse(s: &str) -> PatternParseResult<Transform> {
        let s = s.trim();
        let invalid = || PatternParseError::InvalidPattern(s.to_owned());
        if s.is_empty() {
            return Err(invalid());
        }
        let items = split_top_level(s, ',');
        if items.len() > 1 {
            return Err(invalid());
        }
        let (name, args) = bracket_args(s).ok_or_else(invalid)?;
        let vector = |args: &[&str]| -> PatternParseResult<[i32; 3]> {
            Ok([
                parse_int(args[0], s)?,
                parse_int(args[1], s)?,
                parse_int(args[2], s)?,
            ])
        };
        let inner = |arg: Option<&&str>| -> PatternParseResult<Box<Transform>> {
            Ok(Box::new(match arg {
                Some(text) => Transform::parse(text)?,
                None => Transform::Identity,
            }))
        };
        match name {
            "#offset" | "#rotate" | "#scale" | "#spread" if args.len() == 3 || args.len() == 4 => {
                let v = vector(&args)?;
                let inner = inner(args.get(3))?;
                match name {
                    "#offset" => Ok(Transform::Offset(v, inner)),
                    "#rotate" => {
                        if v.iter().any(|a| a % 90 != 0) {
                            return Err(PatternParseError::InvalidPattern(format!(
                                "{s} (only multiples of 90 degrees)"
                            )));
                        }
                        Ok(Transform::Rotate(v, inner))
                    }
                    "#scale" => {
                        if v.iter().any(|a| *a < 1) {
                            return Err(PatternParseError::InvalidPattern(format!(
                                "{s} (only whole numbers of at least 1)"
                            )));
                        }
                        Ok(Transform::Scale(v, inner))
                    }
                    _ => Ok(Transform::Spread(v.map(|a| a.max(0)), inner)),
                }
            }
            "#pattern" if args.len() == 1 || args.len() == 2 => Ok(Transform::Pattern(
                Pattern::parse(args[0])?,
                inner(args.get(1))?,
            )),
            "#linear" | "#linear3d" if args.len() == 1 => {
                let parts = split_top_level(args[0], ',')
                    .into_iter()
                    .map(Transform::parse)
                    .collect::<PatternParseResult<Vec<_>>>()?;
                Ok(Transform::Linear {
                    parts,
                    dims: if name == "#linear" { 0 } else { 3 },
                })
            }
            _ => Err(PatternParseError::InvalidPattern(format!(
                "{s} (unknown transform)"
            ))),
        }
    }

    /// Where the block `block` that an edit wants to place at `pos` really goes. `origin` is the
    /// center of rotations and scaling, `env` gives the patterns what they need.
    pub fn apply(
        &self,
        pos: BlockPos,
        block: u32,
        origin: BlockPos,
        env: &mut PatternEnv<'_>,
        out: &mut Vec<(BlockPos, u32)>,
    ) {
        match self {
            Transform::Identity => out.push((pos, block)),
            Transform::Offset(d, inner) => inner.apply(
                BlockPos::new(pos.x + d[0], pos.y + d[1], pos.z + d[2]),
                block,
                origin,
                env,
                out,
            ),
            Transform::Rotate(angles, inner) => {
                use super::transform::{rotate_state, Axis};
                let mut rel = (pos.x - origin.x, pos.y - origin.y, pos.z - origin.z);
                let mut block = block;
                for (angle, axis) in [
                    (angles[0], Axis::X),
                    (angles[1], Axis::Y),
                    (angles[2], Axis::Z),
                ] {
                    for _ in 0..(angle / 90).rem_euclid(4) {
                        rel = match axis {
                            Axis::Y => (-rel.2, rel.1, rel.0),
                            Axis::X => (rel.0, rel.2, -rel.1),
                            Axis::Z => (rel.1, -rel.0, rel.2),
                        };
                        block = rotate_state(block, axis);
                    }
                }
                inner.apply(
                    BlockPos::new(origin.x + rel.0, origin.y + rel.1, origin.z + rel.2),
                    block,
                    origin,
                    env,
                    out,
                );
            }
            Transform::Scale(f, inner) => {
                let rel = (pos.x - origin.x, pos.y - origin.y, pos.z - origin.z);
                for dx in 0..f[0] {
                    for dy in 0..f[1] {
                        for dz in 0..f[2] {
                            inner.apply(
                                BlockPos::new(
                                    origin.x + rel.0 * f[0] + dx,
                                    origin.y + rel.1 * f[1] + dy,
                                    origin.z + rel.2 * f[2] + dz,
                                ),
                                block,
                                origin,
                                env,
                                out,
                            );
                        }
                    }
                }
            }
            Transform::Pattern(pattern, inner) => {
                let block = pattern.pick(pos, block, env);
                inner.apply(pos, block, origin, env, out)
            }
            Transform::Linear { parts, dims } => {
                let n = parts.len() as i64;
                let idx = if *dims == 0 {
                    let i = env.linear_counter;
                    env.linear_counter += 1;
                    (i % n as u64) as usize
                } else {
                    (pos.x as i64 + pos.y as i64 + pos.z as i64).rem_euclid(n) as usize
                };
                parts[idx].apply(pos, block, origin, env, out)
            }
            Transform::Spread(d, inner) => {
                let mut offset = |d: i32| env.rng.below(2 * d as u32 + 1) as i32 - d;
                let at = BlockPos::new(
                    pos.x + offset(d[0]),
                    pos.y + offset(d[1]),
                    pos.z + offset(d[2]),
                );
                inner.apply(at, block, origin, env, out)
            }
        }
    }
}

/// A transform with the position it rotates and scales around
#[derive(Clone, Debug, PartialEq)]
pub struct GlobalTransform {
    pub transform: Transform,
    pub origin: BlockPos,
}

impl GlobalTransform {
    pub fn parse(text: &str, origin: BlockPos) -> PatternParseResult<GlobalTransform> {
        Ok(GlobalTransform {
            transform: Transform::parse(text)?,
            origin,
        })
    }
}

static GLOBAL_TRANSFORMS: LazyLock<std::sync::Mutex<FxHashMap<u128, GlobalTransform>>> =
    LazyLock::new(|| std::sync::Mutex::new(FxHashMap::default()));

/// `//gtransform`: the transform every edit of the player goes through (none removes it)
pub fn set_global_transform(owner: u128, transform: Option<GlobalTransform>) {
    let mut map = GLOBAL_TRANSFORMS.lock().unwrap_or_else(|e| e.into_inner());
    match transform {
        Some(t) => map.insert(owner, t),
        None => map.remove(&owner),
    };
}

pub fn global_transform(owner: u128) -> Option<GlobalTransform> {
    let map = GLOBAL_TRANSFORMS.lock().unwrap_or_else(|e| e.into_inner());
    map.get(&owner).cloned()
}

fn block_text(id: u32) -> String {
    let block = Block::from_id(id);
    let name = block.get_name().trim_start_matches("minecraft:");
    let props = block.properties();
    if props.is_empty() {
        return name.to_owned();
    }
    let mut props: Vec<_> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
    props.sort();
    format!("{name}[{}]", props.join(","))
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pattern::Single(id) => write!(f, "{}", block_text(*id)),
            Pattern::KeepStates(id) => write!(f, "^{}", block_text(*id)),
            Pattern::Clipboard => write!(f, "#clipboard"),
            Pattern::Existing => write!(f, "#existing"),
            Pattern::Noise { kind, scale, inner } => {
                let name = match kind {
                    NoiseKind::Simplex => "simplex",
                    NoiseKind::Perlin => "perlin",
                    NoiseKind::Rmf => "rmf",
                    NoiseKind::Voronoi => "voronoi",
                };
                write!(f, "#{name}[{scale}][{inner}]")
            }
            Pattern::Linear { parts, dims, .. } => {
                let name = ["linear", "", "linear2d", "linear3d"][*dims as usize];
                let list: Vec<String> = parts.iter().map(Pattern::to_string).collect();
                write!(f, "#{name}[{}]", list.join(","))
            }
            Pattern::Offset(d, inner) => {
                write!(f, "#offset[{}][{}][{}][{inner}]", d[0], d[1], d[2])
            }
            Pattern::Relative(inner) => write!(f, "#relative[{inner}]"),
            Pattern::Mask { yes, no, .. } => write!(f, "#mask[..][{yes}][{no}]"),
            Pattern::DropAxis(axis, inner) => {
                write!(f, "#!{}[{inner}]", ["x", "y", "z"][*axis])
            }
            Pattern::Spread { solid, d, inner } => write!(
                f,
                "#{}spread[{}][{}][{}][{inner}]",
                if *solid { "solid" } else { "" },
                d[0],
                d[1],
                d[2]
            ),
            Pattern::SurfaceSpread(d, inner) => write!(f, "#surfacespread[{d}][{inner}]"),
            Pattern::Buffer { two_d, inner } => {
                write!(f, "#buffer{}[{inner}]", if *two_d { "2d" } else { "" })
            }
            Pattern::Color(op) => write!(f, "#{}", format!("{op:?}").to_lowercase()),
            Pattern::Random { parts, .. } => {
                for (i, (weight, pattern)) in parts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{weight}%{pattern}")?;
                }
                Ok(())
            }
        }
    }
}

impl FromStr for Pattern {
    type Err = PatternParseError;

    fn from_str(s: &str) -> PatternParseResult<Pattern> {
        Pattern::parse(s)
    }
}

/// `new` with all the properties of `old` that `new` has too
fn keep_states(old: u32, new: u32) -> u32 {
    let old_props = Block::from_id(old).properties();
    let props = old_props
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    let mut block = Block::from_id(new);
    block.set_properties(props);
    block.get_id()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn id(name: &str) -> u32 {
        Block::from_name(&format!("minecraft:{name}"))
            .unwrap()
            .get_id()
    }

    fn state(spec: &str) -> u32 {
        let spec = parse_block_spec(spec).unwrap();
        block_state_of(&spec, "").unwrap()
    }

    fn env() -> PatternEnv<'static> {
        let mut env = PatternEnv::new(None, BlockPos::zero());
        env.rng = FastRng::seeded(0x1234_5678);
        env
    }

    struct MapView(HashMap<BlockPos, u32>);

    impl BlockView for MapView {
        fn block_id(&self, pos: BlockPos) -> u32 {
            self.0.get(&pos).copied().unwrap_or(0)
        }
    }

    #[test]
    fn single_block() {
        assert_eq!(
            Pattern::parse("stone").unwrap(),
            Pattern::Single(id("stone"))
        );
        assert_eq!(
            Pattern::parse("minecraft:stone").unwrap(),
            Pattern::Single(id("stone"))
        );
        assert_eq!(Pattern::parse("air").unwrap().as_single(), Some(0));
    }

    #[test]
    fn raw_id() {
        assert_eq!(Pattern::parse("=1").unwrap(), Pattern::Single(1));
        assert!(Pattern::parse("=99999999").is_err());
    }

    #[test]
    fn block_properties() {
        let pattern = Pattern::parse("oak_stairs[facing=east,half=top,shape=inner_left]").unwrap();
        let block = Block::from_id(pattern.as_single().unwrap());
        let props = block.properties();
        assert_eq!(block.get_name(), "minecraft:oak_stairs");
        assert_eq!(props["facing"], "east");
        assert_eq!(props["half"], "top");
        assert_eq!(props["shape"], "inner_left");
    }

    #[test]
    fn invalid_patterns() {
        assert!(matches!(
            Pattern::parse("not_a_block"),
            Err(PatternParseError::UnknownBlock(_))
        ));
        assert!(matches!(
            Pattern::parse("oak_stairs[facing=sideways]"),
            Err(PatternParseError::InvalidProperty(_))
        ));
        assert!(matches!(
            Pattern::parse("oak_stairs[nonsense=1]"),
            Err(PatternParseError::InvalidProperty(_))
        ));
        assert!(Pattern::parse("").is_err());
        assert!(Pattern::parse("stone[").is_err());
        assert!(Pattern::parse("x%stone").is_err());
        assert!(Pattern::parse("*").is_err());
    }

    #[test]
    fn commas_in_properties_do_not_split() {
        let pattern = Pattern::parse("oak_stairs[facing=east,half=top],stone").unwrap();
        let Pattern::Random { parts, .. } = pattern else {
            panic!("expected a random pattern");
        };
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn weighted_pattern_distribution() {
        let pattern = Pattern::parse("75%stone,25%dirt").unwrap();
        let mut env = env();
        let stone = id("stone");
        let mut stones = 0;
        const N: u32 = 20_000;
        for _ in 0..N {
            let picked = pattern.pick(BlockPos::zero(), 0, &mut env);
            assert!(picked == stone || picked == id("dirt"));
            stones += (picked == stone) as u32;
        }
        let ratio = stones as f32 / N as f32;
        assert!((ratio - 0.75).abs() < 0.02, "ratio was {ratio}");
    }

    #[test]
    fn unweighted_alternatives_are_equal() {
        let pattern = Pattern::parse("stone,dirt").unwrap();
        let mut env = env();
        let dirt = id("dirt");
        let dirts = (0..10_000)
            .filter(|_| pattern.pick(BlockPos::zero(), 0, &mut env) == dirt)
            .count();
        assert!((4500..5500).contains(&dirts), "dirt count was {dirts}");
    }

    #[test]
    fn keep_states_pattern() {
        let old = state("oak_stairs[facing=south,half=top,shape=straight,waterlogged=false]");
        let pattern = Pattern::parse("^stone_stairs").unwrap();
        let new = Block::from_id(pattern.pick(BlockPos::zero(), old, &mut env()));
        let props = new.properties();
        assert_eq!(new.get_name(), "minecraft:stone_stairs");
        assert_eq!(props["facing"], "south");
        assert_eq!(props["half"], "top");

        // Blocks without those properties just become the block
        let stone = Pattern::parse("^stone").unwrap();
        assert_eq!(stone.pick(BlockPos::zero(), old, &mut env()), id("stone"));
    }

    #[test]
    fn clipboard_pattern_tiles() {
        let cb = WorldEditClipboard {
            offset_x: 0,
            offset_y: 0,
            offset_z: 0,
            size_x: 2,
            size_y: 1,
            size_z: 1,
            data: vec![id("stone") as u16, id("dirt") as u16],
            block_entities: Default::default(),
        };
        let pattern = Pattern::parse("#clipboard").unwrap();
        assert!(pattern.needs_clipboard());
        let mut env = PatternEnv::new(Some(&cb), BlockPos::new(10, 5, 10));
        let at = |env: &mut PatternEnv<'_>, x, y, z| pattern.pick(BlockPos::new(x, y, z), 0, env);
        assert_eq!(at(&mut env, 10, 5, 10), id("stone"));
        assert_eq!(at(&mut env, 11, 5, 10), id("dirt"));
        assert_eq!(at(&mut env, 12, 9, 12), id("stone"));
        assert_eq!(at(&mut env, 9, 5, 10), id("dirt"));
    }

    #[test]
    fn simple_masks_match_any_state() {
        let mask = Mask::parse("oak_stairs").unwrap();
        assert!(mask.is_state_only());
        assert!(mask.test_state(state("oak_stairs[facing=west,half=top]")));
        assert!(mask.test_state(state("oak_stairs[facing=north]")));
        assert!(!mask.test_state(state("stone_stairs[facing=north]")));
    }

    #[test]
    fn property_masks() {
        let mask = Mask::parse("oak_stairs[facing=north]").unwrap();
        assert!(mask.test_state(state("oak_stairs[facing=north,half=top]")));
        assert!(!mask.test_state(state("oak_stairs[facing=south]")));
        assert!(matches!(
            Mask::parse("oak_stairs[facing=banana]"),
            Err(PatternParseError::InvalidProperty(_))
        ));
    }

    #[test]
    fn mask_alternatives_negation_and_intersection() {
        let mask = Mask::parse("stone,dirt").unwrap();
        assert!(matches!(mask, Mask::States(_)));
        assert!(mask.test_state(id("stone")) && mask.test_state(id("dirt")));
        assert!(!mask.test_state(id("glass")));

        let not = Mask::parse("!stone,dirt").unwrap();
        // `,` binds weaker than `!`
        assert!(!not.test_state(id("stone")));
        assert!(not.test_state(id("dirt")));
        assert!(not.test_state(id("glass")));

        let and = Mask::parse("#existing&!stone").unwrap();
        assert!(!and.test_state(0));
        assert!(!and.test_state(id("stone")));
        assert!(and.test_state(id("glass")));
    }

    #[test]
    fn special_masks() {
        let air = Mask::parse("#air").unwrap();
        assert!(air.test_state(0) && !air.test_state(id("stone")));
        let existing = Mask::parse("#existing").unwrap();
        assert!(!existing.test_state(0) && existing.test_state(id("stone")));
        let solid = Mask::parse("#solid").unwrap();
        assert!(solid.test_state(id("stone")));
        assert!(!solid.test_state(0));
    }

    #[test]
    fn wildcard_and_tag_masks() {
        let stairs = Mask::parse("*_stairs").unwrap();
        assert!(stairs.test_state(id("oak_stairs")));
        assert!(stairs.test_state(id("stone_brick_stairs")));
        assert!(!stairs.test_state(id("oak_planks")));

        let oak = Mask::parse("oak_*").unwrap();
        assert!(oak.test_state(id("oak_planks")) && oak.test_state(id("oak_stairs")));
        assert!(!oak.test_state(id("birch_planks")));

        let wool = Mask::parse("##wool").unwrap();
        assert!(wool.test_state(id("red_wool")));
        assert!(!wool.test_state(id("red_concrete")));
        assert!(matches!(
            Mask::parse("##nothing"),
            Err(PatternParseError::UnknownTag(_))
        ));
    }

    #[test]
    fn glob() {
        assert!(glob_match("a*c", "abc"));
        assert!(glob_match("a*c", "ac"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a*b*c", "a--b--c"));
        assert!(!glob_match("a*b*c", "a--c--b"));
        assert!(!glob_match("a*c", "abd"));
        assert!(!glob_match("ab*ba", "aba"));
    }

    #[test]
    fn above_and_below_masks() {
        let stone = id("stone");
        let view = MapView(HashMap::from([
            (BlockPos::new(0, 1, 0), stone),
            (BlockPos::new(5, 0, 5), stone),
        ]));
        let mut rng = FastRng::seeded(1);
        let above = Mask::parse(">stone").unwrap();
        assert!(!above.is_state_only());
        assert!(above.test(&view, BlockPos::new(0, 0, 0), 0, &mut rng));
        assert!(!above.test(&view, BlockPos::new(1, 0, 0), 0, &mut rng));
        let below = Mask::parse("<stone").unwrap();
        assert!(below.test(&view, BlockPos::new(5, 1, 5), 0, &mut rng));
        assert!(!below.test(&view, BlockPos::new(0, 0, 0), 0, &mut rng));
        let combined = Mask::parse("#air&<stone").unwrap();
        assert!(combined.test(&view, BlockPos::new(5, 1, 5), 0, &mut rng));
        assert!(!combined.test(&view, BlockPos::new(5, 1, 5), stone, &mut rng));
    }

    #[test]
    fn random_mask() {
        let mask = Mask::parse("%25").unwrap();
        let view = MapView(HashMap::new());
        let mut rng = FastRng::seeded(99);
        let hits = (0..20_000)
            .filter(|_| mask.test(&view, BlockPos::zero(), 0, &mut rng))
            .count();
        assert!((4500..5500).contains(&hits), "hits were {hits}");
        assert!(Mask::parse("%abc").is_err());
    }

    #[test]
    fn fast_rng_bounds() {
        let mut rng = FastRng::seeded(7);
        for _ in 0..1000 {
            assert!(rng.below(10) < 10);
            let f = rng.next_f32();
            assert!((0.0..1.0).contains(&f));
        }
    }

    #[test]
    fn bracket_argument_splitting() {
        assert_eq!(bracket_args("stone"), Some(("stone", vec![])));
        assert_eq!(
            bracket_args("#simplex[10][a[x=1,y=2],b]"),
            Some(("#simplex", vec!["10", "a[x=1,y=2],b"]))
        );
        assert_eq!(bracket_args("#x[1]junk"), None);
        assert_eq!(bracket_args("#x[1"), None);
    }

    #[test]
    fn noise_patterns_pick_from_the_list() {
        let pattern = Pattern::parse("#simplex[5][stone,dirt]").unwrap();
        let mut env = env();
        let (stone, dirt) = (id("stone"), id("dirt"));
        let mut seen = [0usize; 2];
        for x in 0..40 {
            for z in 0..40 {
                let got = pattern.pick(BlockPos::new(x, 3, z), 0, &mut env);
                assert!(got == stone || got == dirt);
                seen[(got == dirt) as usize] += 1;
            }
        }
        assert!(seen[0] > 100 && seen[1] > 100, "{seen:?}");
        // The same position always gives the same block
        let a = pattern.pick(BlockPos::new(7, 1, 9), 0, &mut env);
        assert_eq!(a, pattern.pick(BlockPos::new(7, 1, 9), 0, &mut env));
        for name in ["perlin", "rmf", "voronoi"] {
            assert!(Pattern::parse(&format!("#{name}[stone,dirt]")).is_ok());
        }
        assert!(Pattern::parse("#simplex[0][stone]").is_err());
        assert!(Pattern::parse("#simplex[10][nothing]").is_err());
    }

    #[test]
    fn linear_patterns() {
        let mut env = env();
        let seq = Pattern::parse("#linear[stone,dirt,glass]").unwrap();
        let got: Vec<u32> = (0..4)
            .map(|_| seq.pick(BlockPos::new(0, 0, 0), 0, &mut env))
            .collect();
        assert_eq!(got, vec![id("stone"), id("dirt"), id("glass"), id("stone")]);
        let l3 = Pattern::parse("#linear3d[stone,dirt][2][1][1]").unwrap();
        assert_eq!(l3.pick(BlockPos::new(0, 0, 0), 0, &mut env), id("stone"));
        assert_eq!(l3.pick(BlockPos::new(2, 0, 0), 0, &mut env), id("dirt"));
        assert_eq!(l3.pick(BlockPos::new(1, 0, 0), 0, &mut env), id("stone"));
        assert_eq!(l3.pick(BlockPos::new(0, 1, 0), 0, &mut env), id("dirt"));
        let l2 = Pattern::parse("#linear2d[stone,dirt]").unwrap();
        assert_eq!(l2.pick(BlockPos::new(1, 99, 0), 0, &mut env), id("dirt"));
    }

    #[test]
    fn wrapper_patterns() {
        let mut env = env();
        assert_eq!(
            Pattern::parse("#existing")
                .unwrap()
                .pick(BlockPos::new(0, 0, 0), 5, &mut env),
            5
        );
        let off = Pattern::parse("#offset[1][0][0][#linear3d[stone,dirt]]").unwrap();
        assert_eq!(off.pick(BlockPos::new(0, 0, 0), 0, &mut env), id("dirt"));
        let m = Pattern::parse("#mask[stone][dirt][glass]").unwrap();
        assert_eq!(
            m.pick(BlockPos::new(0, 0, 0), id("stone"), &mut env),
            id("dirt")
        );
        assert_eq!(m.pick(BlockPos::new(0, 0, 0), 0, &mut env), id("glass"));
        assert!(Pattern::parse("#mask[>stone][dirt][glass]").is_ok());
        let nx = Pattern::parse("#!x[#linear3d[stone,dirt]]").unwrap();
        assert_eq!(nx.pick(BlockPos::new(1, 0, 0), 0, &mut env), id("stone"));
        assert!(Pattern::parse("#doesnotexist").is_err());
        // Percent weights in front of special patterns
        assert!(Pattern::parse("50%#existing,50%stone").is_ok());
    }

    #[test]
    fn new_masks() {
        let mut rng = FastRng::seeded(1);
        assert!(Mask::parse("true").unwrap().test_state(5) || true);
        assert!(!Mask::parse("false").unwrap().test_state(id("stone")));
        let liquid = Mask::parse("#liquid").unwrap();
        assert!(liquid.test_state(id("water")) && !liquid.test_state(id("stone")));
        assert!(Mask::parse("#offset[0][1][0]").is_err());
        assert!(Mask::parse("#bogus").is_err());
        let _ = (&mut rng, Mask::parse("#simplex[5][20][80]").unwrap());
        let _ = Mask::parse("#offset[0][1][0][air]&~[stone][2][6]&#surface&#wall").unwrap();
    }

    /// A world with stone below y=5 and a one block step up at x>=0 in its right half
    struct Terrain;

    impl BlockView for Terrain {
        fn block_id(&self, pos: BlockPos) -> u32 {
            let height = if pos.x >= 10 { 8 } else { 5 };
            if pos.y < height {
                id("stone")
            } else {
                0
            }
        }
    }

    #[test]
    fn spread_and_surface_patterns_use_the_view() {
        let terrain = Terrain;
        let mut env = env();
        env.view = Some(&terrain);
        let spread = Pattern::parse("#spread[0][3][0][#existing]").unwrap();
        assert!(spread.needs_world() && spread.reach() >= 3);
        let mut seen = FxHashSet::default();
        for _ in 0..200 {
            seen.insert(spread.pick(BlockPos::new(0, 4, 0), 0, &mut env));
        }
        // Blocks from above and below the surface
        assert!(seen.contains(&0) && seen.contains(&id("stone")));
        let solid = Pattern::parse("#solidspread[1][3][1][dirt]").unwrap();
        assert_eq!(solid.pick(BlockPos::new(0, 4, 0), 0, &mut env), id("dirt"));
        let surface = Pattern::parse("#surfacespread[2][#existing]").unwrap();
        for _ in 0..50 {
            let got = surface.pick(BlockPos::new(0, 4, 0), 0, &mut env);
            assert!(got == id("stone") || got == 0);
        }
        assert!(Pattern::parse("#spread[1][1][dirt]").is_err());
    }

    #[test]
    fn buffer_places_once() {
        let mut env = env();
        let p = Pattern::parse("#buffer[stone]").unwrap();
        assert_eq!(p.pick(BlockPos::new(1, 1, 1), 7, &mut env), id("stone"));
        assert_eq!(p.pick(BlockPos::new(1, 1, 1), 7, &mut env), 7);
        let p2 = Pattern::parse("#buffer2d[stone]").unwrap();
        assert_eq!(p2.pick(BlockPos::new(2, 1, 2), 7, &mut env), id("stone"));
        assert_eq!(p2.pick(BlockPos::new(2, 9, 2), 7, &mut env), 7);
    }

    #[test]
    fn color_patterns() {
        let mut env = env();
        let at = BlockPos::new(0, 0, 0);
        let red = Pattern::parse("#color[#8e2020]")
            .unwrap()
            .pick(at, 0, &mut env);
        assert_eq!(red, id("red_concrete"));
        assert_eq!(
            Pattern::parse("#color[255,255,255]")
                .unwrap()
                .pick(at, 0, &mut env),
            id("snow_block")
        );
        let darker = Pattern::parse("#darken")
            .unwrap()
            .pick(at, id("white_concrete"), &mut env);
        assert_ne!(darker, id("white_concrete"));
        let lighter = Pattern::parse("#lighten")
            .unwrap()
            .pick(at, id("stone"), &mut env);
        assert_ne!(lighter, id("stone"));
        // Blocks without a color stay
        assert_eq!(Pattern::parse("#darken").unwrap().pick(at, 0, &mut env), 0);
        assert!(Pattern::parse("#saturate[10][20][30]").is_ok());
        assert!(Pattern::parse("#averagecolor[red]").is_ok());
        assert!(Pattern::parse("#desaturate[40]").is_ok());
        assert!(Pattern::parse("#color[nothing]").is_err());
        let terrain = Terrain;
        env.view = Some(&terrain);
        let steep = Pattern::parse("#anglecolor[1]").unwrap();
        let flat = steep.pick(BlockPos::new(2, 4, 2), id("white_concrete"), &mut env);
        let edge = steep.pick(BlockPos::new(9, 4, 2), id("white_concrete"), &mut env);
        assert!(luma(color_of(edge).unwrap()) < luma(color_of(flat).unwrap()));
    }

    #[test]
    fn tag_patterns_and_masks() {
        let p = Pattern::parse("##wool").unwrap();
        let mut env = env();
        for _ in 0..20 {
            let got = p.pick(BlockPos::zero(), 0, &mut env);
            assert!(state_names()[got as usize].ends_with("_wool"));
        }
        assert!(Pattern::parse("$carpets").is_ok());
        assert!(Mask::parse("$wool").unwrap().test_state(id("red_wool")));
        assert!(Pattern::parse("$nothing").is_err());
    }

    #[test]
    fn context_masks() {
        let terrain = Terrain;
        let mut rng = FastRng::seeded(3);
        assert!(Mask::parse("#xaxis").is_err());
        set_parse_context(ParseContext {
            origin: Some(BlockPos::new(2, 4, 2)),
            region: Some((BlockPos::new(0, 0, 0), BlockPos::new(5, 5, 5))),
        });
        let x = Mask::parse("#xaxis").unwrap();
        assert!(x.test(&terrain, BlockPos::new(9, 4, 2), 0, &mut rng));
        assert!(!x.test(&terrain, BlockPos::new(9, 5, 2), 0, &mut rng));
        let region = Mask::parse("#region").unwrap();
        assert!(region.test(&terrain, BlockPos::new(5, 5, 5), 0, &mut rng));
        assert!(!region.test(&terrain, BlockPos::new(6, 5, 5), 0, &mut rng));
        let radius = Mask::parse("{1 3").unwrap();
        assert!(radius.test(&terrain, BlockPos::new(4, 4, 2), 0, &mut rng));
        assert!(!radius.test(&terrain, BlockPos::new(2, 4, 2), 0, &mut rng));
        let same = Mask::parse("#id").unwrap();
        // The origin is inside of the stone
        assert!(same.test(&terrain, BlockPos::new(0, 0, 0), id("stone"), &mut rng));
        assert!(!same.test(&terrain, BlockPos::new(0, 0, 0), id("dirt"), &mut rng));
        let angle = Mask::parse("/[1][5]").unwrap();
        assert!(angle.test(&terrain, BlockPos::new(9, 4, 0), id("stone"), &mut rng));
        assert!(!angle.test(&terrain, BlockPos::new(3, 4, 0), id("stone"), &mut rng));
        let degrees = Mask::parse("\\[30d][90d]").unwrap();
        assert!(degrees.test(&terrain, BlockPos::new(9, 4, 0), id("stone"), &mut rng));
        set_parse_context(ParseContext::default());
        assert!(Mask::parse("#region").is_err());
    }

    #[test]
    fn light_masks() {
        struct Lit;
        impl BlockView for Lit {
            fn block_id(&self, _: BlockPos) -> u32 {
                0
            }
            fn sky_light(&self, pos: BlockPos) -> u8 {
                pos.y.clamp(0, 15) as u8
            }
        }
        let mut rng = FastRng::seeded(5);
        let lit = Lit;
        let light = Mask::parse("#light[5][10]").unwrap();
        assert!(light.test(&lit, BlockPos::new(0, 7, 0), 0, &mut rng));
        assert!(!light.test(&lit, BlockPos::new(0, 12, 0), 0, &mut rng));
        assert!(Mask::parse("#skylight[0][0]")
            .unwrap()
            .test(&lit, BlockPos::zero(), 0, &mut rng));
        assert!(Mask::parse("#nolight")
            .unwrap()
            .test(&lit, BlockPos::zero(), 0, &mut rng));
        assert!(Mask::parse("#haslight")
            .unwrap()
            .test(&lit, BlockPos::new(0, 3, 0), 0, &mut rng));
        assert!(Mask::parse("#brightness[15][15]").unwrap().test_state(0) == false);
        let _ = Mask::parse("#opacity[1][15]").unwrap();
        let bright = Mask::parse("#brightness[10]").unwrap();
        assert!(bright.test(&lit, BlockPos::zero(), id("glowstone"), &mut rng));
        assert!(!bright.test(&lit, BlockPos::zero(), id("stone"), &mut rng));
    }

    #[test]
    fn mask_pattern_with_world_mask() {
        let terrain = Terrain;
        let mut env = env();
        env.view = Some(&terrain);
        let p = Pattern::parse("#mask[>air][dirt][glass]").unwrap();
        assert!(p.needs_world());
        // Above y=4 is air (the surface is at 5)
        assert_eq!(
            p.pick(BlockPos::new(0, 4, 0), id("stone"), &mut env),
            id("dirt")
        );
        assert_eq!(
            p.pick(BlockPos::new(0, 1, 0), id("stone"), &mut env),
            id("glass")
        );
    }

    #[test]
    fn transforms() {
        let mut env = env();
        let origin = BlockPos::new(0, 0, 0);
        let run = |text: &str, pos: BlockPos, env: &mut PatternEnv<'_>| {
            let mut out = Vec::new();
            Transform::parse(text)
                .unwrap()
                .apply(pos, id("stone"), origin, env, &mut out);
            out
        };
        let at = BlockPos::new(1, 0, 0);
        assert_eq!(
            run("#offset[0][5][0]", at, &mut env),
            vec![(BlockPos::new(1, 5, 0), id("stone"))]
        );
        assert_eq!(
            run("#rotate[0][90][0]", at, &mut env)[0].0,
            BlockPos::new(0, 0, 1)
        );
        assert_eq!(run("#scale[2][1][2]", at, &mut env).len(), 4);
        assert_eq!(run("#pattern[dirt]", at, &mut env)[0].1, id("dirt"));
        assert_eq!(
            run("#offset[1][0][0][#pattern[dirt]]", at, &mut env)[0],
            (BlockPos::new(2, 0, 0), id("dirt"))
        );
        let a = run("#linear[#offset[0][1][0],#offset[0][2][0]]", at, &mut env)[0].0;
        let b = run("#linear[#offset[0][1][0],#offset[0][2][0]]", at, &mut env)[0].0;
        assert_ne!(a, b);
        let spread = run("#spread[1][0][0]", at, &mut env)[0].0;
        assert!((spread.x - 1).abs() <= 1 && spread.y == 0);
        assert!(Transform::parse("#rotate[0][45][0]").is_err());
        assert!(Transform::parse("#scale[0][1][1]").is_err());
        assert!(Transform::parse("#bogus").is_err());
    }
}
