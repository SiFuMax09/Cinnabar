use super::*;

#[test]
fn every_alias_points_to_a_command() {
    for (alias, target) in ALIASES.iter() {
        let name = target.split_whitespace().next().unwrap();
        assert!(COMMANDS.contains_key(name), "{alias} -> {target}");
    }
}

#[test]
fn completions_can_be_built() {
    let mut nodes = vec![CCommandsNode {
        flags: 0,
        children: Vec::new(),
        redirect_node: None,
        name: None,
        parser: None,
        suggestions_type: None,
    }];
    add_command_completions(&mut nodes);
    assert!(nodes.len() > COMMANDS.len());
}

#[test]
fn default_arguments_parse() {
    // Every default written as text has to be a valid value of its type
    for (name, command) in COMMANDS.iter() {
        for arg in command.arguments {
            if let Some(default) = arg.default_str {
                let ok = match arg.argument_type {
                    ArgumentType::Pattern => Pattern::parse(default).is_ok(),
                    ArgumentType::Mask => Mask::parse(default).is_ok(),
                    _ => true,
                };
                assert!(ok, "{name} {}", arg.name);
            }
        }
    }
}

#[test]
fn region_commands_match_fawe() {
    for alias in ["/re", "/rep", "/mv", "/replacenear"] {
        assert!(ALIASES.contains_key(alias), "{alias}");
    }
    assert!(COMMANDS.contains_key("replacenear"));
    assert!(COMMANDS.contains_key("/fall"));
    assert_eq!(COMMANDS["/set"].permission_node, "worldedit.region.set");
    let flags = |name: &str| -> String {
        let mut letters: Vec<char> = COMMANDS[name].flags.iter().map(|f| f.letter).collect();
        letters.sort();
        letters.into_iter().collect()
    };
    assert_eq!(flags("/move"), "abems");
    assert_eq!(flags("/stack"), "abemrs");
    assert_eq!(flags("/fall"), "m");
    let mask_flag = COMMANDS["/stack"]
        .flags
        .iter()
        .find(|f| f.letter == 'm')
        .unwrap();
    assert!(matches!(mask_flag.argument_type, Some(ArgumentType::Mask)));
}

#[test]
fn direction_names() {
    let facing = BlockFacing::East;
    assert_eq!(parse_facing_name("me", facing), Some(BlockFacing::East));
    assert_eq!(parse_facing_name("f", facing), Some(BlockFacing::East));
    assert_eq!(parse_facing_name("back", facing), Some(BlockFacing::West));
    assert_eq!(parse_facing_name("l", facing), Some(facing.rotate_ccw()));
    assert_eq!(parse_facing_name("north", facing), Some(BlockFacing::North));
    assert_eq!(parse_facing_name("W", facing), Some(BlockFacing::West));
    assert_eq!(parse_facing_name("up", facing), Some(BlockFacing::Up));
    assert_eq!(parse_facing_name("sideways", facing), None);
}

#[test]
fn selection_and_navigation_commands_exist() {
    for name in [
        "/pos1",
        "/pos2",
        "/hpos1",
        "/hpos2",
        "/sel",
        "/wand",
        "/expand",
        "/contract",
        "/shift",
        "/outset",
        "/inset",
        "/size",
        "/count",
        "/distr",
        "/chunk",
        "/rotate",
        "/flip",
        "up",
        "ascend",
        "descend",
        "ceil",
        "thru",
        "jumpto",
        "unstuck",
        "size",
    ] {
        assert!(COMMANDS.contains_key(name), "{name}");
    }
    for (alias, target) in [
        ("!", "unstuck"),
        ("j", "jumpto"),
        ("asc", "ascend"),
        ("desc", "descend"),
        (";", "/sel"),
        ("/desel", "/sel"),
        ("/deselect", "/sel"),
    ] {
        assert_eq!(ALIASES.get(alias), Some(&target), "{alias}");
    }
}

#[test]
fn directions_include_compass_names() {
    assert_eq!(
        parse_facing_name("north", BlockFacing::East),
        Some(BlockFacing::North)
    );
    assert_eq!(
        parse_facing_name("w", BlockFacing::East),
        Some(BlockFacing::West)
    );
    assert_eq!(
        parse_facing_name("me", BlockFacing::East),
        Some(BlockFacing::East)
    );
    assert_eq!(
        parse_facing_name("up", BlockFacing::East),
        Some(BlockFacing::Up)
    );
    assert_eq!(parse_facing_name("sideways", BlockFacing::East), None);
}

#[test]
fn expanding_and_contracting_corners() {
    let a = BlockPos::new(0, 0, 0);
    let b = BlockPos::new(4, 4, 4);
    // Expanding upwards moves the upper corner, downwards the lower one
    assert_eq!(
        adjusted_corners(a, b, BlockPos::new(0, 3, 0), false),
        (a, BlockPos::new(4, 7, 4))
    );
    assert_eq!(
        adjusted_corners(a, b, BlockPos::new(0, -3, 0), false),
        (BlockPos::new(0, -3, 0), b)
    );
    // The order of the corners does not matter
    assert_eq!(
        adjusted_corners(b, a, BlockPos::new(2, 0, 0), false),
        (BlockPos::new(6, 4, 4), a)
    );
    // Contracting from the top and bottom
    assert_eq!(
        adjusted_corners(a, b, BlockPos::new(0, 2, 0), true),
        (BlockPos::new(0, 2, 0), b)
    );
    assert_eq!(
        adjusted_corners(a, b, BlockPos::new(0, -2, 0), true),
        (a, BlockPos::new(4, 2, 4))
    );
}
