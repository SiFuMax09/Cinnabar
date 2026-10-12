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

use mchprs_blocks::blocks::Block;
use mchprs_blocks::{generic, BlockPos};
use mchprs_schematic::WorldEditClipboard;
use mchprs_world::World;
use rand::RngExt;
use rustc_hash::FxHashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

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
}

impl<'a> PatternEnv<'a> {
    pub fn new(clipboard: Option<&'a WorldEditClipboard>, origin: BlockPos) -> PatternEnv<'a> {
        PatternEnv {
            rng: FastRng::new(),
            clipboard,
            origin,
            keep_cache: FxHashMap::default(),
        }
    }
}

/// Read access to the blocks of a world, used by masks that look at neighbouring blocks
pub trait BlockView {
    fn block_id(&self, pos: BlockPos) -> u32;
}

impl<W: World> BlockView for W {
    fn block_id(&self, pos: BlockPos) -> u32 {
        self.get_block_raw(pos)
    }
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
        if let Some(tag) = s.strip_prefix("##") {
            let predicate = tag_predicate(tag).ok_or_else(|| {
                PatternParseError::UnknownTag(format!("{tag} (known tags: {TAG_NAMES})"))
            })?;
            return Ok(Mask::States(StateSet::from_predicate(|id| {
                predicate(state_names()[id as usize])
            })));
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
            Mask::Above(_) | Mask::Below(_) | Mask::Random(_) => false,
        }
    }

    /// Tests a block state. Masks that are not [state only](Mask::is_state_only) never match.
    pub fn test_state(&self, id: u32) -> bool {
        match self {
            Mask::States(set) => set.contains(id),
            Mask::Not(mask) => !mask.test_state(id),
            Mask::And(masks) => masks.iter().all(|mask| mask.test_state(id)),
            Mask::Or(masks) => masks.iter().any(|mask| mask.test_state(id)),
            Mask::Above(_) | Mask::Below(_) | Mask::Random(_) => false,
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
            Some((weight, rest)) => (weight.parse::<f32>().map_err(|_| invalid())?, rest),
            None => (1.0, item),
        };
        if weight < 0.0 {
            return Err(invalid());
        }
        let pattern = if rest == "#clipboard" {
            Pattern::Clipboard
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
}
