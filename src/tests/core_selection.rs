    use super::*;
// Insert in src/main.rs existing #[cfg(test)] mod tests: tests actual Cli, not a facsimile.
#[test]
fn agent_core_flag_is_explicit_and_defaults_off() {
    for (args, expected) in [
        (vec!["monocle", "agent", "q"], false),
        (vec!["monocle", "agent", "q", "--experimental-core-model-only"], true),
    ] {
        let parsed = Cli::try_parse_from(args).unwrap();
        match parsed.command {
            Commands::Agent { experimental_core_model_only, .. } => assert_eq!(experimental_core_model_only, expected),
            _ => panic!("wrong command"),
        }
    }
}
#[test]
fn experimental_core_flag_does_not_enable_other_routes() {
    assert!(Cli::try_parse_from(["monocle", "router", "chat", "--experimental-core-model-only"]).is_err());
}
