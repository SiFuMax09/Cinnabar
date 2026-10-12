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
